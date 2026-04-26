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
    BatteryStateAttribute,
    StatusStateAttribute {
        value: StatusValue,
        flag: StatusFlag,
    },
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
}
