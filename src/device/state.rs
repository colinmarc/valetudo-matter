use enumset::EnumSetType;
use serde::{Deserialize, Serialize};

// Not in the openapi spec for some reason.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "__class")]
#[allow(clippy::enum_variant_names)]
pub(crate) enum StateAttribute {
    AttachmentStateAttribute,
    DockStatusStateAttribute,
    PresetSelectionStateAttribute {
        r#type: String,
        value: String,
    },
    BatteryStateAttribute {
        #[serde(default)]
        level: Option<f64>,
        #[serde(default)]
        flag: BatteryFlag,
    },
    StatusStateAttribute {
        value: StatusValue,
        flag: StatusFlag,
        #[serde(default)]
        error: Option<RobotError>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum StatusValue {
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "docked")]
    Docked,
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "returning")]
    Returning,
    #[serde(rename = "cleaning")]
    Cleaning,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "manual_control")]
    ManualControl,
    #[serde(rename = "moving")]
    Moving,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum StatusFlag {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "zone")]
    Zone,
    #[serde(rename = "segment")]
    Segment,
    #[serde(rename = "spot")]
    Spot,
    #[serde(rename = "target")]
    Target,
    #[serde(rename = "resumable")]
    Resumable,
    #[serde(rename = "mapping")]
    Mapping,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, EnumSetType, Serialize, Deserialize)]
#[enumset(serialize_repr = "list")]
pub(crate) enum Preset {
    #[serde(rename = "mop")]
    Mop,
    #[serde(rename = "vacuum")]
    Vacuum,
    #[serde(rename = "vacuum_and_mop")]
    VacuumAndMop,
    #[serde(rename = "vacuum_then_mop")]
    VacuumThenMop,
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "max")]
    Max,
    #[serde(rename = "off")]
    Off,
}

/// Preserve vendor details without guessing Matter error codes from translated text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct RobotError {
    #[serde(default)]
    pub(crate) message: String,
    #[serde(default, rename = "vendorErrorCode")]
    pub(crate) vendor_error_code: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BatteryFlag {
    None,
    Charging,
    Discharging,
    Charged,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatteryState {
    /// Matter uses half-percent units. Invalid/missing API values stay unknown.
    pub(crate) half_percent: Option<u8>,
    pub(crate) flag: BatteryFlag,
}
impl BatteryState {
    pub(crate) fn from_api(level: Option<f64>, flag: BatteryFlag) -> Self {
        Self {
            half_percent: level
                .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
                .map(|v| (v * 2.0).round() as u8),
            flag,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub(crate) enum CycleMode {
    #[default]
    Idle = 0,
    Cleaning = 1,
    Mapping = 2,
}

impl CycleMode {
    pub(crate) fn observe(self, state: super::DeviceState) -> Self {
        use StatusValue::*;
        match (state.value, state.flag) {
            (Cleaning | Moving, StatusFlag::Mapping) => Self::Mapping,
            (Cleaning, _) => Self::Cleaning,
            (Paused, _) | (Error, _) | (Unknown, _) | (Returning, _) => self,
            (Idle | Docked, StatusFlag::Resumable) => self,
            (Idle | Docked, _) => Self::Idle,
            _ => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(value: StatusValue, flag: StatusFlag) -> super::super::DeviceState {
        super::super::DeviceState { value, flag }
    }
    #[test]
    fn active_cycle_survives_pause_error_return_and_resumable_charging() {
        use StatusFlag::*;
        use StatusValue::*;
        let mut cycle = CycleMode::Idle;
        for status in [
            state(Cleaning, Segment),
            state(Paused, Resumable),
            state(Error, None),
            state(Returning, None),
            state(Docked, Resumable),
            state(Cleaning, Segment),
        ] {
            cycle = cycle.observe(status);
            assert_eq!(cycle, CycleMode::Cleaning);
        }
        assert_eq!(cycle.observe(state(Docked, None)), CycleMode::Idle);
        assert_eq!(cycle.observe(state(Idle, None)), CycleMode::Idle);
    }
    #[test]
    fn mapping_and_navigation_are_distinguished() {
        use StatusFlag::*;
        use StatusValue::*;
        assert_eq!(
            CycleMode::Idle.observe(state(Moving, Target)),
            CycleMode::Idle
        );
        let mapping = CycleMode::Idle.observe(state(Moving, Mapping));
        assert_eq!(mapping, CycleMode::Mapping);
        assert_eq!(
            mapping.observe(state(Paused, Resumable)),
            CycleMode::Mapping
        );
        assert_eq!(
            mapping.observe(state(Docked, Resumable)),
            CycleMode::Mapping
        );
        assert_eq!(mapping.observe(state(Docked, None)), CycleMode::Idle);
    }
    #[test]
    fn unknown_state_does_not_invent_cycle_completion() {
        assert_eq!(
            CycleMode::Cleaning.observe(state(StatusValue::Unknown, StatusFlag::Unknown)),
            CycleMode::Cleaning
        );
        assert_eq!(
            CycleMode::Idle.observe(state(StatusValue::Returning, StatusFlag::None)),
            CycleMode::Idle
        );
    }
    #[test]
    fn battery_unknown_and_invalid_are_not_zero_percent() {
        for level in [
            None,
            Some(-1.0),
            Some(101.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            assert_eq!(
                BatteryState::from_api(level, BatteryFlag::Unknown).half_percent,
                None
            );
        }
        assert_eq!(
            BatteryState::from_api(Some(25.5), BatteryFlag::Charging).half_percent,
            Some(51)
        );
        assert_eq!(
            BatteryState::from_api(Some(100.0), BatteryFlag::Charged).half_percent,
            Some(200)
        );
    }
    #[test]
    fn vendor_error_details_survive_api_deserialization() {
        let attr: StateAttribute = serde_json::from_str(r#"{"__class":"StatusStateAttribute","value":"error","flag":"none","error":{"message":"Laser obstruction","vendorErrorCode":"1","subsystem":"navigation"}}"#).unwrap();
        let StateAttribute::StatusStateAttribute {
            error: Some(error), ..
        } = attr
        else {
            panic!("missing error");
        };
        assert_eq!(error.message, "Laser obstruction");
        assert_eq!(error.vendor_error_code, "1");
    }
}
