use core::pin::pin;

use std::path::Path;

use anyhow::Context;
use log::debug;
use rand::RngCore;
use rs_matter::{
    MATTER_PORT, Matter, clusters,
    crypto::{Crypto, default_crypto},
    devices,
    dm::{
        Async, AsyncHandler, AsyncMetadata, Cluster, DataModel, Dataver, DeviceType, EmptyHandler,
        Endpoint, EpClMatcher, IMBuffer, Node,
        clusters::{
            desc::{self, ClusterHandler as _},
            net_comm::SharedNetworks,
        },
        devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM},
        events::Events,
        networks::eth::EthNetwork,
        subscriptions::Subscriptions,
    },
    pairing::{DiscoveryCapabilities, qr::QrTextType},
    persist::{DirKvBlobStore, SharedKvBlobStore},
    respond::DefaultResponder,
    sc::pase::MAX_COMM_WINDOW_TIMEOUT_SECS,
    transport::network::mdns::builtin::{BuiltinMdnsResponder, Host},
    utils::storage::pooled::PooledBuffers,
};
use smol::future;

use crate::device::Device;
use crate::generated::{
    identify, rvc_clean_mode, rvc_operational_state, rvc_run_mode, service_area,
};
use crate::net::Netif;

pub(crate) async fn run(device: &Device, persistence_dir: &Path) -> anyhow::Result<()> {
    let native = std::env::var("VALETUDO_MATTER_NATIVE_DIAGNOSTICS").as_deref() == Ok("1");
    let wifi = crate::wifi_diagnostics::WifiDiagnostics::new();
    let iface = if native {
        Netif::from_valetudo(&device.client)
            .await
            .context("Cannot bind native Matter to Valetudo interface")?
    } else {
        Netif::pick()
            .await
            .context("Failed to find suitable network interface")?
    };
    let lifetime = crate::node_lifetime::NodeLifetime::start(persistence_dir)?;
    let general = crate::general_diagnostics::GeneralDiagnostics::new(
        native,
        iface.ipv4_addr,
        lifetime.reboot_count,
    );
    let identity = crate::identity::Identity::load(&device.client, native).await?;
    log::info!(
        "Software diagnostics: {}",
        serde_json::to_string(identity.diagnostics())?
    );
    let basic_info = identity.basic_info();
    log::info!(
        "Basic Information: vendor={} product={} serial={} software={} version={}",
        basic_info.vendor_name,
        basic_info.product_name,
        basic_info.serial_no,
        basic_info.sw_ver_str,
        basic_info.sw_ver
    );
    let mut matter = Matter::new(
        &basic_info,
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
    let mut events = Events::<1024>::new_default();
    // Preserve event numbering across application restarts as well as fabrics.
    events.load_persist(&mut kv, &mut kv_buf).await?;
    let power = crate::power_source::PowerSource::new(device, native);
    let root_clusters = root_clusters(native);
    let rvc_clusters = rvc_clusters(
        device.supports_clean_modes(),
        device
            .capabilities
            .contains(crate::device::Capability::MapSegmentationCapability),
    );
    let mut endpoints = vec![
        Endpoint {
            clusters: &root_clusters,
            ..NODE.endpoints[0].clone()
        },
        Endpoint {
            clusters: &rvc_clusters,
            ..NODE.endpoints[1].clone()
        },
    ];
    // BAT requires a coarse charge assessment; a charging flag alone cannot
    // supply it. Omit the optional endpoint if initial charge data is absent.
    if device.battery.get().half_percent.is_some() {
        endpoints.push(Endpoint {
            id: 2,
            device_types: &[DeviceType {
                dtype: 0x0011,
                drev: 1,
            }],
            clusters: &[desc::DescHandler::CLUSTER, POWER_SOURCE_CLUSTER],
        });
    }
    let node = Node {
        endpoints: &endpoints,
    };

    let dm = DataModel::new(
        &matter,
        &crypto,
        &buffers,
        &subscriptions,
        &events,
        dm_handler(node, rng, device, &wifi, &general, &power),
        SharedKvBlobStore::new(kv, kv_buf),
        SharedNetworks::new(EthNetwork::new_default()),
    );

    emit_startup_events(&dm, basic_info.sw_ver)?;
    log::info!("Matter StartUp and BootReason events recorded");
    let responder = DefaultResponder::new(&dm);
    debug!(
        "Responder memory: Responder (stack)={}B, Runner fut (stack)={}B",
        core::mem::size_of_val(&responder),
        core::mem::size_of_val(&responder.run::<4, 4>())
    );

    let mut respond = pin!(responder.run::<4, 4>());

    // mDNS via the builtin responder (no Avahi dependency).
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
    let mut mdns = pin!(mdns.run(&mdns_socket, &mdns_socket, &host, ipv4_iface, iface.index,));

    // Matter transport.
    let socket = smol::Async::new(iface.bind_matter_socket()?)?;
    let mut transport = pin!(matter.run(&crypto, &socket, &socket, &socket));

    if !matter.is_commissioned() {
        matter.print_standard_qr_text(DiscoveryCapabilities::IP)?;
        matter.print_standard_qr_code(QrTextType::Unicode, DiscoveryCapabilities::IP)?;
        matter.open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS, &crypto, dm.change_notify())?;
    }

    let mut dm_run = pin!(dm.run());
    let mut monitor = pin!(async {
        future::zip(
            device.monitor_status(
                &subscriptions,
                RUN_MODE_CLUSTER.id,
                CLEAN_MODE_CLUSTER.id,
                OPERATIONAL_STATE_CLUSTER.id,
            ),
            future::zip(
                async {
                    if native {
                        wifi.monitor(&device.client, &subscriptions).await
                    } else {
                        std::future::pending::<()>().await
                    }
                },
                future::zip(
                    general.monitor(&device.client, &subscriptions),
                    future::zip(
                        device.monitor_identify(&subscriptions),
                        future::zip(device.monitor_segments(&subscriptions), async {
                            loop {
                                device.power_battery.update(
                                    device.battery.get(),
                                    std::time::Instant::now(),
                                    &subscriptions,
                                );
                                device.flush_events(&dm);
                                smol::Timer::after(std::time::Duration::from_millis(100)).await;
                            }
                        }),
                    ),
                ),
            ),
        )
        .await;
        Ok::<(), rs_matter::error::Error>(())
    });

    let fut = try_zip5(
        &mut transport,
        &mut mdns,
        &mut respond,
        &mut dm_run,
        &mut monitor,
    );
    Ok(fut.await?)
}

