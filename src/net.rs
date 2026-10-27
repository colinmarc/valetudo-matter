use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, UdpSocket},
};

use anyhow::anyhow;
use getifaddrs::{Address, Interface, InterfaceFlags, getifaddrs};
use log::{debug, warn};
use rs_matter::transport::network::mdns::{
    MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_SOCKET_DEFAULT_BIND_ADDR,
};

#[derive(Clone)]
pub(crate) struct Netif {
    pub(crate) ipv4_addr: Ipv4Addr,
    pub(crate) ipv6_addr: Ipv6Addr,
    pub(crate) name: String,
    pub(crate) index: Option<u32>,
    native: bool,
}

impl Netif {
    pub(crate) async fn pick() -> Result<Self, io::Error> {
        Self::select(getifaddrs()?.collect(), None)
    }

    pub(crate) async fn from_valetudo(
        client: &crate::http::ValetudoClient,
    ) -> anyhow::Result<Self> {
        let status: serde_json::Value = client
            .get("/api/v2/robot/capabilities/WifiConfigurationCapability")
            .await?;
        let ips = api_addresses(&status)?;
        let iface = Self::select(getifaddrs()?.collect(), Some(&ips))?;
        log::info!(
            "Native Matter interface selected from Valetudo API: {}",
            iface.name
        );
        Ok(iface)
    }

    fn select(interfaces: Vec<Interface>, expected: Option<&[IpAddr]>) -> io::Result<Self> {
        let mut interfaces = interfaces
            .into_iter()
            .filter(interface_suitable)
            .collect::<Vec<_>>();

        // Prioritize link-local ipv6 addresses.
        interfaces.sort_by_key(|iface| match &iface.address {
            Address::V6(addr) if addr.address.is_unicast_link_local() => 0,
            _ => 1,
        });

        let ipv4_interfaces = interfaces.iter().filter(|iface| iface.address.is_ipv4());

        // Search for an interface that has both an ipv4 and ipv6 address.
        // These are separate in the list but have the same name/index.
        let mut matches = ipv4_interfaces.filter_map(|ipv4| {
            interfaces.iter().find_map(|ipv6| {
                if ipv6.index != ipv4.index || ipv6.name != ipv4.name {
                    return None;
                }

                let ipv4_addr = match &ipv4.address {
                    Address::V4(v) => v.address,
                    _ => unreachable!(),
                };

                let ipv6_addr = match &ipv6.address {
                    Address::V6(v) => v.address,
                    _ => return None,
                };

                if expected.is_some_and(|ips| !ips.contains(&IpAddr::V4(ipv4_addr))) {
                    return None;
                }
                Some(Netif {
                    native: expected.is_some(),
                    ipv4_addr,
                    ipv6_addr,
                    name: ipv4.name.clone(),
                    index: ipv4.index,
                })
            })
        });

        let Some(res) = matches.next() else {
            return Err(io::Error::new(
                io::ErrorKind::NetworkUnreachable,
                anyhow!("No suitable network interface found"),
            ));
        };

        if matches.any(|other| other.name != res.name || other.index != res.index) {
            if expected.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Valetudo address matches multiple interfaces",
                ));
            }
            warn!(
                "multiple suitable network interfaces found, picking {} arbitrarily",
                res.name
            );
        }

        Ok(res)
    }

    fn restrict_socket(&self, socket: &socket2::Socket) -> io::Result<()> {
        if !self.native {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            socket.bind_device(Some(self.name.as_bytes()))?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = socket;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Native interface binding requires Linux",
            ))
        }
        #[cfg(target_os = "linux")]
        {
            Ok(())
        }
    }

    pub(crate) fn bind_matter_socket(&self) -> io::Result<UdpSocket> {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        socket.set_only_v6(false)?;
        self.restrict_socket(&socket)?;
        socket.bind(&rs_matter::transport::MATTER_SOCKET_BIND_ADDR.into())?;
        if self.native {
            log::info!(
                "Matter UDP and mDNS restricted to Valetudo interface {}",
                self.name
            );
        }
        Ok(socket.into())
    }

    pub(crate) fn bind_mdns_socket(&self) -> io::Result<UdpSocket> {
        debug!(
            "binding mdns socket on interface {:?} (index: {:?})",
            self.name, self.index
        );

        let socket = socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;

        socket.set_reuse_address(true)?;
        allow_unsupported_reuse_port(socket.set_reuse_port(true))?;
        socket.set_only_v6(false)?;
        self.restrict_socket(&socket)?;
        socket.bind(&MDNS_SOCKET_DEFAULT_BIND_ADDR.into())?;

        debug!("joining mdns for {}", self.ipv6_addr);
        socket.join_multicast_v6(&MDNS_IPV6_BROADCAST_ADDR, self.index.unwrap_or_default())?;
        socket.set_multicast_if_v6(self.index.unwrap_or_default())?;

        // Dualstack mDNS is weird and only supported on linux.
        if cfg!(target_os = "linux") {
            debug!("joining mdns for {}", self.ipv4_addr);
            socket.join_multicast_v4(&MDNS_IPV4_BROADCAST_ADDR, &self.ipv4_addr)?;
            socket.set_multicast_if_v4(&self.ipv4_addr)?;
        }

        Ok(socket.into())
    }
}

fn api_addresses(status: &serde_json::Value) -> anyhow::Result<Vec<IpAddr>> {
    if status["state"].as_str() != Some("connected") {
        anyhow::bail!("Valetudo Wi-Fi is not connected");
    }
    let ips: Vec<IpAddr> = status["details"]["ips"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str()?.parse::<IpAddr>().ok())
        .filter(|ip| !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast())
        .collect();
    if !ips.iter().any(IpAddr::is_ipv4) {
        anyhow::bail!("Valetudo has no usable local IPv4 address");
    }
    Ok(ips)
}

