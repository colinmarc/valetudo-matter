# Native Linux deployment

These examples were exercised on a rooted Roborock S5 with firmware 002034.
Paths and init conventions are model-specific. They are not an OTA installer.

## Build

Install Rust with `armv7-unknown-linux-musleabihf`, Zig and cargo-zigbuild, then run
`./build-s5.sh`. The binary is in
`target/armv7-unknown-linux-musleabihf/release/valetudo-matter`.
The helper respects your normal Cargo/Rustup environment and has no workstation
paths. Native device binding needs Linux and permission for SO_BINDTODEVICE.

## Service and upgrades

`deploy/run.sh` supervises `/mnt/data/valetudo-matter/valetudo-matter`, stores node
state under `/mnt/data/valetudo-matter/persistence`, and keeps two logs in RAM.
`deploy/S12valetudo-matter` is an example for `/etc/init`; inspect your boot scheme
before enabling it. Install both scripts with executable permissions (0755). Start after Valetudo and the Wi-Fi connection are available.
The loop retries startup after 30 seconds if the API/interface is unavailable.

For an upgrade, stop this service, verify that the child exited, back up the
executable and the complete persistence directory, atomically replace the binary,
and restart with the same environment. Do not delete persistence to refresh UI
metadata. A binary rollback should preserve the current fabric state rather than
restore stale session/fabric files from an earlier backup.

Native mode is enabled by `VALETUDO_MATTER_NATIVE_DIAGNOSTICS=1`. Network and
identity data come from the Valetudo API. OS interface enumeration only resolves
the API IP address to the local interface used for transport. No additional robot
private interfaces are exposed. `--diagnostics` reads component versions and exits
without starting Matter or accessing node persistence.

Keep test attestation and VID/PID implications in mind: this implementation is not
a certified Roborock product. Do not publish pairing codes, device credentials,
keys, persistence backups or raw logs in issues.
