# Validation and known limits

## Evidence

The integrated native implementation was tested on a classic Roborock S5,
firmware 002034, with Valetudo 2026.10.0 plus optional serial/MAC API additions.
Apple Home and UniFi commissioning and multi-fabric operation were exercised.
Room cleaning, fan selection, pause/resume, return to dock and the transition to
Charging were observed. A first Resume attempt coincided with confirmed Wi-Fi
reassociation; control recovered after connectivity returned. This is not a claim
that all commands succeed during a transport outage.

Local regression tests use fake HTTP servers, encoded Matter replies and state
fixtures. Additional ARM tests exercised device binding, TimeSnapshot serialization,
startup events, persistent event numbers and delayed command acknowledgements on
the S5 kernel. TimeSnapshot has not been invoked through an external controller.
No Matter certification or cross-model hardware coverage is claimed.

Public checkout validation on 2026-10-27: 116 host tests passed; formatting,
strict Clippy on macOS and the ARMv7 Linux target, shell syntax and ARMv7 musl
release compilation passed. Cross-target Clippy/build checks are not execution
of the test suite on ARM. The public branding and portable deployment changes
have not been redeployed for another hardware qualification run.

## API and capability boundaries

- Native identity requires manufacturer/model from `/api/v2/robot`.
- Serial and firmware from `/api/v2/robot/properties` are optional. Native serial
  remains empty and firmware becomes `unknown` when unavailable.
- The local robot MAC is optional API metadata. An AP BSSID is never substituted.
  Older/unpatched Valetudo can omit MAC and therefore cannot provide a complete
  General Diagnostics NetworkInterfaces entry. No robot OS telemetry fallback is used.
- Native NetworkInterfaces represents the Valetudo Wi-Fi interface and transport
  is restricted to it. This deliberate node boundary needs maintainer review
  against the specification's logical-interface inventory wording.
- Classic S5 has no verified dedicated MappingPassCapability; cleaning with low
  suction is not advertised as Mapping. Models exposing the capability can use it.
- Per-room progress/CurrentArea/Skip, generic consumables-to-HEPA mapping, network
  commissioning, BLE and Matter OTA are not invented or implemented here.
- SupportedArea IDs and names follow the map API, not Apple Home room identity.

## Remaining controller behavior

Apple Home can retain stale state or a cached room selection after service restart.
A displayed cached room does not prove the device retained that selection. An empty
selection currently means general cleaning; explicitly reselect rooms after a
restart before requesting a room-specific job. Subscription recovery may be slow.
These observations remain open interoperability work, not declared solved by the
metadata/selection regressions in this contribution.

## Release policy

Numeric SoftwareVersion is 10010 to preserve ordering from previously deployed
native builds. It must be maintained separately from vendor firmware display
strings. Package-version branding replaces the private S5 build suffix in this
public branch. Supported release combinations need monotonically assigned numeric
versions; independently installed vendor firmware is not automatically ordered.
The optional components and counters do not change VID/PID or fabric identities.

## Reproduce checks

```
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
./build-s5.sh
```

The Linux bind-isolation test is intentionally ignored by default. Run it only on
an appropriate Linux host/interface with the required socket permissions, using
`VALETUDO_MATTER_TEST_INTERFACE` and the test's explicit ignored-test selector.