fn emit_startup_events(
    emitter: &impl rs_matter::dm::EventEmitter,
    software_version: u32,
) -> Result<(), rs_matter::error::Error> {
    use rs_matter::im::EventPriority;
    emitter.emit_event(0, 0x28, 0, EventPriority::Critical, |mut writer| {
        write_lifecycle_payload(&mut writer, Some(software_version))
    })?;
    emitter.emit_event(0, 0x33, 3, EventPriority::Critical, |mut writer| {
        write_lifecycle_payload(&mut writer, None)
    })?;
    Ok(())
}

fn write_lifecycle_payload(
    writer: &mut impl rs_matter::tlv::TLVWrite,
    software_version: Option<u32>,
) -> Result<(), rs_matter::error::Error> {
    use rs_matter::{dm::events::EVENT_DATA_TAG, tlv::TLVTag};
    writer.start_struct(&EVENT_DATA_TAG)?;
    if let Some(version) = software_version {
        writer.u32(&TLVTag::Context(0), version)?;
    } else {
        writer.u8(&TLVTag::Context(0), 0)?;
    } // BootReason: Unspecified, not a guessed MCU reset cause.
    writer.end_container()
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

const POWER_SOURCE_CLUSTER: Cluster<'static> = <crate::power_source::PowerSource<'_> as rs_matter::dm::clusters::decl::power_source::ClusterHandler>::CLUSTER;

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
        Endpoint {
            id: 0,
            device_types: devices!(rs_matter::dm::devices::DEV_TYPE_ROOT_NODE),
            clusters: clusters!(
                <rs_matter::dm::clusters::desc::DescHandler as rs_matter::dm::clusters::desc::ClusterHandler>::CLUSTER,
                <rs_matter::dm::clusters::acl::AclHandler as rs_matter::dm::clusters::acl::ClusterHandler>::CLUSTER,
                <rs_matter::dm::clusters::basic_info::BasicInfoHandler as rs_matter::dm::clusters::basic_info::ClusterHandler>::CLUSTER,
                <rs_matter::dm::clusters::gen_comm::GenCommHandler as rs_matter::dm::clusters::gen_comm::ClusterHandler>::CLUSTER,
                <rs_matter::dm::clusters::adm_comm::AdminCommHandler as rs_matter::dm::clusters::adm_comm::ClusterHandler>::CLUSTER,
                <rs_matter::dm::clusters::noc::NocHandler as rs_matter::dm::clusters::noc::ClusterHandler>::CLUSTER,
                <rs_matter::dm::clusters::grp_key_mgmt::GrpKeyMgmtHandler as rs_matter::dm::clusters::grp_key_mgmt::ClusterHandler>::CLUSTER,
                <rs_matter::dm::clusters::groups::GroupsHandler as rs_matter::dm::clusters::groups::ClusterHandler>::CLUSTER,
                <&crate::general_diagnostics::GeneralDiagnostics as rs_matter::dm::clusters::gen_diag::ClusterHandler>::CLUSTER,
                <&crate::wifi_diagnostics::WifiDiagnostics as rs_matter::dm::clusters::wifi_diag::ClusterHandler>::CLUSTER,
            ),
        },
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

fn root_clusters(native: bool) -> Vec<Cluster<'static>> {
    NODE.endpoints[0]
        .clusters
        .iter()
        .filter(|c| native || c.id != 0x36)
        .cloned()
        .collect()
}
fn rvc_clusters(clean_modes: bool, segments: bool) -> Vec<Cluster<'static>> {
    NODE.endpoints[1]
        .clusters
        .iter()
        .filter(|c| {
            (c.id != CLEAN_MODE_CLUSTER.id || clean_modes)
                && (c.id != SERVICE_AREA_CLUSTER.id || segments)
        })
        .cloned()
        .collect()
}

