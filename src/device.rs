use std::{cell::RefCell, time};

use anyhow::{Context as _, bail};
use enumset::EnumSet;
use log::{debug, error};
use rand::thread_rng;
use rs_matter::dm::AttrChangeNotifier;
use smol::{Timer, stream::StreamExt};

mod capabilities;
mod state;

pub(crate) use capabilities::*;
pub(crate) use state::*;

use crate::{handlers::VersionedCell, http::ValetudoClient};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeviceState {
    pub(crate) value: StatusValue,
    pub(crate) flag: StatusFlag,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self {
            value: StatusValue::Idle,
            flag: StatusFlag::None,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct MapSegment {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: Option<String>,
}

pub(crate) struct Device {
    pub(crate) client: ValetudoClient,
    pub(crate) capabilities: EnumSet<Capability>,
    pub(crate) cleaning_presets: EnumSet<Preset>,
    pub(crate) current_preset: VersionedCell<Preset>,
    pub(crate) current_state: VersionedCell<DeviceState>,
    pub(crate) identify_time: VersionedCell<u16>,
    pub(crate) segments: Vec<MapSegment>,
    pub(crate) selected_areas: RefCell<Vec<u32>>,
}

impl Device {
    pub(crate) async fn init(client: ValetudoClient) -> anyhow::Result<Self> {
        debug!("fetching capabilities");
        let capabilities: EnumSet<Capability> = client
            .get("/api/v2/robot/capabilities")
            .await
            .context("Failed to fetch capabilities")?;

        debug!("capabilities: {capabilities:?}");
        if !capabilities.contains(Capability::BasicControlCapability) {
            bail!("Device requires at least BasicControlCapability");
        }

        let cleaning_presets: EnumSet<Preset> =
            if capabilities.contains(Capability::OperationModeControlCapability) {
                debug!("fetching presets");
                let presets = client
                    .get("/api/v2/robot/capabilities/OperationModeControlCapability/presets")
                    .await
                    .context("Failed to fetch cleaning mode presets")?;

                debug!("available presets: {presets:?}");
                presets
            } else {
                EnumSet::empty()
            };

        debug!("fetching status");
        let attributes: Vec<state::StateAttribute> = client
            .get("/api/v2/robot/state/attributes")
            .await
            .context("Failed to fetch initial status")?;
        let Some(state::StateAttribute::StatusStateAttribute { value, flag }) = attributes
            .iter()
            .find(|attr| matches!(attr, state::StateAttribute::StatusStateAttribute { .. }))
        else {
            bail!("No status attribute in api response");
        };

        debug!("current state: {value:?}/{flag:?}");
        let current_state = VersionedCell::new(DeviceState { value: *value, flag: *flag }, &mut thread_rng());

        let current_preset_value = attributes
            .iter()
            .find_map(|attr| match attr {
                state::StateAttribute::PresetSelectionStateAttribute { r#type, value }
                    if r#type == "operation_mode" => Some(value.as_str()),
                _ => None,
            });

        let segments: Vec<MapSegment> =
            if capabilities.contains(Capability::MapSegmentationCapability) {
                debug!("fetching segments");
                let segs = client
                    .get("/api/v2/robot/capabilities/MapSegmentationCapability")
                    .await
                    .context("Failed to fetch map segments")?;

                debug!("available segments: {segs:?}");
                segs
            } else {
                Vec::new()
            };

        let default_preset = current_preset_value
            .and_then(preset_from_str)
            .or_else(|| cleaning_presets.iter().next())
            .unwrap_or(Preset::Vacuum);
        debug!("current preset: {default_preset:?}");

        Ok(Self {
            client: client.clone(),
            capabilities,
            cleaning_presets,
            current_preset: VersionedCell::new(default_preset, &mut thread_rng()),
            current_state,
            identify_time: VersionedCell::new(0, &mut thread_rng()),
            segments,
            selected_areas: RefCell::new(Vec::new()),
        })
    }

    /// A background worker updating the device state in response to SSE
    /// events. Notifies Matter subscriptions when state changes.
    pub(crate) async fn monitor_status(
        &self,
        notify: &impl AttrChangeNotifier,
        run_mode_cluster: u32,
        clean_mode_cluster: u32,
        operational_state_cluster: u32,
    ) {
        loop {
            match self
                .monitor_status_once(notify, run_mode_cluster, clean_mode_cluster, operational_state_cluster)
                .await
                .context("Stream error")
            {
                Ok(_) => debug!("GET /api/v2/robot/state/attributes/sse: stream exited"),
                Err(e) => error!("GET /api/v2/robot/state/attributes/sse: {e:#}"),
            }

            Timer::after(time::Duration::from_secs(1)).await;
        }
    }

    async fn monitor_status_once(
        &self,
        notify: &impl AttrChangeNotifier,
        run_mode_cluster: u32,
        clean_mode_cluster: u32,
        operational_state_cluster: u32,
    ) -> anyhow::Result<()> {
        // Poll the state once before streaming. This helps fix issues if the
        // SSE stream dies.
        let attributes: Vec<state::StateAttribute> = self
            .client
            .get("/api/v2/robot/state/attributes")
            .await
            .context("Failed to fetch initial status")?;
        self.apply_attributes(&attributes, notify, run_mode_cluster, clean_mode_cluster, operational_state_cluster);

        let mut stream = self.client.sse("/api/v2/robot/state/attributes/sse").await?;
        while let Some(s) = stream.next().await {
            let ev: Vec<state::StateAttribute> =
                serde_json::from_str(&s?).context("Invalid event")?;

            self.apply_attributes(&ev, notify, run_mode_cluster, clean_mode_cluster, operational_state_cluster);
        }

        Ok(())
    }

    fn apply_attributes(
        &self,
        attrs: &[state::StateAttribute],
        notify: &impl AttrChangeNotifier,
        run_mode_cluster: u32,
        clean_mode_cluster: u32,
        operational_state_cluster: u32,
    ) {
        for attr in attrs {
            match attr {
                state::StateAttribute::StatusStateAttribute { value, flag } => {
                    let new_state = DeviceState { value: *value, flag: *flag };
                    debug!("setting state: {:?}/{:?}", value, flag);
                    let old = self.current_state.get();
                    self.current_state.set(new_state);
                    if new_state != old {
                        notify.notify_cluster_changed(1, run_mode_cluster);
                        notify.notify_cluster_changed(1, operational_state_cluster);
                    }
                }
                state::StateAttribute::PresetSelectionStateAttribute { r#type, value }
                    if r#type == "operation_mode" =>
                {
                    if let Some(preset) = preset_from_str(value) {
                        debug!("setting preset: {preset:?}");
                        let old = self.current_preset.get();
                        self.current_preset.set(preset);
                        if preset != old {
                            notify.notify_cluster_changed(1, clean_mode_cluster);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) async fn start_cleaning(&self) -> anyhow::Result<()> {
        let selected = self.selected_areas.borrow().clone();
        if selected.is_empty() {
            debug!("starting full clean");
            self.client
                .put(
                    "/api/v2/robot/capabilities/BasicControlCapability",
                    r#"{"action": "start"}"#.to_owned(),
                )
                .await
        } else {
            let segment_ids: Vec<String> = selected
                .iter()
                .filter_map(|area_id| {
                    self.segments
                        .iter()
                        .find(|s| segment_area_id(s) == Some(*area_id))
                        .map(|s| format!(r#""{}""#, s.id))
                })
                .collect();

            debug!("starting segment clean for segments: {segment_ids:?}");
            let body = format!(
                r#"{{"action": "start_segment_action", "segment_ids": [{}], "iterations": 1}}"#,
                segment_ids.join(", "),
            );
            self.client
                .put("/api/v2/robot/capabilities/MapSegmentationCapability", body)
                .await
        }
    }

    pub(crate) async fn start_mapping_pass(&self) -> anyhow::Result<()> {
        todo!()
    }

    pub(crate) async fn pause(&self) -> anyhow::Result<()> {
        debug!("starting pause command to robot");
        self.client
            .put(
                "/api/v2/robot/capabilities/BasicControlCapability",
                r#"{"action": "pause"}"#.to_owned(),
            )
            .await
    }

    pub(crate) async fn go_home(&self) -> anyhow::Result<()> {
        debug!("starting home command to robot");
        self.client
            .put(
                "/api/v2/robot/capabilities/BasicControlCapability",
                r#"{"action": "home"}"#.to_owned(),
            )
            .await
    }

    pub(crate) async fn set_preset(&self, preset: Preset) -> anyhow::Result<()> {
        let name = match preset {
            Preset::Vacuum => "vacuum",
            Preset::Mop => "mop",
            Preset::VacuumAndMop => "vacuum_and_mop",
            Preset::VacuumThenMop => "vacuum_then_mop",
        };

        debug!("setting operation mode preset to {name}");
        self.client
            .put(
                "/api/v2/robot/capabilities/OperationModeControlCapability/preset",
                format!(r#"{{"name": "{name}"}}"#),
            )
            .await
    }

    pub(crate) async fn stop(&self) -> anyhow::Result<()> {
        debug!("starting stop command to robot");
        self.client
            .put(
                "/api/v2/robot/capabilities/BasicControlCapability",
                r#"{"action": "stop"}"#.to_owned(),
            )
            .await
    }
}

/// Map a Valetudo segment to a Matter area ID by parsing the string ID.
pub(crate) fn segment_area_id(segment: &MapSegment) -> Option<u32> {
    segment.id.parse().ok()
}

fn preset_from_str(s: &str) -> Option<Preset> {
    match s {
        "vacuum" => Some(Preset::Vacuum),
        "mop" => Some(Preset::Mop),
        "vacuum_and_mop" => Some(Preset::VacuumAndMop),
        "vacuum_then_mop" => Some(Preset::VacuumThenMop),
        _ => None,
    }
}
