use std::{
    collections::BTreeMap,
    io,
    net::{Ipv4Addr, Ipv6Addr, UdpSocket},
};

use anyhow::anyhow;
use getifaddrs::{Address, Interface, InterfaceFlags, getifaddrs};
use log::{debug, warn};
use rs_matter::{
    dm::clusters::gen_diag::{InterfaceTypeEnum, NetifDiag, NetifInfo},
    error::Error,
    transport::network::mdns::{
        MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_SOCKET_DEFAULT_BIND_ADDR,
    },
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

#[derive(Debug, Copy, Clone)]
pub(crate) struct GetifaddrsDiag;

impl NetifDiag for GetifaddrsDiag {
    fn netifs(
        &self,
        f: &mut dyn FnMut(&NetifInfo) -> Result<(), rs_matter::error::Error>,
    ) -> Result<(), rs_matter::error::Error> {
        let mut ifaces = BTreeMap::new();
        for ia in getifaddrs()? {
            let Some(index) = ia.index else {
                continue;
            };

            let iface: &mut UnixNetif = ifaces.entry(index).or_default();
            iface.load(ia, index)?;
        }

        for (_, iface) in ifaces {
            f(&iface.to_netif_info())?;
        }

        Ok(())
    }
}

// The below types are copied from rs-matter (I don't want to enable the 'os'
// feature flag.)

/// A type for representing one network interface
#[derive(Clone, Debug, Default)]
pub struct UnixNetif {
    /// Interface name
    pub name: String,
    /// Hardware address
    pub hw_addr: [u8; 8],
    /// IPv4 addresses
    pub ipv4addrs: Vec<Ipv4Addr>,
    /// IPv6 addresses
    pub ipv6addrs: Vec<Ipv6Addr>,
    /// Operational status
    pub operational: bool,
    /// Interface index
    pub netif_index: u32,
}

impl UnixNetif {
    /// Convert to `NetifInfo`
    pub fn to_netif_info(&self) -> NetifInfo<'_> {
        NetifInfo {
            name: &self.name,
            operational: self.operational,
            offprem_svc_reachable_ipv4: None,
            offprem_svc_reachable_ipv6: None,
            hw_addr: &self.hw_addr,
            ipv4_addrs: &self.ipv4addrs,
            ipv6_addrs: &self.ipv6addrs,
            netif_type: InterfaceTypeEnum::Unspecified, // TODO
            netif_index: self.netif_index,
        }
    }

    /// Augment the information of the network interface with
    /// the provided `InterfaceAddress`.
    fn load(&mut self, ia: Interface<Address>, index: u32) -> Result<(), Error> {
        self.name = ia.name.clone();
        self.operational |= ia.flags.contains(InterfaceFlags::RUNNING);
        self.netif_index = index;

        match ia.address {
            Address::V4(v) => self.ipv4addrs.push(v.address),
            Address::V6(v) => self.ipv6addrs.push(v.address),
            Address::Mac(mac) => {
                self.hw_addr[..6].copy_from_slice(&mac);
                self.hw_addr[6..].fill(0);
            }
        }

        Ok(())
    }
}
