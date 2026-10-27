//! Display identity only: commissioning credentials and fabric storage stay unchanged.
use anyhow::{Context, bail};
use rs_matter::dm::{
    clusters::basic_info::BasicInfoConfig,
    devices::test::{TEST_PID, TEST_VID},
};
use serde::{Deserialize, Serialize};

use crate::http::ValetudoClient;

// Ordered Matter implementation release, independent of vendor firmware strings.
// Never derive this from firmware or reset it when presentation changes.
const IMPLEMENTATION_VERSION: &str = env!("CARGO_PKG_VERSION");
const MATTER_SOFTWARE_VERSION: u32 = 10_010;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RobotInfo {
    manufacturer: String,
    model_name: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RobotProperties {
    firmware_version: Option<String>,
    serial_number: Option<String>,
}

#[derive(Default, Deserialize)]
struct ValetudoVersion {
    release: Option<String>,
}

pub(crate) struct Identity {
    manufacturer: String,
    model: String,
    serial: String,
    software: String,
    diagnostics: SoftwareDiagnostics,
}

#[derive(Serialize)]
pub(crate) struct SoftwareDiagnostics {
    native: bool,
    robot_firmware: String,
    valetudo: String,
    valetudo_matter: &'static str,
    matter_software_version: u32,
}

impl Identity {
    pub(crate) async fn load(client: &ValetudoClient, native: bool) -> anyhow::Result<Self> {
        let robot: RobotInfo = client
            .get("/api/v2/robot")
            .await
            .context("Robot identity")?;
        let properties_response: anyhow::Result<RobotProperties> =
            client.get_optional("/api/v2/robot/properties").await;
        let properties_available = properties_response.is_ok();
        let mut properties = properties_response.unwrap_or_else(|_| {
            log::warn!("Optional robot properties unavailable; SN and firmware unknown");
            RobotProperties::default()
        });
        let valetudo: ValetudoVersion = client
            .get_optional("/api/v2/valetudo/version")
            .await
            .unwrap_or_else(|_| {
                log::warn!("Optional Valetudo release unavailable");
                ValetudoVersion::default()
            });
        // Valetudo reads the serial asynchronously. Allow its bounded startup
        // retries to finish before freezing Matter Basic Information.
        for _ in 0..10 {
            if !properties_available || properties.serial_number.is_some() {
                break;
            }
            smol::Timer::after(std::time::Duration::from_secs(2)).await;
            match client.get_optional("/api/v2/robot/properties").await {
                Ok(value) => properties = value,
                Err(_) => {
                    log::warn!("Optional serial refresh unavailable");
                    break;
                }
            }
        }
        let serial = if native {
            None
        } else {
            match std::env::var("VALETUDO_MATTER_SERIAL_NUMBER") {
                Ok(value) => Some(value),
                Err(std::env::VarError::NotPresent) => None,
                Err(e) => return Err(e).context("Invalid VALETUDO_MATTER_SERIAL_NUMBER"),
            }
        };
        Self::new(robot, properties, valetudo, serial, native)
    }

    fn new(
        robot: RobotInfo,
        properties: RobotProperties,
        valetudo: ValetudoVersion,
        serial_override: Option<String>,
        native: bool,
    ) -> anyhow::Result<Self> {
        let serial = if native {
            // Native identity belongs to the robot, never to a host override.
            properties.serial_number.unwrap_or_default()
        } else {
            serial_override
                .or(properties.serial_number)
                .unwrap_or_default()
        };
        validate("manufacturer", &robot.manufacturer, 32, false)?;
        validate("model", &robot.model_name, 32, false)?;
        validate("serial number", &serial, 32, true)?;
        let diagnostics = SoftwareDiagnostics {
            native,
            robot_firmware: optional_version(properties.firmware_version.as_deref(), 64),
            valetudo: optional_version(valetudo.release.as_deref(), 64),
            valetudo_matter: IMPLEMENTATION_VERSION,
            matter_software_version: MATTER_SOFTWARE_VERSION,
        };
        let software = if native {
            diagnostics.robot_firmware.clone()
        } else {
            software_display(
                IMPLEMENTATION_VERSION,
                valetudo.release.as_deref(),
                properties.firmware_version.as_deref(),
            )?
        };
        Ok(Self {
            manufacturer: robot.manufacturer,
            model: robot.model_name,
            serial,
            software,
            diagnostics,
        })
    }

    pub(crate) fn diagnostics(&self) -> &SoftwareDiagnostics {
        &self.diagnostics
    }

    pub(crate) fn basic_info(&self) -> BasicInfoConfig<'_> {
        BasicInfoConfig {
            vendor_name: &self.manufacturer,
            product_name: &self.model,
            serial_no: &self.serial,
            sw_ver: MATTER_SOFTWARE_VERSION,
            sw_ver_str: &self.software,
            device_name: &self.model,
            device_type: Some(0x0074),
            // Keep test IDs consistent with the existing test attestation.
            // Display names do not claim Roborock Matter certification.
            vid: TEST_VID,
            pid: TEST_PID,
            ..BasicInfoConfig::new()
        }
    }
}

