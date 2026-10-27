//! Optional, monotonic bridge-observed operation timing.
//!
//! This is not firmware history or motor-on time. Only successful Matter starts
//! with continuously observed activity qualify. Acknowledgement promotes the
//! command-receipt anchor; restart, stream gaps or polling-discovered transitions
//! invalidate it. Unknown timing is omitted; overflow is encoded as Matter null.
use super::{CycleMode, DeviceState, StatusFlag, StatusValue};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompletionTiming {
    pub(crate) total_seconds: Option<u32>,
    pub(crate) paused_seconds: Option<u32>,
}

#[derive(Debug, Default)]
pub(crate) struct CycleTiming {
    measurement: Option<Measurement>,
}

#[derive(Debug)]
struct Measurement {
    mode: CycleMode,
    started: Instant,
    last_observation: Instant,
    acknowledged: bool,
    observed_activity: bool,
    paused_since: Option<Instant>,
    paused: Duration,
}

impl CycleTiming {
    /// Call only after validating a new-cycle command, before awaiting its API.
    /// Rejected transitions never call this, so they cannot erase an active clock.
    pub(crate) fn begin(&mut self, mode: CycleMode, received_at: Instant) {
        self.measurement = (mode != CycleMode::Idle).then_some(Measurement {
            mode,
            started: received_at,
            last_observation: received_at,
            acknowledged: false,
            observed_activity: false,
            paused_since: None,
            paused: Duration::ZERO,
        });
    }

