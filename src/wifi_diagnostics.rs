//! Matter diagnostics sourced exclusively from the Valetudo HTTP API.
use crate::http::ValetudoClient;
use rs_matter::{
    dm::clusters::wifi_diag::{self, ClusterHandler, SecurityTypeEnum, WiFiVersionEnum},
    dm::{AttrChangeNotifier, Cluster, Dataver, InvokeContext, ReadContext},
    error::{Error, ErrorCode},
    tlv::{Nullable, NullableBuilder, OctetsBuilder, TLVBuilderParent},
    utils::sync::DynBase,
};
use std::{sync::Mutex, time::Duration};

#[derive(Clone, Debug, Default, PartialEq)]
struct Snapshot {
    bssid: Option<[u8; 6]>,
    rssi: Option<i8>,
}

impl Snapshot {
    fn parse(status: &serde_json::Value) -> Self {
        if status["state"].as_str() != Some("connected") {
            return Self::default();
        }
        let details = &status["details"];
        let mac = |s: &str| -> Option<[u8; 6]> {
            let parts = s.split(':').collect::<Vec<_>>();
            if parts.len() != 6 || parts.iter().any(|p| p.len() != 2) {
                return None;
            }
            let bytes = parts
                .iter()
                .map(|p| u8::from_str_radix(p, 16))
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            let bytes: [u8; 6] = bytes.try_into().ok()?;
            (bytes != [0; 6] && bytes[0] & 1 == 0).then_some(bytes)
        };
        let bssid = details["bssid"].as_str().and_then(mac);
        let rssi = details["signal"]
            .as_i64()
            .filter(|v| (-120..=0).contains(v))
            .map(|v| v as i8);
        Self { bssid, rssi }
    }
}