fn dm_handler<'a>(
    node: Node<'a>,
    mut rand: impl RngCore + Copy,
    device: &'a Device,
    wifi: &'a crate::wifi_diagnostics::WifiDiagnostics,
    general: &'a crate::general_diagnostics::GeneralDiagnostics,
    power: &'a crate::power_source::PowerSource<'a>,
) -> impl AsyncMetadata + AsyncHandler + 'a {
    (
        node,
        rs_matter::dm::ChainedHandler::new(
            EpClMatcher::new(Some(0), Some(0x33)),
            Async(rs_matter::dm::clusters::gen_diag::HandlerAdaptor(general)),
            with_custom_network_sys(
            &false,
            rand,
            EmptyHandler
                .chain(EpClMatcher::new(Some(2), Some(POWER_SOURCE_CLUSTER.id)), Async(rs_matter::dm::clusters::decl::power_source::HandlerAdaptor(power)))
                .chain(EpClMatcher::new(Some(2), Some(desc::DescHandler::CLUSTER.id)), Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()))
                .chain(
                    EpClMatcher::new(Some(0), Some(<&crate::wifi_diagnostics::WifiDiagnostics as rs_matter::dm::clusters::wifi_diag::ClusterHandler>::CLUSTER.id)),
                    Async(rs_matter::dm::clusters::wifi_diag::HandlerAdaptor(wifi)),
                )
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
                    crate::matter_audit::Audit(rvc_run_mode::HandlerAsyncAdaptor(device)),
                )
                .chain(
                    EpClMatcher::new(Some(1), Some(CLEAN_MODE_CLUSTER.id)),
                    crate::matter_audit::Audit(rvc_clean_mode::HandlerAsyncAdaptor(device)),
                )
                .chain(
                    EpClMatcher::new(Some(1), Some(OPERATIONAL_STATE_CLUSTER.id)),
                    crate::matter_audit::Audit(rvc_operational_state::HandlerAsyncAdaptor(device)),
                )
                .chain(
                    EpClMatcher::new(Some(1), Some(SERVICE_AREA_CLUSTER.id)),
                    service_area::HandlerAsyncAdaptor(device),
                ),
        )),
    )
}

