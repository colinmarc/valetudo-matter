use core::pin::pin;

use std::{net::UdpSocket, path::Path};

use anyhow::Context;
use log::debug;
use rs_matter::{
    MATTER_PORT, Matter, clusters, devices,
    dm::{
        Async, AsyncHandler, AsyncMetadata, Cluster, DataModel, Dataver, DeviceType, EmptyHandler,
        Endpoint, EpClMatcher, Node,
        clusters::{
            desc::{self, ClusterHandler as _},
            net_comm::NetworkType,
        },
        devices::test::{TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET},
        endpoints,
        subscriptions::Subscriptions,
    },
    pairing::{DiscoveryCapabilities, qr::QrTextType},
    persist::{NO_NETWORKS, Psm},
    respond::DefaultResponder,
    sc::pake::MAX_COMM_WINDOW_TIMEOUT_SECS,
    transport::{
        MATTER_SOCKET_BIND_ADDR,
        network::mdns::builtin::{BuiltinMdnsResponder, Host},
    },
    utils::{storage::pooled::PooledBuffers, sync::blocking::raw::StdRawMutex},
};
use smol::future;

use crate::{device::Device, net::Netif};
use crate::{
    generated::{identify, rvc_clean_mode, rvc_operational_state, rvc_run_mode},
    net::GetifaddrsDiag,
};

pub(crate) async fn run(device: &Device, persistence_dir: &Path) -> anyhow::Result<()> {
    let matter = Matter::new(
        &TEST_DEV_DET,
        TEST_DEV_COMM,
        &TEST_DEV_ATT,
        rs_matter::utils::epoch::sys_epoch,
        rs_matter::utils::rand::sys_rand,
        MATTER_PORT,
    );

    // Need to call this once
    matter.initialize_transport_buffers()?;
    let buffers = PooledBuffers::<10, StdRawMutex, _>::new(0);

    let subscriptions = Subscriptions::<64>::new();

    // Create the Data Model instance
    let dm = DataModel::new(
        &matter,
        &buffers,
        &subscriptions,
        dm_handler(&matter, device),
    );

    // Create a default responder capable of handling up to 3 subscriptions
    // All other subscription requests will be turned down with "resource exhausted"
    let responder = DefaultResponder::new(&dm);
    debug!(
        "Responder memory: Responder (stack)={}B, Runner fut (stack)={}B",
        core::mem::size_of_val(&responder),
        core::mem::size_of_val(&responder.run::<4, 4>())
    );

    // Run the responder with up to 4 handlers (i.e. 4 exchanges can be handled simultaneously)
    // Clients trying to open more exchanges than the ones currently running will get "I'm busy, please try again later"
    let mut respond = pin!(responder.run::<4, 4>());

    // Run the Matter and mDNS transports
    let iface = Netif::pick()
        .await
        .context("Failed to find suitable network interface")?;
    debug!("using network interface {}", iface.name);

    let mdns_socket = iface
        .bind_mdns_socket()
        .context("Failed to bind mdns socket")?;
    let mdns_socket =
        smol::Async::new(mdns_socket).context("Failed to set socket to non-blocking mode")?;

    let host = Host {
        id: 0,
        hostname: "valetudo",
        ip: iface.ipv4_addr,
        ipv6: iface.ipv6_addr,
    };

    let ipv4_iface = if cfg!(target_os = "linux") {
        Some(iface.ipv4_addr)
    } else {
        None
    };

    let mdns = BuiltinMdnsResponder::new(&matter);
    let mut mdns = pin!(mdns.run(&mdns_socket, &mdns_socket, &host, ipv4_iface, iface.index));

    let socket = smol::Async::<UdpSocket>::bind(MATTER_SOCKET_BIND_ADDR)?;
    let mut transport = pin!(matter.run(&socket, &socket));

    // Create, load and run the persister
    let mut psm = Psm::<8192>::new();
    debug!("using persistence at path: {}", persistence_dir.display());
    psm.load(persistence_dir, &matter, NO_NETWORKS)?;

    let mut psm = pin!(psm.run(persistence_dir, &matter, NO_NETWORKS));

    if !matter.is_commissioned() {
        // If the device is not commissioned yet, print the QR text and code to the console
        // and enable basic commissioning.
        matter.print_standard_qr_text(DiscoveryCapabilities::IP)?;
        matter.print_standard_qr_code(QrTextType::Unicode, DiscoveryCapabilities::IP)?;

        matter.open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS)?;
    }

    let mut dm = pin!(dm.run());
    let fut = try_zip5(&mut transport, &mut mdns, &mut psm, &mut respond, &mut dm);
    Ok(fut.await?)
}

