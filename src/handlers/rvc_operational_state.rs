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
    Charging,
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
            OperationalState::Charging => OperationalStateEnum::Charging as u8,
        }
    }
}

impl From<device::DeviceState> for OperationalState {
    fn from(status: device::DeviceState) -> Self {
        match status.value {
            device::StatusValue::Error | device::StatusValue::Unknown => OperationalState::Error,
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
        self.ensure_known_status().map_err(to_matter_err)?;
        let state = operational_state(self.current_state.get(), self.battery.get());
        Ok(state.into())
    }

    async fn operational_error<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
        builder: decl::globals::ErrorStateStructBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        self.ensure_known_status().map_err(to_matter_err)?;
        if self.current_state.get().value == device::StatusValue::Error {
            let error = self.robot_error.borrow();
            let details = error
                .as_ref()
                .map(|error| device::truncate_utf8(&error.message, 64));
            let label = error
                .as_ref()
                .filter(|error| !error.vendor_error_code.is_empty())
                .map(|error| device::truncate_utf8(&error.vendor_error_code, 64))
                .unwrap_or("Valetudo error");
            builder
                .error_state_id(2)?
                .error_state_label(Some(label))?
                .error_state_details(details)?
                .end()
        } else {
            build_ok(builder)
        }
    }

    async fn handle_pause<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        ctx: impl rs_matter::dm::InvokeContext,
        response: rvc_operational_state::OperationalCommandResponseBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        let Some(_command) = self.try_command() else {
            return build_response(response, 3);
        };

        let receipt = self.current_state.dataver();
        let state = self.current_state.get();
        let action = if self.cycle_mode.get() == device::CycleMode::Mapping
            && state.value != device::StatusValue::Returning
            && state.value != device::StatusValue::Paused
        {
            // BasicControl does not promise a mapping-specific resume operation.
            CommandAction::Reject
        } else {
            pause_action(state)
        };
        let code = match action {
            CommandAction::Noop => 0,
            CommandAction::Reject => 3,
            CommandAction::Invoke => match self.pause().await {
                Ok(()) => {
                    if self.acknowledge_state(
                        receipt,
                        device::DeviceState {
                            value: device::StatusValue::Paused,
                            flag: device::StatusFlag::Resumable,
                        },
                        &ctx,
                    ) {
                        self.paused_activity.set(Some(state));
                        self.resume_context_trusted.set(true);
                    }
                    0
                }
                Err(error) => {
                    self.invalidate_timing();
                    log::warn!("Pause failed: {error:#}");
                    3
                }
            },
        };
        build_response(response, code)
    }

    async fn handle_resume<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        ctx: impl rs_matter::dm::InvokeContext,
        response: rvc_operational_state::OperationalCommandResponseBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        let Some(_command) = self.try_command() else {
            return build_response(response, 1);
        };

        let receipt = self.current_state.dataver();
        let returning = self
            .paused_activity
            .get()
            .is_some_and(|state| state.value == device::StatusValue::Returning);
        let mapping = self.cycle_mode.get() == device::CycleMode::Mapping;
        let state = self.current_state.get();
        let can_resume = self.resume_context_trusted.get()
            && ((self.cycle_known.get()
                && self.cycle_mode.get() != device::CycleMode::Idle
                && self.cycle_mode.get() != device::CycleMode::Mapping)
                || self
                    .paused_activity
                    .get()
                    .is_some_and(|origin| origin.value == device::StatusValue::Returning));
        let code = match resume_action(state, can_resume) {
            CommandAction::Noop => 0,
            CommandAction::Reject => {
                if state.value == device::StatusValue::Paused {
                    1
                } else {
                    3
                }
            }
            CommandAction::Invoke => match self.resume().await {
                Ok(()) => {
                    self.acknowledge_state(
                        receipt,
                        device::DeviceState {
                            value: if returning {
                                device::StatusValue::Returning
                            } else if mapping {
                                device::StatusValue::Moving
                            } else {
                                device::StatusValue::Cleaning
                            },
                            flag: if mapping {
                                device::StatusFlag::Mapping
                            } else {
                                device::StatusFlag::None
                            },
                        },
                        &ctx,
                    );
                    0
                }
                Err(error) => {
                    self.invalidate_timing();
                    log::warn!("Resume failed: {error:#}");
                    1
                }
            },
        };
        build_response(response, code)
    }

    async fn handle_go_home<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        ctx: impl rs_matter::dm::InvokeContext,
        response: rvc_operational_state::OperationalCommandResponseBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        let Some(_command) = self.try_command() else {
            return build_response(response, 3);
        };

        let receipt = self.current_state.dataver();
        let code = match home_action(self.current_state.get()) {
            CommandAction::Noop => 0,
            CommandAction::Reject => 3,
            CommandAction::Invoke => match self.go_home().await {
                Ok(()) => {
                    self.acknowledge_state(
                        receipt,
                        device::DeviceState {
                            value: device::StatusValue::Returning,
                            flag: device::StatusFlag::None,
                        },
                        &ctx,
                    );
                    0
                }
                Err(error) => {
                    self.invalidate_timing();
                    log::warn!("GoHome failed: {error:#}");
                    3
                }
            },
        };
        build_response(response, code)
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
        OperationalState::Charging => "Charging",
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

