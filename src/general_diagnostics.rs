//! Native telemetry comes exclusively from Valetudo; external nodes describe their own host.
use crate::http::ValetudoClient;
use rs_matter::{
    dm::clusters::gen_diag::{self, ClusterHandler, NetifDiag, NetifInfo},
    dm::{ArrayAttributeRead, AttrChangeNotifier, Cluster, Dataver, InvokeContext, ReadContext},
    error::{Error, ErrorCode},
    tlv::TLVBuilderParent,
    utils::sync::DynBase,
};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq)]
struct Network {
    mac: [u8; 8],
    ipv4: Vec<Ipv4Addr>,
    ipv6: Vec<Ipv6Addr>,
}
impl Network {
    fn parse(value: &serde_json::Value) -> Option<Self> {
        if value["state"].as_str()? != "connected" {
            return None;
        }
        let details = &value["details"];
        let bytes: Vec<_> = details["mac"]
            .as_str()?
            .split(':')
            .map(|part| {
                if part.len() != 2 {
                    None
                } else {
                    u8::from_str_radix(part, 16).ok()
                }
            })
            .collect::<Option<_>>()?;
        if bytes.len() != 6 || bytes == [0; 6] || bytes[0] & 1 != 0 {
            return None;
        }
        let mut mac = [0; 8];
        mac[2..].copy_from_slice(&bytes);
        let mut ipv4 = Vec::new();
        let mut ipv6 = Vec::new();
        for value in details["ips"].as_array()? {
            match value.as_str().and_then(|s| s.parse::<IpAddr>().ok()) {
                Some(IpAddr::V4(ip))
                    if !ip.is_unspecified()
                        && !ip.is_loopback()
                        && !ip.is_multicast()
                        && !ip.is_broadcast() =>
                {
                    if !ipv4.contains(&ip) {
                        ipv4.push(ip);
                    }
                }
                Some(IpAddr::V6(ip))
                    if !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast() =>
                {
                    if !ipv6.contains(&ip) {
                        ipv6.push(ip);
                    }
                }
                _ => (),
            }
        }
        // Never silently publish a truncated interface inventory.
        if ipv4.len() > 4 || ipv6.len() > 8 || (ipv4.is_empty() && ipv6.is_empty()) {
            return None;
        }
        ipv4.sort();
        ipv6.sort();
        Some(Self { mac, ipv4, ipv6 })
    }
}