// Network setup belongs to Valetudo/the host OS (Root Node CustomNetworkConfig).
// Do not advertise a synthetic Ethernet Network Commissioning instance.
fn with_custom_network_sys<'a>(
    policy: &'a dyn rs_matter::dm::clusters::gen_comm::CommPolicy,
    mut rand: impl RngCore,
    handler: impl AsyncHandler + 'a,
) -> impl AsyncHandler + 'a {
    use rs_matter::dm::clusters::{acl, adm_comm, basic_info, gen_comm, groups, grp_key_mgmt, noc};
    use rs_matter::dm::clusters::{
        acl::ClusterHandler as _, adm_comm::ClusterHandler as _, basic_info::ClusterHandler as _,
        gen_comm::ClusterHandler as _, groups::ClusterHandler as _,
        grp_key_mgmt::ClusterHandler as _, noc::ClusterHandler as _,
    };
    rs_matter::dm::ChainedHandler::new(
        EpClMatcher::new(Some(0), Some(desc::DescHandler::CLUSTER.id)),
        Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
        handler,
    )
    .chain(
        EpClMatcher::new(Some(0), Some(basic_info::BasicInfoHandler::CLUSTER.id)),
        Async(basic_info::BasicInfoHandler::new(Dataver::new_rand(&mut rand)).adapt()),
    )
    .chain(
        EpClMatcher::new(Some(0), Some(gen_comm::GenCommHandler::CLUSTER.id)),
        Async(gen_comm::GenCommHandler::new(Dataver::new_rand(&mut rand), policy).adapt()),
    )
    .chain(
        EpClMatcher::new(Some(0), Some(adm_comm::AdminCommHandler::CLUSTER.id)),
        Async(adm_comm::AdminCommHandler::new(Dataver::new_rand(&mut rand)).adapt()),
    )
    .chain(
        EpClMatcher::new(Some(0), Some(noc::NocHandler::CLUSTER.id)),
        Async(noc::NocHandler::new(Dataver::new_rand(&mut rand)).adapt()),
    )
    .chain(
        EpClMatcher::new(Some(0), Some(acl::AclHandler::CLUSTER.id)),
        Async(acl::AclHandler::new(Dataver::new_rand(&mut rand)).adapt()),
    )
    .chain(
        EpClMatcher::new(Some(0), Some(grp_key_mgmt::GrpKeyMgmtHandler::CLUSTER.id)),
        Async(grp_key_mgmt::GrpKeyMgmtHandler::new(Dataver::new_rand(&mut rand)).adapt()),
    )
    .chain(
        EpClMatcher::new(Some(0), Some(groups::GroupsHandler::CLUSTER.id)),
        Async(groups::GroupsHandler::new(Dataver::new_rand(&mut rand)).adapt()),
    )
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    #[test]
    fn topology_is_direct_rvc_without_bridging() {
        assert_eq!(NODE.endpoints[0].id, 0);
        assert_eq!(NODE.endpoints[0].device_types[0].dtype, 0x0016);
        assert_eq!(NODE.endpoints[1].id, 1);
        assert_eq!(NODE.endpoints[1].device_types[0].dtype, 0x0074);
        for endpoint in NODE.endpoints {
            assert!(
                !endpoint
                    .device_types
                    .iter()
                    .any(|d| d.dtype == 0x000e || d.dtype == 0x0013)
            );
            assert!(!endpoint.clusters.iter().any(|c| c.id == 0x0039));
        }
        assert_ne!(POWER_SOURCE_CLUSTER.id, 0x0039);
        for native in [false, true] {
            assert!(!root_clusters(native).iter().any(|c| c.id == 0x0039));
        }
    }
    #[test]
    fn metadata_matches_supported_transport_and_optional_robot_features() {
        for native in [false, true] {
            let root = root_clusters(native);
            assert!(!root.iter().any(|c| c.id == 0x31 || c.id == 0x37));
            assert_eq!(root.iter().any(|c| c.id == 0x36), native);
            assert_eq!(root.iter().filter(|c| c.id == 0x33).count(), 1);
        }
        for modes in [false, true] {
            for segments in [false, true] {
                let rvc = rvc_clusters(modes, segments);
                assert_eq!(rvc.iter().any(|c| c.id == CLEAN_MODE_CLUSTER.id), modes);
                assert_eq!(
                    rvc.iter().any(|c| c.id == SERVICE_AREA_CLUSTER.id),
                    segments
                );
                assert!(rvc.iter().any(|c| c.id == OPERATIONAL_STATE_CLUSTER.id));
            }
        }
    }
    #[test]
    fn root_has_one_general_diagnostics_with_uptime_and_one_wifi_diagnostics() {
        let root = &NODE.endpoints[0];
        assert_eq!(root.clusters.iter().filter(|c| c.id == 0x33).count(), 1);
        assert_eq!(root.clusters.iter().filter(|c| c.id == 0x36).count(), 1);
        let general = root.clusters.iter().find(|c| c.id == 0x33).unwrap();
        assert!(general.attribute(0).is_some());
        assert!(general.attribute(2).is_some());
        assert_eq!(general.revision, 2);
    }
}

#[cfg(test)]
#[path = "node_integration_tests.rs"]
mod integration_tests;
