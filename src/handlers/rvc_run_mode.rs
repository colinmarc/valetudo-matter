use enum_iterator::Sequence;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use rs_matter::{
    dm::ArrayAttributeRead,
    error::{Error, ErrorCode},
    tlv::TLVBuilderParent,
};

use rs_matter::dm::clusters::decl::{self as decl, rvc_run_mode};

use crate::{
    device::{Capability, CycleMode, Device},
    handlers::to_matter_err,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive, Sequence)]
#[repr(u8)]
enum RunMode {
    Idle,
    Cleaning,
    Mapping,
}

impl rvc_run_mode::ClusterAsyncHandler for Device {
    const CLUSTER: rs_matter::dm::Cluster<'static> = rvc_run_mode::FULL_CLUSTER
        .with_revision(4)
        .with_features(0) // A new cycle cannot replace an active cycle.
        .with_attrs(rs_matter::with!(required));

    fn dataver(&self) -> u32 {
        self.cycle_mode.dataver()
    }

    fn dataver_changed(&self) {
        self.cycle_mode.dataver_changed();
    }

    async fn supported_modes<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
        builder: ArrayAttributeRead<
            decl::globals::ModeOptionStructArrayBuilder<P>,
            decl::globals::ModeOptionStructBuilder<P>,
        >,
    ) -> Result<P, rs_matter::error::Error> {
        match builder {
            ArrayAttributeRead::ReadAll(mut builder) => {
                for mode in supported_modes(
                    self.capabilities
                        .contains(Capability::MappingPassCapability),
                ) {
                    builder = build_mode(builder.push()?, mode)?;
                }

                builder.end()
            }
            ArrayAttributeRead::ReadOne(index, builder) => {
                let mode = supported_modes(
                    self.capabilities
                        .contains(Capability::MappingPassCapability),
                )
                .into_iter()
                .nth(index as usize)
                .ok_or(Error::new(ErrorCode::ConstraintError))?;

                build_mode(builder, mode)
            }
            ArrayAttributeRead::ReadNone(builder) => builder.end(),
        }
    }

    async fn current_mode(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
    ) -> Result<u8, rs_matter::error::Error> {
        self.ensure_known_status().map_err(to_matter_err)?;
        if !self.cycle_known.get() {
            return Err(ErrorCode::InvalidAction.into());
        }
        Ok(self.cycle_mode.get() as u8)
    }

    async fn handle_change_to_mode<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        ctx: impl rs_matter::dm::InvokeContext,
        request: rvc_run_mode::ChangeToModeRequest<'_>,
        response: rvc_run_mode::ChangeToModeResponseBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        let received_at = std::time::Instant::now();
        let Some(_command) = self.try_command() else {
            return response
                .status(2)?
                .status_text(Some("Another control command is in progress"))?
                .end();
        };

        let new_mode = RunMode::try_from(request.new_mode()?).ok();
        let Some(new_mode) = new_mode.filter(|mode| {
            *mode != RunMode::Mapping
                || self
                    .capabilities
                    .contains(Capability::MappingPassCapability)
        }) else {
            return response.status(1)?.status_text(Some(""))?.end();
        };
        if self.ensure_known_status().is_err() {
            return response
                .status(2)?
                .status_text(Some("Robot status unavailable"))?
                .end();
        }
        let desired = match new_mode {
            RunMode::Idle => CycleMode::Idle,
            RunMode::Cleaning => CycleMode::Cleaning,
            RunMode::Mapping => CycleMode::Mapping,
        };
        if !self.cycle_known.get() && desired != CycleMode::Idle {
            return response
                .status(3)?
                .status_text(Some(
                    "Stop the unknown operation before starting a new cycle",
                ))?
                .end();
        }
        if desired != self.cycle_mode.get() || !self.cycle_known.get() {
            if self.cycle_active() && desired != CycleMode::Idle {
                return response
                    .status(3)?
                    .status_text(Some("Stop the active cycle before starting another mode"))?
                    .end();
            }
            if desired != CycleMode::Idle {
                self.begin_timing(desired, received_at);
            }
            let start_receipt = self.start_receipt();
            let result = match new_mode {
                RunMode::Idle => self.stop().await,
                RunMode::Cleaning => self.start_cleaning().await,
                RunMode::Mapping => self.start_mapping().await,
            };
            if let Err(error) = result {
                self.invalidate_timing();
                log::warn!("Run mode command failed: {error:#}");
                return response
                    .status(2)?
                    .status_text(Some("Valetudo rejected or did not confirm command"))?
                    .end();
            }
            if desired != CycleMode::Idle {
                if !self.acknowledge_start(desired, start_receipt, &ctx) {
                    return response
                        .status(2)?
                        .status_text(Some(
                            "Robot observations were interrupted; command outcome is uncertain",
                        ))?
                        .end();
                }
            } else {
                self.set_cycle_mode(desired, &ctx);
            }
        }

        response.status(0)?.status_text(None)?.end()
    }
}

fn build_mode<P: TLVBuilderParent>(
    builder: decl::globals::ModeOptionStructBuilder<P>,
    mode: RunMode,
) -> Result<P, Error> {
    let (label, tag) = match mode {
        RunMode::Idle => ("Idle", rvc_run_mode::ModeTag::Idle),
        RunMode::Cleaning => ("Cleaning", rvc_run_mode::ModeTag::Cleaning),
        RunMode::Mapping => ("Mapping", rvc_run_mode::ModeTag::Mapping),
    };

    builder
        .label(label)?
        .mode(mode.into())?
        .mode_tags()?
        .push()?
        .mfg_code(None)?
        .value(tag as _)?
        .end()?
        .end()?
        .end()
}

fn supported_modes(mapping: bool) -> Vec<RunMode> {
    let mut modes = vec![RunMode::Idle, RunMode::Cleaning];
    if mapping {
        modes.push(RunMode::Mapping);
    }
    modes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mapping_is_advertised_only_with_capability_and_stable_ids() {
        assert_eq!(
            supported_modes(false),
            vec![RunMode::Idle, RunMode::Cleaning]
        );
        assert_eq!(
            supported_modes(true),
            vec![RunMode::Idle, RunMode::Cleaning, RunMode::Mapping]
        );
        assert_eq!(u8::from(RunMode::Mapping), 2);
    }
}