pub(crate) struct GeneralDiagnostics {
    native: bool,
    transport_ipv4: Ipv4Addr,
    network: Mutex<Option<Network>>,
    host_networks: Mutex<Option<std::collections::BTreeMap<String, HostInterface>>>,
    reboot_count: u16,
    started: Instant,
    dataver: Dataver,
}
impl DynBase for GeneralDiagnostics {}
impl GeneralDiagnostics {
    pub(crate) fn new(native: bool, transport_ipv4: Ipv4Addr, reboot_count: u16) -> Self {
        Self {
            native,
            transport_ipv4,
            network: Mutex::new(None),
            host_networks: Mutex::new(if native {
                None
            } else {
                read_host_interfaces().ok()
            }),
            reboot_count,
            started: Instant::now(),
            dataver: Dataver::new_rand(&mut rand::thread_rng()),
        }
    }
    fn update(&self, network: Option<Network>, notify: &impl AttrChangeNotifier) {
        let network = network.filter(|n| self.native && n.ipv4.contains(&self.transport_ipv4));
        let mut current = self.network.lock().unwrap();
        let changed = *current != network;
        if changed && network.is_some() {
            log::info!(
                "General Diagnostics: Wi-Fi interface and local MAC/IPs ready via Valetudo API"
            );
        }
        *current = network;
        drop(current);
        if changed {
            self.dataver.changed_and_notify_cluster(notify, 0, 0x33);
        }
    }
    pub(crate) async fn monitor(&self, client: &ValetudoClient, notify: &impl AttrChangeNotifier) {
        loop {
            if self.native {
                let wifi = api(
                    client,
                    "/api/v2/robot/capabilities/WifiConfigurationCapability",
                )
                .await;
                self.update(Network::parse(&wifi), notify);
            } else {
                let next = read_host_interfaces().ok();
                let mut old = self.host_networks.lock().unwrap();
                if *old != next {
                    *old = next;
                    drop(old);
                    self.dataver.changed_and_notify_cluster(notify, 0, 0x33);
                }
            }
            smol::Timer::after(Duration::from_secs(15)).await;
        }
    }
    fn uptime_value(&self) -> u64 {
        self.started.elapsed().as_secs()
    }
}
async fn api(client: &ValetudoClient, path: &str) -> serde_json::Value {
    smol::future::or(
        async { client.get(path).await.unwrap_or(serde_json::Value::Null) },
        async {
            smol::Timer::after(Duration::from_secs(6)).await;
            serde_json::Value::Null
        },
    )
    .await
}
impl NetifDiag for GeneralDiagnostics {
    fn netifs(&self, f: &mut dyn FnMut(&NetifInfo) -> Result<(), Error>) -> Result<(), Error> {
        if !self.native {
            let snapshot = self.host_networks.lock().unwrap();
            return host_interfaces(
                snapshot.as_ref().ok_or(ErrorCode::InvalidAction)?,
                self.transport_ipv4,
                f,
            );
        }
        let snapshot = self.network.lock().unwrap().clone();
        let network = snapshot.ok_or(ErrorCode::InvalidAction)?;
        f(&NetifInfo {
            name: "Wi-Fi",
            operational: true,
            offprem_svc_reachable_ipv4: None,
            offprem_svc_reachable_ipv6: None,
            hw_addr: &network.mac,
            ipv4_addrs: &network.ipv4,
            ipv6_addrs: &network.ipv6,
            netif_type: gen_diag::InterfaceTypeEnum::WiFi,
            netif_index: 0,
        })
    }
}
// Host inventory is used only when the Matter application runs off-robot.
// Never infer Wi-Fi technology from interface names, nor borrow the robot's MAC.
#[derive(Default, Debug, PartialEq)]
struct HostInterface {
    mac: Option<[u8; 8]>,
    ipv4: Vec<Ipv4Addr>,
    ipv6: Vec<Ipv6Addr>,
    index: u32,
    up: bool,
    non_ieee: bool,
}
fn collect_host_interfaces(
    interfaces: impl IntoIterator<Item = getifaddrs::Interface>,
) -> Result<std::collections::BTreeMap<String, HostInterface>, Error> {
    use getifaddrs::{Address, InterfaceFlags};
    let mut result = std::collections::BTreeMap::<String, HostInterface>::new();
    for interface in interfaces {
        if interface.name.len() > 32 {
            return Err(ErrorCode::ConstraintError.into());
        }
        let entry = result.entry(interface.name).or_default();
        entry.index = interface.index.unwrap_or(0);
        entry.up |= interface.flags.contains(InterfaceFlags::UP);
        entry.non_ieee |= interface
            .flags
            .intersects(InterfaceFlags::LOOPBACK | InterfaceFlags::POINTTOPOINT);
        match interface.address {
            Address::Mac(value) => {
                let mut padded = [0; 8];
                padded[2..].copy_from_slice(&value);
                entry.mac = Some(padded);
            }
            Address::V4(value)
                if !value.address.is_unspecified() && !value.address.is_multicast() =>
            {
                if !entry.ipv4.contains(&value.address) {
                    entry.ipv4.push(value.address);
                }
            }
            Address::V6(value)
                if !value.address.is_unspecified() && !value.address.is_multicast() =>
            {
                if !entry.ipv6.contains(&value.address) {
                    entry.ipv6.push(value.address);
                }
            }
            _ => (),
        }
    }
    for entry in result.values_mut() {
        entry.ipv4.sort();
        entry.ipv6.sort();
        if entry.mac.is_none() && entry.non_ieee {
            // Core 11.12.5.6 assigns link-address semantics only to IEEE 802
            // interfaces. For OS-verified loopback/point-to-point interfaces,
            // use a zero-valued hwadr with Type Unspecified as a non-IEEE
            // representation policy, not a guessed or CSA-defined unknown MAC.
            entry.mac = Some([0; 8]);
        }
        if entry.mac.is_none() || entry.ipv4.len() > 4 || entry.ipv6.len() > 8 {
            // No invented address or silently truncated mandatory inventory.
            return Err(ErrorCode::InvalidAction.into());
        }
    }
    Ok(result)
}
fn read_host_interfaces() -> Result<std::collections::BTreeMap<String, HostInterface>, Error> {
    collect_host_interfaces(getifaddrs::getifaddrs().map_err(|_| ErrorCode::InvalidAction)?)
}
fn host_interfaces(
    interfaces: &std::collections::BTreeMap<String, HostInterface>,
    transport: Ipv4Addr,
    callback: &mut dyn FnMut(&NetifInfo) -> Result<(), Error>,
) -> Result<(), Error> {
    for (name, interface) in interfaces {
        callback(&NetifInfo {
            name,
            operational: interface.up && interface.ipv4.contains(&transport),
            offprem_svc_reachable_ipv4: None,
            offprem_svc_reachable_ipv6: None,
            hw_addr: &interface.mac.unwrap(),
            ipv4_addrs: &interface.ipv4,
            ipv6_addrs: &interface.ipv6,
            netif_type: gen_diag::InterfaceTypeEnum::Unspecified,
            netif_index: interface.index,
        })?;
    }
    Ok(())
}

