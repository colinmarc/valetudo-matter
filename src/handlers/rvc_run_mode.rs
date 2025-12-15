use enum_iterator::Sequence;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use rs_matter::{
    dm::ArrayAttributeRead,
    error::{Error, ErrorCode},
    tlv::TLVBuilderParent,
};

use crate::{
    device::{Capability, Device, DeviceState, StatusFlag, StatusValue},
    generated::rvc_run_mode,
    handlers::to_matter_err,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive, Sequence)]
#[repr(u8)]
enum RunMode {
    Idle,
    Cleaning,
    Mapping,
}

impl From<DeviceState> for RunMode {
    fn from(status: DeviceState) -> Self {
        match (status.value, status.flag) {
            (StatusValue::Error, _) => RunMode::Idle,
            (StatusValue::Docked, _) => RunMode::Idle,
            (StatusValue::Idle, _) => RunMode::Idle,
            (StatusValue::Returning, _) => RunMode::Idle,
            (StatusValue::Cleaning, _) => RunMode::Cleaning,
            (StatusValue::Paused, _) => RunMode::Idle,
            (StatusValue::ManualControl, _) => RunMode::Cleaning,
            // todo verify
            (StatusValue::Moving, StatusFlag::Mapping) => RunMode::Mapping,
            (StatusValue::Moving, _) => RunMode::Cleaning,
        }
    }
}

impl Device {
    fn supported_modes(&self) -> impl Iterator<Item = RunMode> {
        enum_iterator::all::<RunMode>().take_while(|mode| {
            *mode != RunMode::Mapping
                || self
                    .capabilities
                    .contains(Capability::MappingPassCapability)
        })
    }
}

impl rvc_run_mode::ClusterAsyncHandler for Device {
    const CLUSTER: rs_matter::dm::Cluster<'static> = rvc_run_mode::FULL_CLUSTER
        .with_revision(4)
        .with_features(20) // DIRECTMODECH, not currently included in rs-matter
        .with_attrs(rs_matter::with!(required));

    fn dataver(&self) -> u32 {
        self.current_state.dataver()
    }

    fn dataver_changed(&self) {
        self.current_state.dataver_changed();
    }

    async fn supported_modes<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
        builder: ArrayAttributeRead<
            rvc_run_mode::ModeOptionStructArrayBuilder<P>,
            rvc_run_mode::ModeOptionStructBuilder<P>,
        >,
    ) -> Result<P, rs_matter::error::Error> {
        match builder {
            ArrayAttributeRead::ReadAll(mut builder) => {
                for mode in enum_iterator::all::<RunMode>() {
                    builder = build_mode(builder.push()?, mode)?;
                }

                builder.end()
            }
            ArrayAttributeRead::ReadOne(index, builder) => {
                let mode = self
                    .supported_modes()
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
        let mode: RunMode = self.current_state.get().into();
        Ok(mode.into())
    }

    async fn handle_change_to_mode<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::InvokeContext,
        request: rvc_run_mode::ChangeToModeRequest<'_>,
        response: rvc_run_mode::ChangeToModeResponseBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        let Some(new_mode) = request.new_mode()?.try_into().ok() else {
            return Err(Error::new(ErrorCode::ConstraintError));
        };

        let current_mode: RunMode = self.current_state.get().into();
        if new_mode != current_mode {
            match new_mode {
                RunMode::Idle => self.stop().await,
                RunMode::Cleaning => self.start_cleaning().await,
                RunMode::Mapping => self.start_mapping_pass().await,
            }
            .map_err(to_matter_err)?;
        }

        response.status(0)?.status_text(None)?.end()
    }
}

fn build_mode<P: TLVBuilderParent>(
    builder: rvc_run_mode::ModeOptionStructBuilder<P>,
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