pub(crate) struct WifiDiagnostics {
    snapshot: Mutex<Snapshot>,
    dataver: Dataver,
}
impl DynBase for WifiDiagnostics {}
impl WifiDiagnostics {
    pub(crate) fn new() -> Self {
        Self {
            snapshot: Mutex::new(Snapshot::default()),
            dataver: Dataver::new_rand(&mut rand::thread_rng()),
        }
    }
    fn update(&self, next: Snapshot, notify: &impl AttrChangeNotifier) {
        let mut current = self.snapshot.lock().unwrap();
        if *current != next {
            if current.bssid.is_none() && next.bssid.is_some() {
                log::info!(
                    "Wi-Fi diagnostics ready via Valetudo API: BSSID available, rssi={:?}",
                    next.rssi
                );
            }
            *current = next;
            drop(current);
            self.dataver
                .changed_and_notify_cluster(notify, 0, <&Self>::CLUSTER.id);
        }
    }
    pub(crate) async fn monitor(&self, client: &ValetudoClient, notify: &impl AttrChangeNotifier) {
        loop {
            let next = read_api(client).await;
            self.update(next, notify);
            smol::Timer::after(Duration::from_secs(15)).await;
        }
    }
}
async fn read_api(client: &ValetudoClient) -> Snapshot {
    smol::future::or(
        async {
            client
                .get::<serde_json::Value>("/api/v2/robot/capabilities/WifiConfigurationCapability")
                .await
                .map(|value| Snapshot::parse(&value))
                .unwrap_or_default()
        },
        async {
            smol::Timer::after(Duration::from_secs(6)).await;
            Snapshot::default()
        },
    )
    .await
}
impl wifi_diag::WirelessDiag for WifiDiagnostics {
    fn connected(&self) -> Result<bool, Error> {
        Ok(self.snapshot.lock().unwrap().bssid.is_some())
    }
}
impl wifi_diag::WifiDiag for WifiDiagnostics {
    fn bssid(&self, f: &mut dyn FnMut(Option<&[u8]>) -> Result<(), Error>) -> Result<(), Error> {
        let bssid = self.snapshot.lock().unwrap().bssid;
        f(bssid.as_ref().map(|b| b.as_slice()))
    }
}
impl ClusterHandler for &WifiDiagnostics {
    const CLUSTER: Cluster<'static> =
        wifi_diag::WifiDiagHandler::CLUSTER.with_events(rs_matter::with!());
    fn dataver(&self) -> u32 {
        self.dataver.get()
    }
    fn dataver_changed(&self) {
        self.dataver.changed();
    }
    fn bssid<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: NullableBuilder<P, OctetsBuilder<P>>,
    ) -> Result<P, Error> {
        wifi_diag::WifiDiagHandler::new(Dataver::new(0), *self).bssid(ctx, builder)
    }
    fn security_type(&self, _: impl ReadContext) -> Result<Nullable<SecurityTypeEnum>, Error> {
        Ok(Nullable::none())
    }
    fn wi_fi_version(&self, _: impl ReadContext) -> Result<Nullable<WiFiVersionEnum>, Error> {
        Ok(Nullable::none()) // Negotiated PHY is not reported by this driver; 72.2 Mb/s is not proof.
    }
    fn channel_number(&self, _: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::none())
    }
    fn rssi(&self, _: impl ReadContext) -> Result<Nullable<i8>, Error> {
        Ok(self.snapshot.lock().unwrap().rssi.into())
    }
    fn handle_reset_counts(&self, _: impl InvokeContext) -> Result<(), Error> {
        Err(ErrorCode::InvalidAction.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn status() -> serde_json::Value {
        serde_json::json!({"state":"connected","details":{"bssid":"02:11:22:33:44:55","signal":-39,"channel":1,"security":"wpa2"}})
    }
    #[derive(Default)]
    struct Notify(std::cell::Cell<u32>);
    impl AttrChangeNotifier for Notify {
        fn notify_attr_changed(&self, _: u16, _: u32, _: u32) {
            panic!("unexpected attribute notification")
        }
        fn notify_cluster_changed(&self, ep: u16, cluster: u32) {
            assert_eq!((ep, cluster), (0, 0x36));
            self.0.set(self.0.get() + 1);
        }
        fn notify_endpoint_changed(&self, _: u16) {
            panic!("unexpected endpoint notification")
        }
        fn notify_all_changed(&self) {
            panic!("unexpected global notification")
        }
    }
    #[test]
    fn changes_and_failures_invalidate_data_version_and_notify_subscribers() {
        let wifi = WifiDiagnostics::new();
        let notify = Notify::default();
        let before = wifi.dataver.get();
        let sample = Snapshot::parse(&status());
        wifi.update(sample.clone(), &notify);
        assert_eq!(wifi.dataver.get(), before.wrapping_add(1));
        wifi.update(sample, &notify);
        assert_eq!(notify.0.get(), 1);
        wifi.update(Snapshot::default(), &notify);
        assert_eq!(notify.0.get(), 2);
        assert_eq!(wifi.dataver.get(), before.wrapping_add(2));
        assert_eq!(*wifi.snapshot.lock().unwrap(), Snapshot::default());
    }
    #[test]
    fn api_values_map_to_matter() {
        let s = Snapshot::parse(&status());
        assert_eq!(s.bssid, Some([2, 17, 34, 51, 68, 85]));
        assert_eq!(s.rssi, Some(-39));
    }
    #[test]
    fn disconnected_unknown_and_old_api_are_supported() {
        for state in ["not_connected", "unknown"] {
            let mut value = status();
            value["state"] = state.into();
            assert_eq!(Snapshot::parse(&value), Snapshot::default());
        }
        let old = serde_json::json!({"state":"connected","details":{"signal":-55}});
        let s = Snapshot::parse(&old);
        assert_eq!(s.rssi, Some(-55));
    }
    #[test]
    fn unverified_extra_api_fields_are_ignored() {
        let mut extended = status();
        extended["details"]["channel"] = 11.into();
        extended["details"]["security"] = "wpa3".into();
        extended["details"]["wifiVersion"] = "ax".into();
        assert_eq!(Snapshot::parse(&extended), Snapshot::parse(&status()));
    }
    #[test]
    fn rssi_respects_matter_1_6_1_range() {
        for (value, expected) in [
            (-128, None),
            (-127, None),
            (-121, None),
            (-120, Some(-120)),
            (0, Some(0)),
            (1, None),
        ] {
            let mut input = status();
            input["details"]["signal"] = value.into();
            assert_eq!(Snapshot::parse(&input).rssi, expected);
        }
    }
    #[test]
    fn malformed_fields_are_not_invented() {
        let value = serde_json::json!({"state":"connected","details":{"bssid":"ff:ff:ff:ff:ff:ff","signal":256,"channel":0,"security":"future"}});
        assert_eq!(Snapshot::parse(&value), Snapshot::default());
    }
    #[test]
    fn real_http_path_and_http_failure_clear_diagnostics() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for code in [200, 500] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap();
                let mut fields = request.lines().next().unwrap().split_whitespace();
                assert_eq!(fields.next(), Some("GET"));
                let uri: hyper::Uri = fields.next().unwrap().parse().unwrap();
                assert_eq!(
                    uri.path(),
                    "/api/v2/robot/capabilities/WifiConfigurationCapability"
                );
                let body = status().to_string();
                write!(
                    stream,
                    "HTTP/1.1 {code} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let ex = Box::leak(Box::new(smol::LocalExecutor::new()));
        smol::block_on(ex.run(async {
            let client = ValetudoClient::new(ex, format!("http://{address}").parse().unwrap());
            assert_eq!(read_api(&client).await, Snapshot::parse(&status()));
            assert_eq!(read_api(&client).await, Snapshot::default());
        }));
        server.join().unwrap();
    }
}