fn optional_version(value: Option<&str>, limit: usize) -> String {
    value
        .filter(|v| validate("optional software metadata", v, limit, false).is_ok())
        .unwrap_or("unknown")
        .to_owned()
}

// These versions annotate the bridge software; they are not stable device identity.
// Keep whole validated components, in priority order, without exceeding Matter's
// 64-byte SoftwareVersionString limit. Missing/invalid optional data is explicit.
fn software_display(
    bridge: &str,
    release: Option<&str>,
    firmware: Option<&str>,
) -> anyhow::Result<String> {
    let mut software = format!("VM {bridge}");
    validate("bridge software version", &software, 64, false)?;
    for (name, value) in [("Valetudo", release), ("FW", firmware)] {
        let value = value
            .filter(|value| validate("optional software metadata", value, 32, false).is_ok())
            .unwrap_or("unknown");
        let suffix = format!("; {name} {value}");
        if software.len() + suffix.len() <= 64 {
            software.push_str(&suffix);
        }
    }
    Ok(software)
}

fn validate(field: &str, value: &str, max_bytes: usize, allow_empty: bool) -> anyhow::Result<()> {
    if (!allow_empty && value.trim().is_empty())
        || value.len() > max_bytes
        || value.chars().any(char::is_control)
        || (!value.is_empty() && value.trim() != value)
    {
        // Do not include potentially private identity values in errors.
        bail!("Invalid {field}: expected clean UTF-8 text of at most {max_bytes} bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(serial: &str, firmware: Option<&str>) -> anyhow::Result<Identity> {
        Identity::new(
            serde_json::from_str(
                r#"{"manufacturer":"Roborock","modelName":"S5","implementation":"ignored"}"#,
            )?,
            RobotProperties {
                firmware_version: firmware.map(str::to_owned),
                serial_number: Some(serial.into()),
            },
            ValetudoVersion {
                release: Some("2026.10.0".into()),
            },
            None,
            false,
        )
    }

    #[test]
    fn live_api_shape_maps_to_display_fields_without_changing_test_ids() {
        let identity = identity("label-example", Some("002034")).unwrap();
        let info = identity.basic_info();
        assert_eq!(
            (info.vendor_name, info.product_name, info.serial_no),
            ("Roborock", "S5", "label-example")
        );
        assert_eq!(info.sw_ver_str, "VM 0.1.0; Valetudo 2026.10.0; FW 002034");
        assert_eq!(info.sw_ver, 10_010);
        assert_eq!((info.vid, info.pid), (TEST_VID, TEST_PID));
        assert_eq!((info.hw_ver, info.hw_ver_str), (0, ""));
    }

    #[test]
    fn api_serial_is_optional_and_explicit_override_wins() {
        let robot = || RobotInfo {
            manufacturer: "Roborock".into(),
            model_name: "S5".into(),
        };
        let version = || ValetudoVersion {
            release: Some("2026.10.0".into()),
        };
        let properties = || {
            serde_json::from_str::<RobotProperties>(
                r#"{"firmwareVersion":"002034","serialNumber":"api-serial"}"#,
            )
            .unwrap()
        };
        let automatic = Identity::new(robot(), properties(), version(), None, false).unwrap();
        assert_eq!(automatic.basic_info().serial_no, "api-serial");
        let explicit = Identity::new(
            robot(),
            properties(),
            version(),
            Some("override".into()),
            false,
        )
        .unwrap();
        assert_eq!(explicit.basic_info().serial_no, "override");
        let old_api =
            serde_json::from_str::<RobotProperties>(r#"{"firmwareVersion":"002034"}"#).unwrap();
        assert_eq!(
            Identity::new(robot(), old_api, version(), None, false)
                .unwrap()
                .basic_info()
                .serial_no,
            ""
        );
    }

    #[test]
    fn unknown_values_are_not_invented() {
        let identity = identity("", None).unwrap();
        assert_eq!(identity.basic_info().serial_no, "");
        assert!(identity.basic_info().sw_ver_str.ends_with("FW unknown"));
    }

    #[test]
    fn rejects_overlong_or_malformed_values_without_truncating_identity() {
        assert!(identity(&"x".repeat(33), Some("002034")).is_err());
        assert!(identity("serial\n", Some("002034")).is_err());
        assert!(identity(" serial", Some("002034")).is_err());
        assert!(identity(&"é".repeat(17), Some("002034")).is_err());
        assert!(identity("", Some(&"v".repeat(32))).is_ok());
        assert!(
            identity("", Some(""))
                .unwrap()
                .basic_info()
                .sw_ver_str
                .ends_with("FW unknown")
        );
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;
    #[test]
    fn optional_components_never_break_valid_device_identity_or_split_utf8() {
        for release in ["v".repeat(32), "é".repeat(16), "2026.10.0".into()] {
            for firmware in ["f".repeat(32), "é".repeat(16), "002034".into()] {
                let identity = Identity::new(
                    RobotInfo {
                        manufacturer: "Vendor".into(),
                        model_name: "Model".into(),
                    },
                    RobotProperties {
                        firmware_version: Some(firmware.clone()),
                        serial_number: Some("stable-serial".into()),
                    },
                    ValetudoVersion {
                        release: Some(release.clone()),
                    },
                    None,
                    false,
                )
                .unwrap();
                let info = identity.basic_info();
                assert_eq!(
                    (info.vendor_name, info.product_name, info.serial_no),
                    ("Vendor", "Model", "stable-serial")
                );
                assert!(info.sw_ver_str.len() <= 64);
                assert!(
                    info.sw_ver_str
                        .starts_with(&format!("VM {IMPLEMENTATION_VERSION}"))
                );
                if info.sw_ver_str.contains("; Valetudo ") {
                    assert!(info.sw_ver_str.contains(&format!("; Valetudo {release}")));
                }
                if info.sw_ver_str.contains("; FW ") {
                    assert!(info.sw_ver_str.ends_with(&format!("; FW {firmware}")));
                }
            }
        }
    }
    #[test]
    fn unavailable_or_malformed_optional_versions_are_qualified_as_unknown() {
        for value in [
            None,
            Some(""),
            Some("bad\nrelease"),
            Some(" leading"),
            Some("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
        ] {
            assert_eq!(
                software_display("1.2.3", value, value).unwrap(),
                "VM 1.2.3; Valetudo unknown; FW unknown"
            );
        }
    }
    #[test]
    fn composition_keeps_complete_components_at_boundary() {
        assert_eq!(
            software_display(&"b".repeat(61), Some("v"), Some("f")).unwrap(),
            format!("VM {}", "b".repeat(61))
        );
        assert!(software_display(&"b".repeat(62), None, None).is_err());
        let version =
            software_display("1.2.3", Some(&"r".repeat(32)), Some(&"f".repeat(32))).unwrap();
        assert_eq!(version, format!("VM 1.2.3; Valetudo {}", "r".repeat(32)));
    }
    #[test]
    fn optional_release_http_failure_does_not_prevent_loading_identity() {
        for native in [false, true] {
            load_without_release(native);
        }
    }
    fn load_without_release(native: bool) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (path, status, body) in [
                (
                    "/api/v2/robot",
                    200,
                    r#"{"manufacturer":"Vendor","modelName":"Model"}"#,
                ),
                (
                    "/api/v2/robot/properties",
                    200,
                    r#"{"serialNumber":"stable","firmwareVersion":"002034"}"#,
                ),
                ("/api/v2/valetudo/version", 404, "{}"),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap();
                let mut request_line = request.lines().next().unwrap().split_whitespace();
                assert_eq!(request_line.next(), Some("GET"));
                let uri: hyper::Uri = request_line.next().unwrap().parse().unwrap();
                assert_eq!(uri.path(), path);
                write!(stream,"HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let executor = Box::leak(Box::new(smol::LocalExecutor::new()));
        smol::block_on(executor.run(async {
            let client =
                ValetudoClient::new(executor, format!("http://{address}").parse().unwrap());
            let identity = Identity::load(&client, native).await.unwrap();
            if native {
                assert_eq!(identity.basic_info().sw_ver_str, "002034");
            } else {
                assert!(
                    identity
                        .basic_info()
                        .sw_ver_str
                        .contains("Valetudo unknown")
                );
            }
            assert_eq!(identity.diagnostics().valetudo, "unknown");
            assert_eq!(identity.basic_info().vendor_name, "Vendor");
        }));
        server.join().unwrap();
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;
    fn native(firmware: Option<&str>) -> Identity {
        Identity::new(
            RobotInfo {
                manufacturer: "Roborock".into(),
                model_name: "S5".into(),
            },
            RobotProperties {
                firmware_version: firmware.map(str::to_owned),
                serial_number: Some("api-serial".into()),
            },
            ValetudoVersion {
                release: Some("2026.10.0".into()),
            },
            Some("host-override".into()),
            true,
        )
        .unwrap()
    }
    #[test]
    fn native_identity_is_the_robot_and_versions_are_separate() {
        let id = native(Some("002034"));
        let info = id.basic_info();
        assert_eq!(
            (info.vendor_name, info.product_name, info.serial_no),
            ("Roborock", "S5", "api-serial")
        );
        assert_eq!(info.sw_ver_str, "002034");
        assert_eq!((info.vid, info.pid), (TEST_VID, TEST_PID));
        let diagnostics = serde_json::to_value(id.diagnostics()).unwrap();
        assert_eq!(diagnostics["robot_firmware"], "002034");
        assert_eq!(diagnostics["valetudo"], "2026.10.0");
        assert_eq!(diagnostics["valetudo_matter"], IMPLEMENTATION_VERSION);
    }
    #[test]
    fn native_fallback_preserves_identity_and_numeric_release_order() {
        for firmware in [
            None,
            Some(""),
            Some(" bad"),
            Some("bad\nversion"),
            Some("002034"),
            Some("1.2.3"),
            Some("999999999999999999999999999999"),
        ] {
            let id = native(firmware);
            assert_eq!(id.basic_info().sw_ver, 10_010);
            assert!(id.basic_info().sw_ver > 10_009); // Previously deployed release.
            assert_eq!(id.basic_info().serial_no, "api-serial");
        }
        for firmware in [None, Some(""), Some(" bad"), Some("bad\nversion")] {
            assert_eq!(native(firmware).basic_info().sw_ver_str, "unknown");
        }
        assert_eq!(
            native(Some(&"a".repeat(64))).basic_info().sw_ver_str,
            "a".repeat(64)
        );
        assert_eq!(
            native(Some(&"a".repeat(65))).basic_info().sw_ver_str,
            "unknown"
        );
        assert_eq!(
            native(Some(&"é".repeat(32))).basic_info().sw_ver_str,
            "é".repeat(32)
        );
        assert_eq!(
            native(Some(&"é".repeat(33))).basic_info().sw_ver_str,
            "unknown"
        );
    }
}