async fn try_zip5<E>(
    f1: impl Future<Output = Result<(), E>>,
    f2: impl Future<Output = Result<(), E>>,
    f3: impl Future<Output = Result<(), E>>,
    f4: impl Future<Output = Result<(), E>>,
    f5: impl Future<Output = Result<(), E>>,
) -> Result<(), E> {
    #[rustfmt::skip]
    future::try_zip(f1,
        future::try_zip(f2,
            future::try_zip(f3,
                future::try_zip(f4, f5))),
    )
    .await?;

    Ok(())
}

const DEV_TYPE_RVC: DeviceType = DeviceType {
    dtype: 0x0074,
    drev: 4,
};

const IDENTIFY_CLUSTER: Cluster<'static> = <Device as identify::ClusterAsyncHandler>::CLUSTER;
const RUN_MODE_CLUSTER: Cluster<'static> = <Device as rvc_run_mode::ClusterAsyncHandler>::CLUSTER;
const CLEAN_MODE_CLUSTER: Cluster<'static> =
    <Device as rvc_clean_mode::ClusterAsyncHandler>::CLUSTER;
const OPERATIONAL_STATE_CLUSTER: Cluster<'static> =
    <Device as rvc_operational_state::ClusterAsyncHandler>::CLUSTER;

const NODE: Node<'static> = Node {
    id: 0,
    endpoints: &[
        endpoints::root_endpoint(NetworkType::Ethernet),
        Endpoint {
            id: 1,
            device_types: devices!(DEV_TYPE_RVC),
            clusters: clusters!(
                desc::DescHandler::CLUSTER,
                IDENTIFY_CLUSTER,
                RUN_MODE_CLUSTER,
                // CLEAN_MODE_CLUSTER,
                OPERATIONAL_STATE_CLUSTER,
            ),
        },
    ],
};

fn dm_handler<'a>(
    matter: &'a Matter<'a>,
    device: &'a Device,
) -> impl AsyncMetadata + AsyncHandler + 'a {
    (
        NODE,
        endpoints::with_eth(
            &(),
            &GetifaddrsDiag,
            matter.rand(),
            endpoints::with_sys(
                &true,
                matter.rand(),
                EmptyHandler
                    .chain(
                        EpClMatcher::new(Some(1), Some(desc::DescHandler::CLUSTER.id)),
                        Async(desc::DescHandler::new(Dataver::new_rand(matter.rand())).adapt()),
                    )
                    .chain(
                        EpClMatcher::new(Some(1), Some(IDENTIFY_CLUSTER.id)),
                        identify::HandlerAsyncAdaptor(device),
                    )
                    .chain(
                        EpClMatcher::new(Some(1), Some(RUN_MODE_CLUSTER.id)),
                        rvc_run_mode::HandlerAsyncAdaptor(device),
                    )
                    // .chain(
                    //     EpClMatcher::new(Some(1), Some(CLEAN_MODE_CLUSTER.id)),
                    //     rvc_clean_mode::HandlerAsyncAdaptor(&device),
                    // )
                    .chain(
                        EpClMatcher::new(Some(1), Some(OPERATIONAL_STATE_CLUSTER.id)),
                        rvc_operational_state::HandlerAsyncAdaptor(device),
                    ),
            ),
        ),
    )
}