impl ClusterHandler for &GeneralDiagnostics {
    const CLUSTER: Cluster<'static> = gen_diag::GenDiagHandler::CLUSTER
        .with_attrs(rs_matter::with!(required; gen_diag::AttributeId::UpTime))
        .with_cmds(rs_matter::with!(
            gen_diag::CommandId::TestEventTrigger | gen_diag::CommandId::TimeSnapshot
        ))
        .with_events(rs_matter::with!(gen_diag::EventId::BootReason));
    fn dataver(&self) -> u32 {
        self.dataver.get()
    }
    fn dataver_changed(&self) {
        self.dataver.changed();
    }
    fn network_interfaces<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: ArrayAttributeRead<
            gen_diag::NetworkInterfaceArrayBuilder<P>,
            gen_diag::NetworkInterfaceBuilder<P>,
        >,
    ) -> Result<P, Error> {
        gen_diag::GenDiagHandler::new(Dataver::new(0), &(), *self).network_interfaces(ctx, builder)
    }
    // Persistent application-node count, independent of the remote vacuum host.
    fn reboot_count(&self, _: impl ReadContext) -> Result<u16, Error> {
        Ok(self.reboot_count)
    }
    fn up_time(&self, _: impl ReadContext) -> Result<u64, Error> {
        Ok(self.uptime_value())
    }
    fn test_event_triggers_enabled(&self, _: impl ReadContext) -> Result<bool, Error> {
        Ok(false)
    }
    fn handle_test_event_trigger(
        &self,
        _: impl InvokeContext,
        _: gen_diag::TestEventTriggerRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::ConstraintError.into())
    }
    fn handle_time_snapshot<P: TLVBuilderParent>(
        &self,
        _: impl InvokeContext,
        response: gen_diag::TimeSnapshotResponseBuilder<P>,
    ) -> Result<P, Error> {
        // Same monotonic application lifetime as UpTime. No synchronized wall
        // clock is promised by this node; do not turn the OS clock into one.
        response
            .system_time_ms(self.started.elapsed().as_millis() as u64)?
            .posix_time_ms(rs_matter::tlv::Nullable::none())?
            .end()
    }
    fn handle_payload_test_request<P: TLVBuilderParent>(
        &self,
        _: impl InvokeContext,
        _: gen_diag::PayloadTestRequestRequest<'_>,
        _: gen_diag::PayloadTestResponseBuilder<P>,
    ) -> Result<P, Error> {
        Err(ErrorCode::InvalidAction.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn status() -> serde_json::Value {
        serde_json::json!({"state":"connected","details":{"mac":"02:aa:bb:cc:dd:ee","bssid":"02:11:22:33:44:55","ips":["192.0.2.1","fe80::1","2001:db8::1"]}})
    }
    #[test]
    fn mac_is_local_padded_and_addresses_are_binary() {
        let parsed = Network::parse(&status()).unwrap();
        assert_eq!(parsed.mac, [0, 0, 2, 170, 187, 204, 221, 238]);
        assert_eq!(parsed.ipv4, ["192.0.2.1".parse::<Ipv4Addr>().unwrap()]);
        assert_eq!(parsed.ipv6.len(), 2);
    }
    #[test]
    fn old_api_and_missing_capability_do_not_invent_an_interface() {
        for value in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"state":"connected","details":{"bssid":"02:11:22:33:44:55","ips":["192.0.2.1"]}}),
        ] {
            assert_eq!(Network::parse(&value), None);
        }
        for mac in ["00:00:00:00:00:00", "ff:ff:ff:ff:ff:ff", "bad"] {
            let mut value = status();
            value["details"]["mac"] = mac.into();
            assert_eq!(Network::parse(&value), None);
        }
    }
    #[test]
    fn invalid_ips_filtered_deduplicated_and_disconnection_invalidates() {
        let mut value = status();
        value["details"]["ips"] = serde_json::json!([
            "192.0.2.1",
            "192.0.2.1",
            "127.0.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "ff02::1",
            "::1",
            "::",
            "garbage"
        ]);
        let network = Network::parse(&value).unwrap();
        assert_eq!(network.ipv4.len(), 1);
        assert!(network.ipv6.is_empty());
        value["state"] = "not_connected".into();
        assert_eq!(Network::parse(&value), None);
    }
}

