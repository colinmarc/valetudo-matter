# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- API-derived robot identity, native firmware display and separate component diagnostics.
- Optional native Linux interface binding for Matter and mDNS using the Valetudo Wi-Fi API.
- Wi-Fi and General Diagnostics, including local MAC/IPs when available, uptime,
  TimeSnapshot and persisted startup event numbering.
- Power Source battery percentage, charge assessment and charging state.
- Fan preset clean modes, capability-gated Mapping and optional cycle durations.
- ARMv7 build helper and rooted S5 supervisor examples.

### Fixed

- Preserve partially received SSE events across pending reads and bound HTTP operations.
- Use durable Matter storage defaults and reject ambiguous legacy migrations.
- Handle mDNS multicast on older Linux kernels.
- Reconcile Pause/Resume/GoHome acknowledgements with newer observed states.
- Report docking/charging, errors and completion events coherently across a cycle.
- Refresh Service Area metadata and reject stale/deleted room selections.
- Bound Identify duration, cancellation and restart behavior.
- Omit unsupported optional capabilities, events and unavailable telemetry.


## [0.1.0](https://github.com/colinmarc/valetudo-matter/releases/tag/v0.1.0) - 2026-05-01

### Added

- map segments
- cleaning mode
- automatic state updates

### Fixed

- notify on state changes

### Other

- install clippy manually
- release v0.1.0
- drop zigbuild for cross compilation
- ci/cd
- bump matter-rs
- bump rs-matter
- initial commit
