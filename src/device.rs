use std::time;

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

pub(crate) struct Device {
    pub(crate) client: ValetudoClient,
    pub(crate) capabilities: EnumSet<Capability>,
    pub(crate) cleaning_presets: EnumSet<Preset>,
    pub(crate) current_preset: VersionedCell<Preset>,
    pub(crate) current_state: VersionedCell<DeviceState>,
    pub(crate) identify_time: VersionedCell<u16>,
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
            .into_iter()
            .find(|attr| matches!(attr, state::StateAttribute::StatusStateAttribute { .. }))
        else {
            bail!("No status attribute in api response");
        };

        debug!("current state: {value:?}/{flag:?}");
        let current_state = VersionedCell::new(DeviceState { value, flag }, &mut thread_rng());

        let default_preset = cleaning_presets
            .iter()
            .next()
            .unwrap_or(Preset::Vacuum);

        Ok(Self {
            client: client.clone(),
            capabilities,
            cleaning_presets,
            current_preset: VersionedCell::new(default_preset, &mut thread_rng()),
            current_state,
            identify_time: VersionedCell::new(0, &mut thread_rng()),
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
        let Some(state::StateAttribute::StatusStateAttribute { value, flag }) = attributes
            .into_iter()
            .find(|attr| matches!(attr, state::StateAttribute::StatusStateAttribute { .. }))
        else {
            bail!("No status attribute in api response");
        };

        self.update_state(DeviceState { value, flag }, notify, run_mode_cluster, clean_mode_cluster, operational_state_cluster);

        let mut stream = self.client.sse("/api/v2/robot/state/attributes/sse").await?;
        while let Some(s) = stream.next().await {
            let ev: Vec<state::StateAttribute> =
                serde_json::from_str(&s?).context("Invalid event")?;
            let Some(state::StateAttribute::StatusStateAttribute { value, flag }) = ev
                .into_iter()
                .find(|attr| matches!(attr, state::StateAttribute::StatusStateAttribute { .. }))
            else {
                bail!("Invalid event");
            };

            self.update_state(DeviceState { value, flag }, notify, run_mode_cluster, clean_mode_cluster, operational_state_cluster);
        }

        Ok(())
    }

    fn update_state(
        &self,
        state: DeviceState,
        notify: &impl AttrChangeNotifier,
        run_mode_cluster: u32,
        clean_mode_cluster: u32,
        operational_state_cluster: u32,
    ) {
        debug!("setting state: {:?}/{:?}", state.value, state.flag);
        let old = self.current_state.get();
        self.current_state.set(state);
        if state != old {
            notify.notify_cluster_changed(1, run_mode_cluster);
            notify.notify_cluster_changed(1, clean_mode_cluster);
            notify.notify_cluster_changed(1, operational_state_cluster);
        }
    }

    pub(crate) async fn start_cleaning(&self) -> anyhow::Result<()> {
        debug!("starting start command to robot");
        self.client
            .put(
                "/api/v2/robot/capabilities/BasicControlCapability",
                r#"{"action": "start"}"#.to_owned(),
            )
            .await
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

    pub(crate) async fn stop(&self) -> Result<(), anyhow::Error> {
        debug!("starting stop command to robot");
        self.client
            .put(
                "/api/v2/robot/capabilities/BasicControlCapability",
                r#"{"action": "stop"}"#.to_owned(),
            )
            .await
    }
}