#[cfg(test)]
mod provider_tests {
    use super::*;
    #[derive(Default)]
    struct Notify(std::cell::Cell<usize>);
    impl AttrChangeNotifier for Notify {
        fn notify_attr_changed(&self, _: u16, _: u32, _: u32) {
            panic!("unexpected");
        }
        fn notify_cluster_changed(&self, ep: u16, cl: u32) {
            assert_eq!((ep, cl), (0, 0x33));
            self.0.set(self.0.get() + 1);
        }
        fn notify_endpoint_changed(&self, _: u16) {
            panic!("unexpected");
        }
        fn notify_all_changed(&self) {
            panic!("unexpected");
        }
    }
    fn network() -> Network {
        Network::parse(&serde_json::json!({"state":"connected","details":{"mac":"02:aa:bb:cc:dd:ee","ips":["192.0.2.1","fe80::1"]}})).unwrap()
    }
    #[test]
    fn provider_reads_api_data_and_invalidates_on_failure() {
        let diag = GeneralDiagnostics::new(true, "192.0.2.1".parse().unwrap(), 7);
        let notify = Notify::default();
        let before = diag.dataver.get();
        diag.update(Some(network()), &notify);
        diag.netifs(&mut |n| {
            assert_eq!(n.netif_type, gen_diag::InterfaceTypeEnum::WiFi);
            assert_eq!(n.hw_addr, &[0, 0, 2, 170, 187, 204, 221, 238]);
            assert_eq!(n.ipv6_addrs.len(), 1);
            assert_eq!(n.offprem_svc_reachable_ipv4, None);
            Ok(())
        })
        .unwrap();
        assert!(diag.uptime_value() < 5);
        diag.update(Some(network()), &notify);
        assert_eq!(notify.0.get(), 1);
        diag.update(None, &notify);
        assert_eq!(notify.0.get(), 2);
        assert_eq!(diag.dataver.get(), before.wrapping_add(2));
        assert!(diag.netifs(&mut |_| panic!("stale interface")).is_err());
        assert!(diag.uptime_value() < 5);
    }
    #[test]
    fn transport_mismatch_never_reports_robot_interface_as_local() {
        {
            let diag = GeneralDiagnostics::new(true, "198.51.100.1".parse().unwrap(), 7);
            diag.update(Some(network()), &Notify::default());
            assert!(
                diag.netifs(&mut |_| panic!("wrong node interface"))
                    .is_err()
            );
        }
    }
    #[test]
    fn lifetime_survives_missing_api_telemetry() {
        let diag = GeneralDiagnostics::new(true, "192.0.2.1".parse().unwrap(), 7);
        assert_eq!(diag.reboot_count, 7);
        assert!(diag.uptime_value() < 5);
        diag.update(None, &Notify::default());
        assert_eq!(diag.reboot_count, 7);
        assert!(diag.uptime_value() < 5);
    }
    #[test]
    fn optional_api_errors_and_malformed_json_do_not_fail_the_provider() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (code, body) in [
                (404, "{}"),
                (501, "{}"),
                (200, "bad json"),
                (200, "{\"uptime\":123}"),
            ] {
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
            let client = ValetudoClient::new(ex, format!("http://{addr}").parse().unwrap());
            for _ in 0..3 {
                assert_eq!(
                    api(&client, "/api/v2/system/host/info").await,
                    serde_json::Value::Null
                );
            }
            assert_eq!(
                api(&client, "/api/v2/system/host/info").await["uptime"],
                123
            );
        }));
        server.join().unwrap();
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;
    use getifaddrs::{Address, Interface, InterfaceFlags, NetworkAddress};
    fn interface(name: &str, address: Address) -> Interface {
        Interface {
            name: name.into(),
            address,
            flags: InterfaceFlags::UP,
            index: Some(3),
            #[cfg(windows)]
            description: String::new(),
        }
    }
    fn v4(ip: &str) -> Address {
        Address::V4(NetworkAddress {
            address: ip.parse().unwrap(),
            netmask: None,
            associated_address: None,
        })
    }
    #[test]
    fn host_inventory_groups_addresses_and_only_transport_is_operational() {
        let inventory = collect_host_interfaces([
            interface("en0", Address::Mac([2, 1, 2, 3, 4, 5])),
            interface("en0", v4("192.0.2.4")),
            interface("en0", v4("192.0.2.4")),
            interface("en1", Address::Mac([2, 1, 2, 3, 4, 6])),
            interface("en1", v4("198.51.100.4")),
        ])
        .unwrap();
        assert_eq!(inventory.len(), 2);
        assert_eq!(inventory["en0"].ipv4.len(), 1);
        let mut seen = 0;
        host_interfaces(&inventory, "192.0.2.4".parse().unwrap(), &mut |n| {
            assert_eq!(n.netif_type, gen_diag::InterfaceTypeEnum::Unspecified);
            assert_eq!(n.operational, n.name == "en0");
            assert_eq!(n.hw_addr.len(), 8);
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, 2);
    }
    #[test]
    fn missing_link_address_and_oversized_lists_are_not_fabricated_or_truncated() {
        assert!(collect_host_interfaces([interface("tun0", v4("192.0.2.4"))]).is_err());
        let mut rows = vec![interface("en0", Address::Mac([2, 1, 2, 3, 4, 5]))];
        for i in 1..=5 {
            rows.push(interface("en0", v4(&format!("192.0.2.{i}"))));
        }
        assert!(collect_host_interfaces(rows).is_err());
    }
    #[test]
    fn verified_non_ieee_interfaces_do_not_poison_physical_inventory() {
        let mut loopback = interface("lo0", v4("127.0.0.1"));
        loopback.flags |= InterfaceFlags::LOOPBACK;
        let mut tunnel = interface("utun0", v4("192.0.2.8"));
        tunnel.flags |= InterfaceFlags::POINTTOPOINT;
        let inventory = collect_host_interfaces([
            interface("en0", Address::Mac([2, 1, 2, 3, 4, 5])),
            interface("en0", v4("192.0.2.4")),
            loopback,
            tunnel,
        ])
        .unwrap();
        assert_eq!(inventory.len(), 3);
        for name in ["lo0", "utun0"] {
            assert_eq!(inventory[name].mac, Some([0; 8]));
            assert!(inventory[name].non_ieee);
        }
        assert_eq!(inventory["en0"].mac, Some([0, 0, 2, 1, 2, 3, 4, 5]));
        assert!(!inventory["en0"].non_ieee);
        // An interface name alone is never evidence for a non-IEEE address.
        assert!(collect_host_interfaces([interface("utun0", v4("192.0.2.8"))]).is_err());
    }
    #[test]
    fn remote_provider_ignores_vacuum_network_and_reports_host_snapshot() {
        let diag = GeneralDiagnostics::new(false, "192.0.2.4".parse().unwrap(), 9);
        let host = collect_host_interfaces([
            interface("host0", Address::Mac([2, 1, 2, 3, 4, 5])),
            interface("host0", v4("192.0.2.4")),
        ])
        .unwrap();
        *diag.host_networks.lock().unwrap() = Some(host);
        diag.netifs(&mut |n| {
            assert_eq!(n.name, "host0");
            Ok(())
        })
        .unwrap();
        assert_eq!(diag.reboot_count, 9);
    }
}
