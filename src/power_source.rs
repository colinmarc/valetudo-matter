//! Robot battery telemetry from Valetudo, never from the bridge host battery.
use crate::device::{BatteryFlag, BatteryState, Device};
use rs_matter::{
    dm::{
        ArrayAttributeRead, ReadContext,
        clusters::decl::power_source::{self, *},
    },
    error::{Error, ErrorCode},
    tlv::{Nullable, TLVBuilderParent, Utf8StrBuilder},
};

/// Reporting projection for Core 1.6.1 section 11.7.7.13. Raw telemetry stays
/// separate so RVC charging state is immediate. All Power Source reads use this
/// projection, including wildcard reports triggered by charge-flag changes.
pub(crate) struct PowerBattery {
    value: crate::handlers::VersionedCell<BatteryState>,
    last_percent: std::cell::Cell<Option<std::time::Instant>>,
}
impl PowerBattery {
    pub(crate) fn new(initial: BatteryState, now: std::time::Instant) -> Self {
        Self {
            value: crate::handlers::VersionedCell::new(initial, &mut rand::thread_rng()),
            last_percent: std::cell::Cell::new(initial.half_percent.map(|_| now)),
        }
    }
    pub(crate) fn update(
        &self,
        raw: BatteryState,
        now: std::time::Instant,
        notify: &impl rs_matter::dm::AttrChangeNotifier,
    ) {
        let old = self.value.get();
        let mut next = raw;
        if raw.half_percent != old.half_percent {
            let null_transition = raw.half_percent.is_none() || old.half_percent.is_none();
            let due = self.last_percent.get().is_none_or(|last| {
                now.saturating_duration_since(last) >= std::time::Duration::from_secs(10)
            });
            if null_transition || due {
                self.last_percent.set(raw.half_percent.map(|_| now));
            } else {
                next.half_percent = old.half_percent;
            }
        }
        if next != old {
            self.value.set(next);
            notify.notify_cluster_changed(2, 0x2f);
        }
    }
}

pub(crate) struct PowerSource<'a> {
    device: &'a Device,
    native: bool,
}
impl<'a> PowerSource<'a> {
    pub(crate) fn new(device: &'a Device, native: bool) -> Self {
        Self { device, native }
    }
}
impl rs_matter::utils::sync::DynBase for PowerSource<'_> {}
// Core 1.6.1 section 11.7.7.32: a nonempty list includes its hosting endpoint.
fn supplied_endpoints(native: bool) -> &'static [u16] {
    if native { &[0, 1, 2] } else { &[1, 2] }
}

fn source_status(battery: BatteryState) -> PowerSourceStatusEnum {
    match battery.flag {
        BatteryFlag::Discharging => PowerSourceStatusEnum::Active,
        // Charging does not prove whether the battery or charger supplies the
        // robot electronics. Do not infer an electrical topology from docking.
        _ => PowerSourceStatusEnum::Unspecified,
    }
}

fn charge_state(flag: BatteryFlag) -> BatChargeStateEnum {
    match flag {
        BatteryFlag::Charging => BatChargeStateEnum::IsCharging,
        BatteryFlag::Charged => BatChargeStateEnum::IsAtFullCharge,
        BatteryFlag::Discharging => BatChargeStateEnum::IsNotCharging,
        BatteryFlag::None | BatteryFlag::Unknown => BatChargeStateEnum::Unknown,
    }
}

/// Bridge assessment policy, not a reported robot/vendor threshold: <=5% is
/// critical, <=20% is warning, above that nominal. Missing data is unavailable,
/// never a fabricated zero or critical alarm. Half-percent units are preserved.
fn charge_level(half_percent: Option<u8>) -> Result<BatChargeLevelEnum, Error> {
    match half_percent {
        Some(0..=10) => Ok(BatChargeLevelEnum::Critical),
        Some(11..=40) => Ok(BatChargeLevelEnum::Warning),
        Some(41..=200) => Ok(BatChargeLevelEnum::OK),
        _ => Err(ErrorCode::Failure.into()),
    }
}