fn interface_suitable(interface: &Interface) -> bool {
    if interface.flags.contains(InterfaceFlags::LOOPBACK)
        || interface.flags.contains(InterfaceFlags::POINTTOPOINT)
    {
        return false;
    }

    interface.flags.contains(InterfaceFlags::UP)
        && interface.flags.contains(InterfaceFlags::BROADCAST)
}

// SO_REUSEPORT is unavailable on older Linux kernels. SO_REUSEADDR is
// already enabled by the caller; only an unsupported option is recoverable.
fn allow_unsupported_reuse_port(result: io::Result<()>) -> io::Result<()> {
    match result {
        #[cfg(target_os = "linux")]
        Err(error) if error.raw_os_error() == Some(libc::ENOPROTOOPT) => {
            debug!("SO_REUSEPORT unavailable; using SO_REUSEADDR for mDNS");
            Ok(())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_reuse_port_is_recoverable_only_on_linux() {
        let result =
            allow_unsupported_reuse_port(Err(io::Error::from_raw_os_error(libc::ENOPROTOOPT)));
        if cfg!(target_os = "linux") {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ENOPROTOOPT));
        }
    }

    #[test]
    fn reuse_port_preserves_other_errors_and_success() {
        assert!(allow_unsupported_reuse_port(Ok(())).is_ok());
        for errno in [libc::EACCES, libc::EINVAL, libc::EBADF] {
            let error =
                allow_unsupported_reuse_port(Err(io::Error::from_raw_os_error(errno))).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(errno));
        }
        let error = allow_unsupported_reuse_port(Err(io::Error::other("synthetic"))).unwrap_err();
        assert_eq!(error.to_string(), "synthetic");
    }
}

#[cfg(test)]
mod native_binding_tests {
    use super::*;
    fn interfaces(name: &str, index: u32, ip: &str) -> Vec<Interface> {
        let flags = InterfaceFlags::UP | InterfaceFlags::BROADCAST;
        vec![
            Interface {
                name: name.into(),
                index: Some(index),
                flags,
                address: Address::V4(getifaddrs::NetworkAddress {
                    address: ip.parse().unwrap(),
                    netmask: None,
                    associated_address: None,
                }),
            },
            Interface {
                name: name.into(),
                index: Some(index),
                flags,
                address: Address::V6(getifaddrs::NetworkAddress {
                    address: format!("fe80::{index}").parse().unwrap(),
                    netmask: None,
                    associated_address: None,
                }),
            },
        ]
    }
    #[test]
    fn native_selection_uses_api_not_os_order_and_rejects_ambiguity() {
        let mut all = interfaces("private0", 1, "192.0.2.1");
        all.extend(interfaces("wlan0", 2, "192.0.2.2"));
        let ips = api_addresses(
            &serde_json::json!({"state":"connected","details":{"ips":["192.0.2.2"]}}),
        )
        .unwrap();
        let selected = Netif::select(all.clone(), Some(&ips)).unwrap();
        assert_eq!(selected.name, "wlan0");
        assert!(selected.native);
        assert_eq!(selected.index, Some(2));
        assert!(Netif::select(all.clone(), Some(&["192.0.2.99".parse().unwrap()])).is_err());
        all.extend(interfaces("other0", 3, "192.0.2.2"));
        assert!(Netif::select(all, Some(&ips)).is_err());
    }
    #[test]
    fn unavailable_native_api_never_falls_back_to_arbitrary_host_interface() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"state":"not_connected","details":{"ips":["192.0.2.1"]}}),
            serde_json::json!({"state":"connected","details":{"ips":["127.0.0.1","0.0.0.0","bad"]}}),
        ] {
            assert!(api_addresses(&value).is_err());
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_binding_validation {
    use super::*;
    #[test]
    #[ignore = "requires explicit native Linux interface and bind-device permission"]
    fn native_socket_rejects_loopback_ingress() {
        let name =
            std::env::var("VALETUDO_MATTER_TEST_INTERFACE").expect("explicit interface required");
        assert_ne!(name, "lo");
        let iface = Netif {
            name,
            index: None,
            native: true,
            ipv4_addr: Ipv4Addr::UNSPECIFIED,
            ipv6_addr: Ipv6Addr::UNSPECIFIED,
        };
        let make = || {
            let s = socket2::Socket::new(
                socket2::Domain::IPV6,
                socket2::Type::DGRAM,
                Some(socket2::Protocol::UDP),
            )
            .unwrap();
            s.set_only_v6(false).unwrap();
            s
        };
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        // Control proves loopback delivery works without the device restriction.
        let control = make();
        control
            .bind(&"[::]:0".parse::<std::net::SocketAddr>().unwrap().into())
            .unwrap();
        let control: UdpSocket = control.into();
        control
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        sender
            .send_to(
                b"control",
                ("127.0.0.1", control.local_addr().unwrap().port()),
            )
            .unwrap();
        let mut buf = [0; 32];
        assert_eq!(control.recv_from(&mut buf).unwrap().0, 7);
        let restricted = make();
        iface.restrict_socket(&restricted).unwrap();
        restricted
            .bind(&"[::]:0".parse::<std::net::SocketAddr>().unwrap().into())
            .unwrap();
        let restricted: UdpSocket = restricted.into();
        restricted
            .set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .unwrap();
        sender
            .send_to(
                b"blocked",
                ("127.0.0.1", restricted.local_addr().unwrap().port()),
            )
            .unwrap();
        let e = restricted.recv_from(&mut buf).unwrap_err();
        assert!(matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
    }
}