    pub(crate) fn acknowledge(&mut self) {
        if let Some(measurement) = &mut self.measurement {
            measurement.acknowledged = true;
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.measurement = None;
    }

    pub(crate) fn observe(&mut self, state: DeviceState, at: Instant) {
        let Some(measurement) = &mut self.measurement else {
            return;
        };
        if at < measurement.last_observation || state.value == StatusValue::Unknown {
            self.invalidate();
            return;
        }
        let observed_mode = match (state.value, state.flag) {
            (StatusValue::Cleaning | StatusValue::Moving, StatusFlag::Mapping) => {
                Some(CycleMode::Mapping)
            }
            (StatusValue::Cleaning, _) => Some(CycleMode::Cleaning),
            _ => None,
        };
        if observed_mode.is_some_and(|mode| mode != measurement.mode)
            || matches!(state.value, StatusValue::ManualControl)
            || (state.value == StatusValue::Moving && state.flag != StatusFlag::Mapping)
            || (!measurement.observed_activity
                && matches!(
                    state.value,
                    StatusValue::Paused | StatusValue::Returning | StatusValue::Error
                ))
        {
            self.invalidate();
            return;
        }
        if observed_mode.is_some() {
            measurement.observed_activity = true;
        }
        if state.value == StatusValue::Paused {
            measurement.paused_since.get_or_insert(at);
        } else if let Some(since) = measurement.paused_since.take() {
            let Some(paused) = measurement.paused.checked_add(at.duration_since(since)) else {
                self.invalidate();
                return;
            };
            measurement.paused = paused;
        }
        measurement.last_observation = at;
    }

    /// Freeze into the queued event now, not when an event subscriber reads it.
    pub(crate) fn finish(&mut self, at: Instant) -> Option<CompletionTiming> {
        let measurement = self.measurement.take()?;
        if !measurement.acknowledged
            || !measurement.observed_activity
            || at < measurement.last_observation
        {
            return None;
        }
        let total = at.checked_duration_since(measurement.started)?;
        let paused = match measurement.paused_since {
            Some(since) => measurement
                .paused
                .checked_add(at.checked_duration_since(since)?)?,
            None => measurement.paused,
        };
        Some(CompletionTiming {
            total_seconds: u32::try_from(total.as_secs()).ok(),
            paused_seconds: u32::try_from(paused.as_secs()).ok(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(value: StatusValue, flag: StatusFlag) -> DeviceState {
        DeviceState { value, flag }
    }
    #[test]
    fn includes_paused_time_in_total_and_counts_only_paused_intervals() {
        let start = Instant::now();
        let mut clock = CycleTiming::default();
        clock.begin(CycleMode::Cleaning, start);
        clock.observe(
            state(StatusValue::Cleaning, StatusFlag::Segment),
            start + Duration::from_secs(1),
        );
        clock.acknowledge(); // Activity may precede the HTTP acknowledgement.
        clock.observe(
            state(StatusValue::Paused, StatusFlag::Resumable),
            start + Duration::from_secs(10),
        );
        clock.observe(
            state(StatusValue::Paused, StatusFlag::Resumable),
            start + Duration::from_secs(20),
        );
        clock.observe(
            state(StatusValue::Cleaning, StatusFlag::Segment),
            start + Duration::from_secs(30),
        );
        clock.observe(
            state(StatusValue::Returning, StatusFlag::None),
            start + Duration::from_secs(40),
        );
        assert_eq!(
            clock.finish(start + Duration::from_secs(45)),
            Some(CompletionTiming {
                total_seconds: Some(45),
                paused_seconds: Some(20)
            })
        );
        assert_eq!(clock.finish(start + Duration::from_secs(50)), None);
    }
    #[test]
    fn completion_while_paused_and_resumable_charging_are_not_conflated() {
        let start = Instant::now();
        let mut clock = CycleTiming::default();
        clock.begin(CycleMode::Cleaning, start);
        clock.acknowledge();
        clock.observe(state(StatusValue::Cleaning, StatusFlag::None), start);
        clock.observe(
            state(StatusValue::Docked, StatusFlag::Resumable),
            start + Duration::from_secs(5),
        );
        clock.observe(
            state(StatusValue::Paused, StatusFlag::Resumable),
            start + Duration::from_secs(10),
        );
        assert_eq!(
            clock.finish(start + Duration::from_secs(30)),
            Some(CompletionTiming {
                total_seconds: Some(30),
                paused_seconds: Some(20)
            })
        );
    }
    #[test]
    fn gap_restart_external_cycle_and_unacknowledged_start_omit_timings() {
        let now = Instant::now();
        let mut clock = CycleTiming::default();
        assert_eq!(clock.finish(now), None); // External/restarted operation.
        clock.begin(CycleMode::Cleaning, now);
        clock.observe(state(StatusValue::Cleaning, StatusFlag::None), now);
        assert_eq!(clock.finish(now), None); // No successful acknowledgement.
        clock.begin(CycleMode::Cleaning, now);
        clock.acknowledge();
        clock.observe(state(StatusValue::Cleaning, StatusFlag::None), now);
        clock.invalidate();
        assert_eq!(clock.finish(now), None);
        clock.begin(CycleMode::Cleaning, now);
        clock.acknowledge();
        clock.observe(state(StatusValue::Moving, StatusFlag::Mapping), now);
        assert_eq!(clock.finish(now), None); // Different external operation.
    }
    #[test]
    fn overflow_is_unknown_not_saturated_and_subseconds_accumulate_before_rounding() {
        let now = Instant::now();
        let mut clock = CycleTiming::default();
        clock.begin(CycleMode::Cleaning, now);
        clock.acknowledge();
        clock.observe(state(StatusValue::Cleaning, StatusFlag::None), now);
        clock.observe(state(StatusValue::Paused, StatusFlag::Resumable), now);
        let end = now + Duration::from_secs(u64::from(u32::MAX) + 1);
        assert_eq!(
            clock.finish(end),
            Some(CompletionTiming {
                total_seconds: None,
                paused_seconds: None
            })
        );
        clock.begin(CycleMode::Cleaning, now);
        clock.acknowledge();
        clock.observe(state(StatusValue::Cleaning, StatusFlag::None), now);
        clock.observe(
            state(StatusValue::Paused, StatusFlag::Resumable),
            now + Duration::from_millis(100),
        );
        clock.observe(
            state(StatusValue::Cleaning, StatusFlag::None),
            now + Duration::from_millis(700),
        );
        clock.observe(
            state(StatusValue::Paused, StatusFlag::Resumable),
            now + Duration::from_millis(800),
        );
        assert_eq!(
            clock.finish(now + Duration::from_millis(1400)),
            Some(CompletionTiming {
                total_seconds: Some(1),
                paused_seconds: Some(1)
            })
        );
    }
}
