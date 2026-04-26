use enum_iterator::Sequence;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use rs_matter::{
    dm::ArrayAttributeRead,
    error::{Error, ErrorCode},
    tlv::{Nullable, NullableBuilder, TLVBuilderParent, Utf8StrArrayBuilder, Utf8StrBuilder},
};

use crate::{
    device::{self, Device},
    generated::rvc_operational_state,
    handlers::to_matter_err,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive, IntoPrimitive, Sequence)]
#[repr(u8)]
enum OperationalState {
    Error,
    Running,
    Paused,
    Returning,
    Docked,
}

impl From<device::DeviceState> for OperationalState {
    fn from(status: device::DeviceState) -> Self {
        match status.value {
            device::StatusValue::Error => OperationalState::Error,
            device::StatusValue::Docked => OperationalState::Docked,
            device::StatusValue::Idle => OperationalState::Running,
            device::StatusValue::Returning => OperationalState::Returning,
            device::StatusValue::Cleaning => OperationalState::Running,
            device::StatusValue::Paused => OperationalState::Paused,
            device::StatusValue::ManualControl => OperationalState::Running,
            device::StatusValue::Moving => OperationalState::Running,
        }
    }
}

impl rvc_operational_state::ClusterAsyncHandler for Device {
    const CLUSTER: rs_matter::dm::Cluster<'static> = rvc_operational_state::FULL_CLUSTER
        .with_revision(3)
        .with_attrs(rs_matter::with!(required));

    fn dataver(&self) -> u32 {
        self.current_state.dataver()
    }

    fn dataver_changed(&self) {
        self.current_state.dataver_changed();
    }

    async fn phase_list<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
        builder: ArrayAttributeRead<NullableBuilder<P, Utf8StrArrayBuilder<P>>, Utf8StrBuilder<P>>,
    ) -> Result<P, rs_matter::error::Error> {
        match builder {
            ArrayAttributeRead::ReadAll(builder) => builder.null(),
            ArrayAttributeRead::ReadOne(_, _) => Err(Error::new(ErrorCode::ConstraintError)),
            ArrayAttributeRead::ReadNone(builder) => builder.null(),
        }
    }

    async fn current_phase(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
    ) -> Result<Nullable<u8>, rs_matter::error::Error> {
        Ok(Nullable::none())
    }

    async fn operational_state_list<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
        builder: rs_matter::dm::ArrayAttributeRead<
            rvc_operational_state::OperationalStateStructArrayBuilder<P>,
            rvc_operational_state::OperationalStateStructBuilder<P>,
        >,
    ) -> Result<P, rs_matter::error::Error> {
        match builder {
            ArrayAttributeRead::ReadAll(mut builder) => {
                for v in enum_iterator::all::<OperationalState>() {
                    builder = build_state(builder.push()?, v)?;
                }

                builder.end()
            }
            ArrayAttributeRead::ReadOne(index, builder) => {
                let state = enum_iterator::all::<OperationalState>()
                    .nth(index as usize)
                    .ok_or(Error::new(ErrorCode::ConstraintError))?;

                build_state(builder, state)
            }
            ArrayAttributeRead::ReadNone(builder) => builder.end(),
        }
    }

    async fn operational_state(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
    ) -> Result<u8, rs_matter::error::Error> {
        let state: OperationalState = self.current_state.get().into();
        Ok(state.into())
    }

    async fn operational_error<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
        builder: rvc_operational_state::ErrorStateStructBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        // todo
        build_ok(builder)
    }

    async fn handle_pause<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::InvokeContext,
        response: rvc_operational_state::OperationalCommandResponseBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        self.pause().await.map_err(to_matter_err)?;
        build_ok(response.command_response_state()?)?.end()
    }

    async fn handle_resume<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::InvokeContext,
        response: rvc_operational_state::OperationalCommandResponseBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        self.start_cleaning().await.map_err(to_matter_err)?;
        build_ok(response.command_response_state()?)?.end()
    }

    async fn handle_go_home<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::InvokeContext,
        response: rvc_operational_state::OperationalCommandResponseBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        self.go_home().await.map_err(to_matter_err)?;
        build_ok(response.command_response_state()?)?.end()
    }
}

fn build_state<P: TLVBuilderParent>(
    builder: rvc_operational_state::OperationalStateStructBuilder<P>,
    state: OperationalState,
) -> Result<P, Error> {
    // Unclear why Error and Running aren't generating as part of the enum.
    let (id, label) = match state {
        OperationalState::Error => (0x03, Some("Error")),
        OperationalState::Running => (0x01, Some("Running")),
        OperationalState::Paused => (0x02, Some("Paused")),
        OperationalState::Returning => (
            rvc_operational_state::OperationalStateEnum::SeekingCharger as _,
            Some("Returning to Dock"),
        ),
        OperationalState::Docked => (
            rvc_operational_state::OperationalStateEnum::Docked as _,
            Some("Docked"),
        ),
    };

    builder
        .operational_state_id(id)?
        .operational_state_label(label)?
        .end()
}

fn build_ok<P: TLVBuilderParent>(
    builder: rvc_operational_state::ErrorStateStructBuilder<P>,
) -> Result<P, Error> {
    builder
        .error_state_id(0x0)?
        .error_state_label(None)?
        .error_state_details(None)?
        .end()
}
