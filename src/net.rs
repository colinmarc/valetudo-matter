use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, UdpSocket},
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
}

impl Netif {
    pub(crate) async fn pick() -> Result<Self, io::Error> {
        let mut interfaces = getifaddrs()?.filter(interface_suitable).collect::<Vec<_>>();

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

                Some(Netif {
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

        if matches.count() > 0 {
            warn!(
                "multiple suitable network interfaces found, picking {} arbitrarily",
                res.name
            );
        }

        Ok(res)
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
        socket.set_reuse_port(true)?;
        socket.set_only_v6(false)?;
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

fn interface_suitable(interface: &Interface) -> bool {
    if interface.flags.contains(InterfaceFlags::LOOPBACK)
        || interface.flags.contains(InterfaceFlags::POINTTOPOINT)
    {
        return false;
    }

    interface.flags.contains(InterfaceFlags::UP)
        && interface.flags.contains(InterfaceFlags::BROADCAST)
}
