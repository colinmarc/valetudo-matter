use enum_iterator::Sequence;

use rs_matter::{
    dm::ArrayAttributeRead,
    error::{Error, ErrorCode},
    tlv::{Nullable, NullableBuilder, TLVBuilderParent, Utf8StrArrayBuilder, Utf8StrBuilder},
};

use rs_matter::dm::clusters::decl::{self as decl, rvc_operational_state};

use crate::{
    device::{self, Device},
    handlers::to_matter_err,
};

use rvc_operational_state::OperationalStateEnum;

// Wraps OperationalStateEnum to add conversions from Valetudo state.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Sequence)]
enum OperationalState {
    Stopped,
    Running,
    Paused,
    Error,
    SeekingCharger,
    Docked,
}

impl From<OperationalState> for u8 {
    fn from(state: OperationalState) -> u8 {
        match state {
            OperationalState::Stopped => OperationalStateEnum::Stopped as u8,
            OperationalState::Running => OperationalStateEnum::Running as u8,
            OperationalState::Paused => OperationalStateEnum::Paused as u8,
            OperationalState::Error => OperationalStateEnum::VError as u8,
            OperationalState::SeekingCharger => OperationalStateEnum::SeekingCharger as u8,
            OperationalState::Docked => OperationalStateEnum::Docked as u8,
        }
    }
}

impl From<device::DeviceState> for OperationalState {
    fn from(status: device::DeviceState) -> Self {
        match status.value {
            device::StatusValue::Error => OperationalState::Error,
            device::StatusValue::Docked => OperationalState::Docked,
            device::StatusValue::Idle => OperationalState::Stopped,
            device::StatusValue::Returning => OperationalState::SeekingCharger,
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
            decl::globals::OperationalStateStructArrayBuilder<P>,
            decl::globals::OperationalStateStructBuilder<P>,
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
        builder: decl::globals::ErrorStateStructBuilder<P>,
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
    builder: decl::globals::OperationalStateStructBuilder<P>,
    state: OperationalState,
) -> Result<P, Error> {
    let label = match state {
        OperationalState::Stopped => "Stopped",
        OperationalState::Running => "Running",
        OperationalState::Paused => "Paused",
        OperationalState::Error => "Error",
        OperationalState::SeekingCharger => "Returning to Dock",
        OperationalState::Docked => "Docked",
    };

    builder
        .operational_state_id(state.into())?
        .operational_state_label(Some(label))?
        .end()
}

fn build_ok<P: TLVBuilderParent>(
    builder: decl::globals::ErrorStateStructBuilder<P>,
) -> Result<P, Error> {
    builder
        .error_state_id(0x0)?
        .error_state_label(None)?
        .error_state_details(None)?
        .end()
}
