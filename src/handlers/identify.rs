use log::{info, warn};
use rs_matter::{
    dm::OperationContext,
    error::{Error, ErrorCode},
};

use crate::{device::Capability, device::Device, generated::identify};

impl Device {
    async fn identify_robot(&self, ctx: &impl OperationContext, dur: u16) -> Result<(), Error> {
        self.identify_deadline.set(
            (dur != 0)
                .then(|| std::time::Instant::now() + std::time::Duration::from_secs(dur as u64)),
        );
        self.identify_time.set(dur);
        // Explicit writes/commands are reportable even when their value is the
        // same: a new request restarts the deadline.
        ctx.notify_own_cluster_changed();
        // Coalesce bursts; the worker reads the latest deadline, not old commands.
        let _ = self.identify_wake.0.try_send(());
        // The independent worker performs the presentation. Command handling
        // must not wait for the full duration or prevent a later cancellation.
        Ok(())
    }

    pub(crate) fn tick_identify(&self, notify: &impl rs_matter::dm::AttrChangeNotifier) {
        let remaining = remaining_seconds(self.identify_deadline.get(), std::time::Instant::now());
        let previous = self.identify_time.get();
        self.identify_time.set(remaining);
        if previous != 0 && remaining == 0 {
            self.identify_deadline.set(None);
            notify.notify_cluster_changed(1, identify::FULL_CLUSTER.id);
        }
    }

    pub(crate) async fn present_identify_once(&self) -> bool {
        // A pending notification describes the deadline already being read here.
        while self.identify_wake.1.try_recv().is_ok() {}
        let Some(deadline) = self
            .identify_deadline
            .get()
            .filter(|deadline| *deadline > std::time::Instant::now())
        else {
            return false;
        };
        if let Some((path, body)) = identification_request(self.capabilities) {
            info!("playing identification sound");
            // Stop waiting/dispatching on cancellation, replacement or expiry.
            // Dropping HTTP cannot retract an announcement already accepted by
            // the robot: the public API has no playback-stop command.
            return smol::future::or(
                async {
                    if let Err(error) = self.client.put(path, body.to_owned()).await {
                        warn!("Identification sound failed: {error:#}");
                    }
                    false
                },
                async {
                    smol::future::or(
                        async {
                            let _ = self.identify_wake.1.recv().await;
                        },
                        async {
                            smol::Timer::at(deadline).await;
                        },
                    )
                    .await;
                    true
                },
            )
            .await;
        }
        false
    }

    pub(crate) async fn monitor_identify(&self, notify: &impl rs_matter::dm::AttrChangeNotifier) {
        let countdown = async {
            loop {
                self.tick_identify(notify);
                smol::Timer::after(std::time::Duration::from_secs(1)).await;
            }
        };
        let presentation = async {
            loop {
                if self.present_identify_once().await {
                    continue;
                }
                let active =
                    remaining_seconds(self.identify_deadline.get(), std::time::Instant::now()) != 0;
                smol::future::or(
                    async {
                        let _ = self.identify_wake.1.recv().await;
                    },
                    async {
                        if active {
                            // Repeat interval, not an asserted sound duration.
                            smol::Timer::after(std::time::Duration::from_secs(5)).await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    },
                )
                .await;
            }
        };
        smol::future::zip(countdown, presentation).await;
    }
}

fn identification_request(
    capabilities: enumset::EnumSet<Capability>,
) -> Option<(&'static str, &'static str)> {
    if capabilities.contains(Capability::LocateCapability) {
        Some((
            "/api/v2/robot/capabilities/LocateCapability",
            r#"{"action":"locate"}"#,
        ))
    } else if capabilities.contains(Capability::SpeakerTestCapability) {
        Some((
            "/api/v2/robot/capabilities/SpeakerTestCapability",
            r#"{"action":"play_test_sound"}"#,
        ))
    } else {
        None
    }
}

fn remaining_seconds(deadline: Option<std::time::Instant>, now: std::time::Instant) -> u16 {
    deadline
        .map(|deadline| deadline.saturating_duration_since(now).as_secs_f64().ceil() as u16)
        .unwrap_or(0)
}

impl identify::ClusterAsyncHandler for Device {
    const CLUSTER: rs_matter::dm::Cluster<'static> = identify::FULL_CLUSTER
        .with_revision(6)
        .with_cmds(rs_matter::with!(identify::CommandId::Identify))
        .with_attrs(rs_matter::with!(required));

    fn dataver(&self) -> u32 {
        self.identify_time.dataver()
    }

    fn dataver_changed(&self) {
        self.identify_time.dataver_changed();
    }

    async fn identify_time(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
    ) -> Result<u16, rs_matter::error::Error> {
        Ok(remaining_seconds(
            self.identify_deadline.get(),
            std::time::Instant::now(),
        ))
    }

    async fn identify_type(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
    ) -> Result<identify::IdentifyTypeEnum, rs_matter::error::Error> {
        Ok(if identification_request(self.capabilities).is_some() {
            identify::IdentifyTypeEnum::AudibleBeep
        } else {
            identify::IdentifyTypeEnum::None
        })
    }

    async fn set_identify_time(
        &self,
        ctx: impl rs_matter::dm::WriteContext,
        value: u16,
    ) -> Result<(), rs_matter::error::Error> {
        self.identify_robot(&ctx, value).await
    }

    async fn handle_identify(
        &self,
        ctx: impl rs_matter::dm::InvokeContext,
        request: identify::IdentifyRequest<'_>,
    ) -> Result<(), rs_matter::error::Error> {
        self.identify_robot(&ctx, request.identify_time()?).await
    }

    async fn handle_trigger_effect(
        &self,
        _ctx: impl rs_matter::dm::InvokeContext,
        _request: identify::TriggerEffectRequest<'_>,
    ) -> Result<(), rs_matter::error::Error> {
        Err(ErrorCode::InvalidCommand.into()) // Unimplemented.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_uses_only_real_capabilities() {
        assert!(identification_request(enumset::EnumSet::new()).is_none());
        let both = Capability::LocateCapability | Capability::SpeakerTestCapability;
        assert_eq!(
            identification_request(both).unwrap().0,
            "/api/v2/robot/capabilities/LocateCapability"
        );
        let cluster = <Device as identify::ClusterAsyncHandler>::CLUSTER;
        let ids: Vec<_> = cluster
            .commands
            .iter()
            .filter(|cmd| (cluster.with_cmds)(cmd, cluster.revision, cluster.feature_map))
            .map(|cmd| cmd.id)
            .collect();
        assert_eq!(ids, vec![identify::CommandId::Identify as u32]);
    }
    #[test]
    fn duration_expires_and_can_be_cancelled() {
        let start = std::time::Instant::now();
        let deadline = Some(start + std::time::Duration::from_secs(3));
        assert_eq!(remaining_seconds(deadline, start), 3);
        assert_eq!(
            remaining_seconds(deadline, start + std::time::Duration::from_secs(1)),
            2
        );
        assert_eq!(
            remaining_seconds(deadline, start + std::time::Duration::from_secs(4)),
            0
        );
        assert_eq!(remaining_seconds(None, start), 0);
    }
}

#[cfg(test)]
#[path = "identify_monitor_tests.rs"]
mod monitor_tests;