impl power_source::ClusterHandler for PowerSource<'_> {
    const CLUSTER: rs_matter::dm::Cluster<'static> = power_source::FULL_CLUSTER
        .with_features(Feature::BATTERY.bits() | Feature::RECHARGEABLE.bits())
        .with_attrs(rs_matter::with!(required;
            AttributeId::BatPercentRemaining | AttributeId::BatChargeLevel |
            AttributeId::BatReplacementNeeded | AttributeId::BatReplaceability |
            AttributeId::BatChargeState | AttributeId::BatFunctionalWhileCharging))
        .with_cmds(rs_matter::with!())
        .with_events(rs_matter::with!());

    fn dataver(&self) -> u32 {
        self.device.power_battery.value.dataver()
    }
    fn dataver_changed(&self) {
        self.device.power_battery.value.dataver_changed();
    }
    fn status(&self, _: impl ReadContext) -> Result<PowerSourceStatusEnum, Error> {
        Ok(source_status(self.device.power_battery.value.get()))
    }
    fn order(&self, _: impl ReadContext) -> Result<u8, Error> {
        Ok(0)
    }
    fn description<P: TLVBuilderParent>(
        &self,
        _: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        builder.set("Robot battery")
    }
    fn bat_percent_remaining(&self, _: impl ReadContext) -> Result<Nullable<u8>, Error> {
        Ok(self.device.power_battery.value.get().half_percent.into())
    }
    fn bat_charge_level(&self, _: impl ReadContext) -> Result<BatChargeLevelEnum, Error> {
        charge_level(self.device.power_battery.value.get().half_percent)
    }
    fn bat_replacement_needed(&self, _: impl ReadContext) -> Result<bool, Error> {
        // No replacement condition has been detected by this implementation.
        Ok(false)
    }
    fn bat_replaceability(&self, _: impl ReadContext) -> Result<BatReplaceabilityEnum, Error> {
        Ok(BatReplaceabilityEnum::Unspecified)
    }
    fn bat_charge_state(&self, _: impl ReadContext) -> Result<BatChargeStateEnum, Error> {
        Ok(charge_state(self.device.power_battery.value.get().flag))
    }
    fn bat_functional_while_charging(&self, _: impl ReadContext) -> Result<bool, Error> {
        // The robot's Valetudo API remains operational while charging. This
        // attribute does not assert that cleaning can occur while docked.
        Ok(true)
    }
    fn endpoint_list<P: TLVBuilderParent>(
        &self,
        _: impl ReadContext,
        builder: ArrayAttributeRead<
            rs_matter::tlv::ToTLVArrayBuilder<P, u16>,
            rs_matter::tlv::ToTLVBuilder<P, u16>,
        >,
    ) -> Result<P, Error> {
        let endpoints = supplied_endpoints(self.native);
        match builder {
            ArrayAttributeRead::ReadAll(mut builder) => {
                for endpoint in endpoints {
                    builder = builder.push(endpoint)?;
                }
                builder.end()
            }
            ArrayAttributeRead::ReadOne(index, builder) => builder.set(
                endpoints
                    .get(index as usize)
                    .ok_or(ErrorCode::ConstraintError)?,
            ),
            ArrayAttributeRead::ReadNone(builder) => builder.end(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coarse_assessment_policy_has_explicit_thresholds_and_unknown() {
        assert_eq!(
            charge_level(Some(10)).unwrap(),
            BatChargeLevelEnum::Critical
        );
        assert_eq!(charge_level(Some(11)).unwrap(), BatChargeLevelEnum::Warning);
        assert_eq!(charge_level(Some(40)).unwrap(), BatChargeLevelEnum::Warning);
        assert_eq!(charge_level(Some(41)).unwrap(), BatChargeLevelEnum::OK);
        assert!(charge_level(None).is_err());
        assert!(charge_level(Some(201)).is_err());
    }
    #[test]
    fn charging_flags_do_not_invent_power_topology() {
        assert_eq!(
            charge_state(BatteryFlag::Charging),
            BatChargeStateEnum::IsCharging
        );
        assert_eq!(
            charge_state(BatteryFlag::Charged),
            BatChargeStateEnum::IsAtFullCharge
        );
        assert_eq!(
            charge_state(BatteryFlag::Discharging),
            BatChargeStateEnum::IsNotCharging
        );
        assert_eq!(
            charge_state(BatteryFlag::Unknown),
            BatChargeStateEnum::Unknown
        );
        assert_eq!(
            source_status(BatteryState::default()),
            PowerSourceStatusEnum::Unspecified
        );
        assert_eq!(
            source_status(BatteryState {
                half_percent: Some(100),
                flag: BatteryFlag::Charging
            }),
            PowerSourceStatusEnum::Unspecified
        );
        assert_eq!(
            source_status(BatteryState {
                half_percent: Some(100),
                flag: BatteryFlag::Discharging
            }),
            PowerSourceStatusEnum::Active
        );
    }
    #[test]
    fn feature_metadata_does_not_advertise_unmeasured_optional_values() {
        let cluster = <PowerSource as power_source::ClusterHandler>::CLUSTER;
        assert_eq!(cluster.feature_map, 6);
        let attrs: Vec<_> = cluster
            .attributes
            .iter()
            .filter(|attr| (cluster.with_attrs)(attr, cluster.revision, cluster.feature_map))
            .map(|attr| attr.id)
            .collect();
        assert!(attrs.contains(&(AttributeId::BatPercentRemaining as u32)));
        assert!(attrs.contains(&(AttributeId::BatChargeState as u32)));
        for id in [
            AttributeId::Status,
            AttributeId::Order,
            AttributeId::Description,
            AttributeId::EndpointList,
            AttributeId::BatChargeLevel,
            AttributeId::BatReplacementNeeded,
            AttributeId::BatReplaceability,
            AttributeId::BatFunctionalWhileCharging,
        ] {
            assert!(attrs.contains(&(id as u32)));
        }
        assert_eq!(supplied_endpoints(true), &[0, 1, 2]);
        assert_eq!(supplied_endpoints(false), &[1, 2]);
        assert!(!attrs.contains(&(AttributeId::BatVoltage as u32)));
        assert!(!attrs.contains(&(AttributeId::BatCapacity as u32)));
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[derive(Default)]
    struct Notify(std::cell::Cell<usize>);
    impl rs_matter::dm::AttrChangeNotifier for Notify {
        fn notify_attr_changed(&self, _: u16, _: u32, _: u32) {
            panic!("unexpected");
        }
        fn notify_cluster_changed(&self, ep: u16, cl: u32) {
            assert_eq!((ep, cl), (2, 0x2f));
            self.0.set(self.0.get() + 1);
        }
        fn notify_endpoint_changed(&self, _: u16) {
            panic!("unexpected");
        }
        fn notify_all_changed(&self) {
            panic!("unexpected");
        }
    }
    fn raw(percent: Option<u8>, flag: BatteryFlag) -> BatteryState {
        BatteryState {
            half_percent: percent,
            flag,
        }
    }
    #[test]
    fn percentage_window_latest_wins_flags_are_immediate_and_null_bypasses() {
        let t = Instant::now();
        let n = Notify::default();
        let p = PowerBattery::new(raw(Some(100), BatteryFlag::Discharging), t);
        let version = p.value.dataver();
        p.update(
            raw(Some(101), BatteryFlag::Discharging),
            t + Duration::from_secs(1),
            &n,
        );
        assert_eq!(p.value.get().half_percent, Some(100));
        assert_eq!(p.value.dataver(), version);
        p.update(
            raw(Some(102), BatteryFlag::Charging),
            t + Duration::from_secs(2),
            &n,
        );
        assert_eq!(p.value.get(), raw(Some(100), BatteryFlag::Charging));
        assert_eq!(n.0.get(), 1);
        p.update(
            raw(Some(103), BatteryFlag::Charging),
            t + Duration::from_millis(9999),
            &n,
        );
        assert_eq!(p.value.get().half_percent, Some(100));
        assert_eq!(n.0.get(), 1);
        // Periodic update receives the latest raw observation, not the first pending value.
        p.update(
            raw(Some(103), BatteryFlag::Charging),
            t + Duration::from_secs(10),
            &n,
        );
        assert_eq!(p.value.get().half_percent, Some(103));
        assert_eq!(n.0.get(), 2);
        p.update(
            raw(None, BatteryFlag::Unknown),
            t + Duration::from_secs(11),
            &n,
        );
        assert_eq!(p.value.get().half_percent, None);
        p.update(
            raw(Some(150), BatteryFlag::Discharging),
            t + Duration::from_secs(12),
            &n,
        );
        assert_eq!(p.value.get().half_percent, Some(150));
        assert_eq!(n.0.get(), 4);
        p.update(
            raw(Some(151), BatteryFlag::Discharging),
            t + Duration::from_secs(21),
            &n,
        );
        assert_eq!(p.value.get().half_percent, Some(150));
        p.update(
            raw(Some(151), BatteryFlag::Discharging),
            t + Duration::from_secs(22),
            &n,
        );
        assert_eq!(p.value.get().half_percent, Some(151));
    }
    #[test]
    fn return_to_published_value_has_no_pending_report_or_version_change() {
        let t = Instant::now();
        let n = Notify::default();
        let initial = raw(Some(100), BatteryFlag::Discharging);
        let p = PowerBattery::new(initial, t);
        let version = p.value.dataver();
        p.update(
            raw(Some(101), BatteryFlag::Discharging),
            t + Duration::from_secs(2),
            &n,
        );
        p.update(initial, t + Duration::from_secs(3), &n);
        p.update(initial, t + Duration::from_secs(10), &n);
        assert_eq!(n.0.get(), 0);
        assert_eq!(p.value.dataver(), version);
    }
}
