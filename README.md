# Valetudo Matter


<p align="center">
  <img src="doc/screenshot-cleaning.png" width="300" alt="Cleaning controls in Apple Home" />
  &nbsp;&nbsp;&nbsp;&nbsp;
  <img src="doc/screenshot-settings.png" width="300" alt="Device settings and automations" />
</p>

A [Matter](https://csa-iot.org/all-solutions/matter/) implementation for
[Valetudo](https://valetudo.cloud/)-controlled robot vacuums. Runs on the robot as a separate process alongside valetudo.

> [!NOTE]
> I'm not affiliated with Valetudo and this is not an official extension; please ask for support here, not there.

## Supported features

- Start, pause, resume and return to dock with observed-state reconciliation
- Cleaning modes derived from supported fan and operation presets
- Room selection with map refresh and stale-selection validation
- Mapping only when Valetudo advertises MappingPassCapability
- Battery percentage and charging state when available
- Timed Identify, operational events and eligible completion durations
- API-derived identity and opt-in native Wi-Fi/General Diagnostics

## Installation

Download the correct release for your platform to the robot. Then add something like the following to `/data/_root_postboot.sh` after Valetudo starts:

```bash
if [[ -f /data/valetudo-matter ]]; then
    sleep 30
    VALETUDO_MATTER_BASE_URI=http://localhost VALETUDO_MATTER_PERSISTENCE=/data/matter \
	    /data/valetudo-matter > /dev/null 2>&1 &
fi
```

In order to get the commissioning code, run the tool once with logging enabled:

```sh
VALETUDO_MATTER_BASE_URI=http://localhost VALETUDO_MATTER_PERSISTENCE=/data/matter \
    RUST_LOG=info /data/valetudo-matter
```

It'll print a QR code for comissioning. Once that's completed, you can kill the process and reboot the robot.


Use the same persistence directory for commissioning and every subsequent start.
Choose a location on the robot's persistent filesystem; paths differ between models.
For example, rooted Roborock S5 installations commonly use `/mnt/data` instead of `/data`.

Without `VALETUDO_MATTER_PERSISTENCE`, the Linux default is
`$XDG_STATE_HOME/valetudo-matter`, or `$HOME/.local/state/valetudo-matter` when
`XDG_STATE_HOME` is unset or empty. The state home must be absolute. On systems
without these variables, configure the persistence directory explicitly.

Older versions defaulted to temporary storage. If that legacy directory contains
state, the bridge stops instead of silently creating a different commissioned
identity. Stop all bridge processes, preserve the complete old directory, copy it
to a new durable location, and set `VALETUDO_MATTER_PERSISTENCE` explicitly. Do not
merge or overwrite two existing state directories; select the one containing the
fabrics you intend to retain. The bridge does not migrate or delete files itself.

## Native Valetudo network binding

When `VALETUDO_MATTER_NATIVE_DIAGNOSTICS=1`, the native Matter implementation must run on the same Linux host as Valetudo. It resolves the connected Wi-Fi IPv4 address returned by the Valetudo API to exactly one local interface, then restricts both Matter UDP and mDNS sockets with `SO_BINDTODEVICE`. The OS interface lookup is only for transport routing; robot network telemetry continues to come from Valetudo. A missing, disconnected, remote, or ambiguous API address prevents startup instead of selecting another interface. Linux permissions sufficient for `SO_BINDTODEVICE` are required (the native S5 service already runs as root). External bridge installations retain host interface selection.

General Diagnostics provides a monotonic application-lifetime TimeSnapshot with null wall-clock time because synchronized wall-clock availability is not established. Startup emits Basic Information StartUp followed by General Diagnostics BootReason (Unspecified); event numbers are restored from the existing persistence directory. Unsupported optional Wi-Fi/fault events are not advertised.


## Native robot identity and software versions

`VALETUDO_MATTER_NATIVE_DIAGNOSTICS=1` explicitly selects an installation on the
robot (including the native S5 service). It represents the robot as a direct RVC:
endpoint 0 Root Node, endpoint 1 RVC, and optional endpoint 2 Power Source.
There is no Aggregator, Bridged Node device type, or Bridged Device Basic
Information cluster. External mode also retains the existing direct RVC topology.
The word "bridge" in older documentation was not a Matter topology declaration.

In native mode, Basic Information uses Valetudo's `/api/v2/robot` manufacturer and
modelName, and `/api/v2/robot/properties` serialNumber and firmwareVersion.
`SoftwareVersionString` is the unmodified robot firmware string, e.g. `002034`;
leading zeroes are preserved. The host serial override is ignored in native mode.
Missing serial remains empty. Missing, blank, malformed or over-64-byte UTF-8
firmware becomes `unknown`, without preventing startup or substituting another
component's version. Valid firmware is never truncated. Required manufacturer,
model and serial validation otherwise remain unchanged.

Valetudo and valetudo-matter versions are separate fields in the startup
`Software diagnostics` JSON log and in `valetudo-matter --diagnostics`. Run this
command with the same base URI and native-mode environment as the service. It
only reads the API and prints component versions; it does not start Matter,
resolve/write the persistence directory or open commissioning. Optional missing
component versions are `unknown`. These are support diagnostics, not invented
attributes in standard Matter diagnostic clusters. External mode retains its
composite SoftwareVersionString and optional serial override.

### Numeric SoftwareVersion policy

Matter Core 1.6.1 sections 11.1.5.10–11 define the ordered numeric software release
and human-readable text separately; they need not have a decimal conversion.
`SoftwareVersion` remains the explicitly assigned release number of this Matter
implementation: this contribution uses 10010, continuing the 10009 release
previously deployed during native testing. The number is newer even though native
presentation now shows `002034`. Parsing that string as 2034 would regress the
installed sequence; timestamps, API availability and arbitrary vendor version
strings must not alter the numeric value. The same number applies when firmware
metadata is unavailable, and StartUp reports that same number.

Release maintainers must assign increasing numbers to subsequent supported
software releases, including integrated robot/Valetudo changes shipped as a new
supported combination. A separately installed vendor firmware is not automatically
ordered by this implementation counter: generic Valetudo metadata supplies no
cross-model ordering contract. This limitation must be resolved with an explicit
release mapping before adding integrated Matter OTA. There is currently no Matter
OTA Requestor here. If OTA is added, image headers must use the same release
number; functional rollbacks need a newly numbered release (Core 11.20.3.3), not
reuse of an old lower number. The counter is compiled release metadata, not a
new mutable identity or per-boot persistent counter.

This patch does not change VID/PID, attestation, commissioning credentials,
UniqueID generation, stored Basic Information, fabric storage or Wi-Fi settings.


## Native Roborock S5 deployment

See [native deployment](doc/native-deployment.md) for ARMv7 build and supervisor
examples. Native mode is opt-in; other installations retain host network selection.
See [validation and limits](doc/validation.md) for the tested hardware, API
prerequisites and remaining interoperability limitations.
