use core::pin::pin;

use std::{net::UdpSocket, path::Path};

use anyhow::Context;
use log::debug;
use rand::RngCore;
use rs_matter::{
    MATTER_PORT, Matter, clusters, devices,
    crypto::{Crypto, default_crypto},
    dm::{
        Async, AsyncHandler, AsyncMetadata, Cluster, DataModel, Dataver, DeviceType, EmptyHandler,
        Endpoint, EpClMatcher, IMBuffer, Node,
        clusters::{
            desc::{self, ClusterHandler as _},
            net_comm::SharedNetworks,
        },
        devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET},
        endpoints,
        events::NoEvents,
        networks::{SysNetifs, eth::EthNetwork},
        subscriptions::Subscriptions,
    },
    pairing::{DiscoveryCapabilities, qr::QrTextType},
    persist::{DirKvBlobStore, SharedKvBlobStore},
    respond::DefaultResponder,
    root_endpoint,
    sc::pase::MAX_COMM_WINDOW_TIMEOUT_SECS,
    transport::{
        MATTER_SOCKET_BIND_ADDR,
        network::mdns::builtin::{BuiltinMdnsResponder, Host},
    },
    utils::storage::pooled::PooledBuffers,
};
use smol::future;

use crate::device::Device;
use crate::generated::{
    identify, rvc_clean_mode, rvc_operational_state, rvc_run_mode, service_area,
};
use crate::net::Netif;

pub(crate) async fn run(
    device: &Device,
    persistence_dir: &Path,
) -> anyhow::Result<()> {
    let mut matter = Matter::new(
        &TEST_DEV_DET,
        TEST_DEV_COMM,
        &TEST_DEV_ATT,
        rs_matter::utils::epoch::sys_epoch,
        MATTER_PORT,
    );

    let crypto = default_crypto(rand::thread_rng(), DAC_PRIVKEY);
    let rng = crypto.rand()?;

    // Persistence.
    let mut kv_buf = [0u8; 4096];
    let mut kv = DirKvBlobStore::new(persistence_dir.to_path_buf());
    smol::block_on(matter.load_persist(&mut kv, &mut kv_buf))?;

    let buffers = PooledBuffers::<10, IMBuffer>::new(0);
    let subscriptions = Subscriptions::<64>::new();
    let events = NoEvents::new_default();

    let dm = DataModel::new(
        &matter,
        &crypto,
        &buffers,
        &subscriptions,
        &events,
        dm_handler(rng, device),
        SharedKvBlobStore::new(kv, kv_buf),
        SharedNetworks::new(EthNetwork::new_default()),
    );

    let responder = DefaultResponder::new(&dm);
    debug!(
        "Responder memory: Responder (stack)={}B, Runner fut (stack)={}B",
        core::mem::size_of_val(&responder),
        core::mem::size_of_val(&responder.run::<4, 4>())
    );

    let mut respond = pin!(responder.run::<4, 4>());

    // mDNS via the builtin responder (no Avahi dependency).
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

    let mdns = BuiltinMdnsResponder::new(&matter, &crypto);
    let mut mdns = pin!(mdns.run(
        &mdns_socket, &mdns_socket, &host, ipv4_iface, iface.index,
    ));

    // Matter transport.
    let socket = smol::Async::<UdpSocket>::bind(MATTER_SOCKET_BIND_ADDR)?;
    let mut transport = pin!(matter.run(&crypto, &socket, &socket, &socket));

    if !matter.is_commissioned() {
        matter.print_standard_qr_text(DiscoveryCapabilities::IP)?;
        matter.print_standard_qr_code(QrTextType::Unicode, DiscoveryCapabilities::IP)?;
        matter.open_basic_comm_window(
            MAX_COMM_WINDOW_TIMEOUT_SECS,
            &crypto,
            dm.change_notify(),
        )?;
    }

    let mut dm = pin!(dm.run());
    let mut monitor = pin!(async {
        device.monitor_status(
            &subscriptions,
            RUN_MODE_CLUSTER.id,
            CLEAN_MODE_CLUSTER.id,
            OPERATIONAL_STATE_CLUSTER.id,
        ).await;
        Ok::<(), rs_matter::error::Error>(())
    });

    let fut = try_zip5(
        &mut transport, &mut mdns, &mut respond, &mut dm, &mut monitor,
    );
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
const SERVICE_AREA_CLUSTER: Cluster<'static> =
    <Device as service_area::ClusterAsyncHandler>::CLUSTER;

const NODE: Node<'static> = Node {
    endpoints: &[
        root_endpoint!(geth),
        Endpoint {
            id: 1,
            device_types: devices!(DEV_TYPE_RVC),
            clusters: clusters!(
                desc::DescHandler::CLUSTER,
                IDENTIFY_CLUSTER,
                RUN_MODE_CLUSTER,
                CLEAN_MODE_CLUSTER,
                OPERATIONAL_STATE_CLUSTER,
                SERVICE_AREA_CLUSTER,
            ),
        },
    ],
};

fn dm_handler<'a>(
    mut rand: impl RngCore + Copy,
    device: &'a Device,
) -> impl AsyncMetadata + AsyncHandler + 'a {
    (
        NODE,
        endpoints::with_eth_sys(
            &false,
            &(),
            &SysNetifs,
            rand,
            EmptyHandler
                .chain(
                    EpClMatcher::new(Some(1), Some(desc::DescHandler::CLUSTER.id)),
                    Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
                )
                .chain(
                    EpClMatcher::new(Some(1), Some(IDENTIFY_CLUSTER.id)),
                    identify::HandlerAsyncAdaptor(device),
                )
                .chain(
                    EpClMatcher::new(Some(1), Some(RUN_MODE_CLUSTER.id)),
                    rvc_run_mode::HandlerAsyncAdaptor(device),
                )
                .chain(
                    EpClMatcher::new(Some(1), Some(CLEAN_MODE_CLUSTER.id)),
                    rvc_clean_mode::HandlerAsyncAdaptor(device),
                )
                .chain(
                    EpClMatcher::new(Some(1), Some(OPERATIONAL_STATE_CLUSTER.id)),
                    rvc_operational_state::HandlerAsyncAdaptor(device),
                )
                .chain(
                    EpClMatcher::new(Some(1), Some(SERVICE_AREA_CLUSTER.id)),
                    service_area::HandlerAsyncAdaptor(device),
                ),
        ),
    )
}