fn operational_state(
    state: device::DeviceState,
    battery: device::BatteryState,
) -> OperationalState {
    if state.value == device::StatusValue::Docked && battery.flag == device::BatteryFlag::Charging {
        OperationalState::Charging
    } else {
        state.into()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CommandAction {
    Invoke,
    Noop,
    Reject,
}

fn pause_action(state: device::DeviceState) -> CommandAction {
    use device::StatusValue::*;
    match state.value {
        Paused => CommandAction::Noop,
        Cleaning | Moving | ManualControl | Returning => CommandAction::Invoke,
        _ => CommandAction::Reject,
    }
}

fn resume_action(state: device::DeviceState, cycle_active: bool) -> CommandAction {
    use device::StatusValue::*;
    match state.value {
        Cleaning | Moving | ManualControl => CommandAction::Noop,
        Paused if cycle_active => CommandAction::Invoke,
        Docked if cycle_active && state.flag == device::StatusFlag::Resumable => {
            CommandAction::Invoke
        }
        _ => CommandAction::Reject,
    }
}

fn home_action(state: device::DeviceState) -> CommandAction {
    use device::StatusValue::*;
    match state.value {
        Returning => CommandAction::Noop,
        Idle | Cleaning | Moving | Paused | ManualControl => CommandAction::Invoke,
        _ => CommandAction::Reject,
    }
}

fn build_response<P: TLVBuilderParent>(
    response: rvc_operational_state::OperationalCommandResponseBuilder<P>,
    code: u8,
) -> Result<P, Error> {
    response
        .command_response_state()?
        .error_state_id(code)?
        .error_state_label(None)?
        .error_state_details(None)?
        .end()?
        .end()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(value: device::StatusValue, flag: device::StatusFlag) -> device::DeviceState {
        device::DeviceState { value, flag }
    }
    #[test]
    fn docked_resume_requires_a_resumable_active_cycle() {
        use device::{StatusFlag::*, StatusValue::*};
        assert_eq!(
            resume_action(state(Docked, None), false),
            CommandAction::Reject
        );
        assert_eq!(
            resume_action(state(Docked, Resumable), false),
            CommandAction::Reject
        );
        assert_eq!(
            resume_action(state(Docked, None), true),
            CommandAction::Reject
        );
        assert_eq!(
            resume_action(state(Docked, Resumable), true),
            CommandAction::Invoke
        );
        assert_eq!(
            resume_action(state(Returning, None), true),
            CommandAction::Reject
        );
        assert_eq!(
            resume_action(state(Paused, Resumable), true),
            CommandAction::Invoke
        );
    }
    #[test]
    fn pause_and_home_are_idempotent_and_state_guarded() {
        use device::{StatusFlag::*, StatusValue::*};
        assert_eq!(pause_action(state(Paused, Resumable)), CommandAction::Noop);
        assert_eq!(pause_action(state(Docked, None)), CommandAction::Reject);
        assert_eq!(pause_action(state(Returning, None)), CommandAction::Invoke);
        assert_eq!(home_action(state(Returning, None)), CommandAction::Noop);
        assert_eq!(home_action(state(Docked, None)), CommandAction::Reject);
    }
    #[test]
    fn docked_battery_state_distinguishes_charging() {
        let docked = state(device::StatusValue::Docked, device::StatusFlag::None);
        assert_eq!(
            operational_state(
                docked,
                device::BatteryState::from_api(Some(25.0), device::BatteryFlag::Charging)
            ),
            OperationalState::Charging
        );
        assert_eq!(
            operational_state(
                docked,
                device::BatteryState::from_api(Some(100.0), device::BatteryFlag::Charged)
            ),
            OperationalState::Docked
        );
        assert_eq!(
            operational_state(docked, device::BatteryState::default()),
            OperationalState::Docked
        );
    }
}
