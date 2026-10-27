//! In-process tests of production node metadata and handler dispatch (no fabric reset or network commission).
use super::*;
use rs_matter::{
    acl::{Accessor, AccessorSubjects, AuthMode},
    dm::clusters::net_comm::NetworksAccess,
    dm::events::EventTLVWrite,
    dm::{
        AttrChangeNotifier, AttrDetails, AttrId, ClusterId, EndptId, EventEmitter, EventId,
        HandlerContext, OperationContext, OwnAttrChangeNotifier, OwnEventEmitter, ReadContext,
        ReadReplyInstance,
    },
    error::Error,
    im::{EventNumber, EventPriority},
    persist::{DummyKvBlobStoreAccess, KvBlobStoreAccess},
    tlv::{TLVElement, TLVTag},
    transport::exchange::Exchange,
    utils::storage::{WriteBuf, pooled::BufferAccess},
};
struct TestReadContext<'a, C> {
    context: C,
    attr: &'a AttrDetails<'a>,
}
impl<C> HandlerContext for TestReadContext<'_, C>
where
    C: HandlerContext,
{
    fn matter(&self) -> &Matter<'_> {
        self.context.matter()
    }

    fn crypto(&self) -> impl Crypto + '_ {
        self.context.crypto()
    }

    fn kv(&self) -> impl KvBlobStoreAccess + '_ {
        self.context.kv()
    }

    fn networks(&self) -> impl NetworksAccess + '_ {
        self.context.networks()
    }

    fn handler(&self) -> impl AsyncHandler + '_ {
        self.context.handler()
    }

    fn buffers(&self) -> impl BufferAccess<IMBuffer> + '_ {
        self.context.buffers()
    }
}

impl<C> AttrChangeNotifier for TestReadContext<'_, C>
where
    C: HandlerContext,
{
    fn notify_attr_changed(&self, endpoint_id: EndptId, cluster_id: ClusterId, attr_id: AttrId) {
        self.context
            .notify_attr_changed(endpoint_id, cluster_id, attr_id);
    }

    fn notify_cluster_changed(&self, endpoint_id: EndptId, cluster_id: ClusterId) {
        self.context.notify_cluster_changed(endpoint_id, cluster_id);
    }

    fn notify_endpoint_changed(&self, endpoint_id: EndptId) {
        self.context.notify_endpoint_changed(endpoint_id);
    }

    fn notify_all_changed(&self) {
        self.context.notify_all_changed();
    }
}

impl<C> EventEmitter for TestReadContext<'_, C>
where
    C: HandlerContext,
{
    fn emit_event<F>(
        &self,
        endpoint_id: EndptId,
        cluster_id: ClusterId,
        event_id: EventId,
        priority: EventPriority,
        f: F,
    ) -> Result<EventNumber, Error>
    where
        F: FnOnce(EventTLVWrite<'_>) -> Result<(), Error>,
    {
        self.context
            .emit_event(endpoint_id, cluster_id, event_id, priority, f)
    }
}

impl<C> OperationContext for TestReadContext<'_, C>
where
    C: HandlerContext,
{
    fn exchange(&self) -> &Exchange<'_> {
        panic!("This attribute read must not inspect an exchange")
    }

    fn endpt(&self) -> EndptId {
        self.attr.endpoint_id
    }

    fn cluster(&self) -> ClusterId {
        self.attr.cluster_id
    }
}

impl<C> OwnAttrChangeNotifier for TestReadContext<'_, C>
where
    C: HandlerContext,
{
    fn notify_own_attr_changed(&self, attr_id: AttrId) {
        self.notify_attr_changed(self.endpt(), self.cluster(), attr_id);
    }

    fn notify_own_cluster_changed(&self) {
        self.notify_cluster_changed(self.endpt(), self.cluster());
    }

    fn notify_own_endpoint_changed(&self) {
        self.notify_endpoint_changed(self.endpt());
    }
}

impl<C> OwnEventEmitter for TestReadContext<'_, C>
where
    C: HandlerContext,
{
    fn emit_own_event<F>(
        &self,
        event_id: EventId,
        priority: EventPriority,
        f: F,
    ) -> Result<EventNumber, Error>
    where
        F: FnOnce(EventTLVWrite<'_>) -> Result<(), Error>,
    {
        self.context
            .emit_event(self.endpt(), self.cluster(), event_id, priority, f)
    }
}

impl<C> ReadContext for TestReadContext<'_, C>
where
    C: HandlerContext,
{
    fn attr(&self) -> &AttrDetails<'_> {
        self.attr
    }
}

fn device_fixture() -> Device {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for body in [
            r#"["BasicControlCapability"]"#,
            r#"[{"__class":"StatusStateAttribute","value":"docked","flag":"none"}]"#,
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    let ex = Box::leak(Box::new(smol::LocalExecutor::new()));
    let client = crate::http::ValetudoClient::new(ex, format!("http://{address}").parse().unwrap());
    let device = smol::block_on(ex.run(Device::init(client))).unwrap();
    server.join().unwrap();
    device
}

#[test]
fn production_root_chain_dispatches_general_diagnostics_to_real_provider() {
    let device = device_fixture();
    let general =
        crate::general_diagnostics::GeneralDiagnostics::new(true, "192.0.2.2".parse().unwrap(), 42);
    let wifi = crate::wifi_diagnostics::WifiDiagnostics::new();
    let matter = Matter::new(
        &rs_matter::dm::devices::test::TEST_DEV_DET,
        TEST_DEV_COMM,
        &TEST_DEV_ATT,
        rs_matter::utils::epoch::sys_epoch,
        MATTER_PORT,
    );
    let crypto = default_crypto(rand::thread_rng(), DAC_PRIVKEY);
    let buffers = PooledBuffers::<2, IMBuffer>::new(0);
    let subscriptions = Subscriptions::<2>::new();
    let events = Events::<128>::new_default();
    let power = crate::power_source::PowerSource::new(&device, true);
    let handler = dm_handler(
        NODE,
        crypto.rand().unwrap(),
        &device,
        &wifi,
        &general,
        &power,
    );
    let dm = DataModel::new(
        &matter,
        &crypto,
        &buffers,
        &subscriptions,
        &events,
        handler,
        DummyKvBlobStoreAccess,
        SharedNetworks::new(EthNetwork::new_default()),
    );
    for (id, expected) in [(1, 42), (2, 0)] {
        let attr = AttrDetails {
            node: &NODE,
            endpoint_id: 0,
            cluster_id: 0x33,
            attr_id: id,
            list_index: None,
            list_chunked: false,
            fab_idx: 0,
            fab_filter: false,
            dataver: None,
            wildcard: false,
        };
        let ctx = TestReadContext {
            context: &dm,
            attr: &attr,
        };
        let mut bytes = [0; 256];
        let mut writer = WriteBuf::new(&mut bytes);
        smol::block_on(
            dm.handler()
                .read(&ctx, ReadReplyInstance::new(&attr, &mut writer)),
        )
        .unwrap();
        let value = TLVElement::new(writer.as_slice())
            .r#struct()
            .unwrap()
            .find_ctx(1)
            .unwrap()
            .r#struct()
            .unwrap()
            .find_ctx(2)
            .unwrap();
        if id == 1 {
            assert_eq!(value.u64().unwrap(), expected);
        } else {
            assert!(value.u64().unwrap() < 5);
        }
    }
}

#[test]
fn optional_clusters_are_rejected_by_production_metadata_path_expansion() {
    use rs_matter::{
        im::client::ReadRequestBuilder,
        im::{AttrPath, IMStatusCode, ReadReq, ReportDataReq},
        tlv::ToTLV,
    };
    let root = root_clusters(false);
    let rvc = rvc_clusters(false, false);
    let endpoints = [
        Endpoint {
            clusters: &root,
            ..NODE.endpoints[0].clone()
        },
        Endpoint {
            clusters: &rvc,
            ..NODE.endpoints[1].clone()
        },
    ];
    let node = Node {
        endpoints: &endpoints,
    };
    let matter = Matter::new(
        &rs_matter::dm::devices::test::TEST_DEV_DET,
        TEST_DEV_COMM,
        &TEST_DEV_ATT,
        rs_matter::utils::epoch::sys_epoch,
        MATTER_PORT,
    );
    let accessor = Accessor::new(0, AccessorSubjects::new(0), Some(AuthMode::Pase), &matter);
    for (endpoint, cluster) in [
        (0, 0x31),
        (0, 0x36),
        (0, 0x37),
        (1, CLEAN_MODE_CLUSTER.id),
        (1, SERVICE_AREA_CLUSTER.id),
    ] {
        let paths = [AttrPath {
            endpoint: Some(endpoint),
            cluster: Some(cluster),
            attr: Some(0),
            ..Default::default()
        }];
        let mut bytes = [0; 256];
        let mut writer = WriteBuf::new(&mut bytes);
        ReadRequestBuilder::attributes(&paths, false)
            .to_tlv(&TLVTag::Anonymous, &mut writer)
            .unwrap();
        let req = ReadReq::new(TLVElement::new(writer.as_slice()));
        let report = ReportDataReq::Read(&req);
        let mut expanded = node.read(&report, &accessor, |_, _, _| true).unwrap();
        let status = expanded.next().unwrap().unwrap().unwrap_err();
        assert_eq!(status.status.status, IMStatusCode::UnsupportedCluster);
        assert!(expanded.next().is_none());
    }
}

// Identify handlers only use endpoint/cluster notification from these contexts.
// Deliberately fail if they unexpectedly depend on a transport exchange or raw
// write payload instead of their decoded arguments.
impl<C: HandlerContext> rs_matter::dm::WriteContext for TestReadContext<'_, C> {
    fn attr(&self) -> &AttrDetails<'_> {
        self.attr
    }
    fn data(&self) -> &TLVElement<'_> {
        panic!("decoded Identify handler must not read raw payload")
    }
}

impl<C: HandlerContext> rs_matter::dm::InvokeContext for TestReadContext<'_, C> {
    fn cmd(&self) -> &rs_matter::dm::CmdDetails<'_> {
        panic!("decoded handler must not inspect transport command")
    }
    fn data(&self) -> &TLVElement<'_> {
        panic!("decoded handler must not inspect raw payload")
    }
}

#[test]
fn identify_handler_cancel_locate_and_expiry_use_real_http_and_notifications() {
    use rs_matter::dm::clusters::decl::identify::ClusterAsyncHandler;
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let replies = [
            r#"["BasicControlCapability","LocateCapability","SpeakerTestCapability"]"#,
            r#"[{"__class":"StatusStateAttribute","value":"docked","flag":"none"}]"#,
            "{}",
        ];
        for (index, body) in replies.iter().enumerate() {
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
            let header = String::from_utf8(request).unwrap();
            if index == 2 {
                assert!(header.starts_with("PUT "));
                assert_eq!(
                    header
                        .split_whitespace()
                        .nth(1)
                        .unwrap()
                        .parse::<hyper::Uri>()
                        .unwrap()
                        .path(),
                    "/api/v2/robot/capabilities/LocateCapability"
                );
                let size = header
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                let mut payload = vec![0; size];
                stream.read_exact(&mut payload).unwrap();
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&payload).unwrap(),
                    serde_json::json!({"action":"locate"})
                );
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        listener
    });
    let ex = Box::leak(Box::new(smol::LocalExecutor::new()));
    let device = smol::block_on(ex.run(Device::init(crate::http::ValetudoClient::new(
        ex,
        format!("http://{address}").parse().unwrap(),
    ))))
    .unwrap();
    let general =
        crate::general_diagnostics::GeneralDiagnostics::new(true, "192.0.2.2".parse().unwrap(), 1);
    let wifi = crate::wifi_diagnostics::WifiDiagnostics::new();
    let matter = Matter::new(
        &rs_matter::dm::devices::test::TEST_DEV_DET,
        TEST_DEV_COMM,
        &TEST_DEV_ATT,
        rs_matter::utils::epoch::sys_epoch,
        MATTER_PORT,
    );
    let crypto = default_crypto(rand::thread_rng(), DAC_PRIVKEY);
    let buffers = PooledBuffers::<2, IMBuffer>::new(0);
    let subscriptions = Subscriptions::<2>::new();
    let events = Events::<128>::new_default();
    let power = crate::power_source::PowerSource::new(&device, true);
    let handler = dm_handler(
        NODE,
        crypto.rand().unwrap(),
        &device,
        &wifi,
        &general,
        &power,
    );
    let dm = DataModel::new(
        &matter,
        &crypto,
        &buffers,
        &subscriptions,
        &events,
        handler,
        DummyKvBlobStoreAccess,
        SharedNetworks::new(EthNetwork::new_default()),
    );
    let attr = AttrDetails {
        node: &NODE,
        endpoint_id: 1,
        cluster_id: 3,
        attr_id: 0,
        list_index: None,
        list_chunked: false,
        fab_idx: 0,
        fab_filter: false,
        dataver: None,
        wildcard: false,
    };
    let ctx = TestReadContext {
        context: &dm,
        attr: &attr,
    };
    smol::block_on(ex.run(async {
        device.set_identify_time(&ctx, 0).await.unwrap();
        device.present_identify_once().await; // Must not occupy the one expected PUT.
        assert_eq!(device.identify_time(&ctx).await.unwrap(), 0);
        device.set_identify_time(&ctx, 10).await.unwrap();
        assert_eq!(
            device.identify_type(&ctx).await.unwrap(),
            identify::IdentifyTypeEnum::AudibleBeep
        );
        device.present_identify_once().await;
        device.set_identify_time(&ctx, 0).await.unwrap();
        device.present_identify_once().await;
        assert!(device.identify_deadline.get().is_none());
    }));
    let listener = server.join().unwrap();
    listener.set_nonblocking(true).unwrap();
    assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
    struct Notifications(std::cell::Cell<usize>);
    impl AttrChangeNotifier for Notifications {
        fn notify_attr_changed(&self, _: u16, _: u32, _: u32) {}
        fn notify_cluster_changed(&self, ep: u16, cluster: u32) {
            assert_eq!((ep, cluster), (1, 3));
            self.0.set(self.0.get() + 1);
        }
        fn notify_endpoint_changed(&self, _: u16) {}
        fn notify_all_changed(&self) {}
    }
    let notify = Notifications(std::cell::Cell::new(0));
    device.identify_time.set(5);
    device.identify_deadline.set(Some(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
    ));
    device.tick_identify(&notify);
    assert_eq!(notify.0.get(), 0); // Countdown is not reportable.
    device.identify_deadline.set(Some(
        std::time::Instant::now() - std::time::Duration::from_secs(1),
    ));
    device.tick_identify(&notify);
    device.tick_identify(&notify);
    assert_eq!(notify.0.get(), 1); // Expiry reports exactly once.
    assert_eq!(device.identify_time.get(), 0);

    // A deliberate new SelectAreas(empty) is distinct from a list cleared by
    // map deletion: it acknowledges that unconstrained cleaning is requested.
    use rs_matter::{
        dm::clusters::decl::service_area::{self, ClusterAsyncHandler as _},
        tlv::TLVWriteParent,
    };
    let area_attr = AttrDetails {
        cluster_id: service_area::FULL_CLUSTER.id,
        ..attr
    };
    let area_ctx = TestReadContext {
        context: &dm,
        attr: &area_attr,
    };
    device.selection_invalidated.set(true);
    let request_bytes = [0x15, 0x36, 0, 0x18, 0x18]; // Structure { NewAreas: [] }
    let request = service_area::SelectAreasRequest::new(TLVElement::new(&request_bytes));
    let mut response_bytes = [0; 128];
    let mut writer = WriteBuf::new(&mut response_bytes);
    let response = service_area::SelectAreasResponseBuilder::new(
        TLVWriteParent::new((), &mut writer),
        &TLVTag::Anonymous,
    )
    .unwrap();
    smol::block_on(device.handle_select_areas(&area_ctx, request, response)).unwrap();
    assert!(!device.selection_invalidated.get());
    assert_eq!(
        TLVElement::new(writer.as_slice())
            .r#struct()
            .unwrap()
            .find_ctx(0)
            .unwrap()
            .u8()
            .unwrap(),
        0
    );

    device
        .segments
        .borrow_mut()
        .push(crate::device::MapSegment {
            id: "16".into(),
            name: Some("Kitchen".into()),
        });
    let _pending_command = device.try_command().unwrap();
    for (requested, expected) in [(vec![], 0), (vec![16u32], 2)] {
        use rs_matter::tlv::TLVWrite;
        let mut input = [0; 64];
        let mut input_writer = WriteBuf::new(&mut input);
        input_writer.start_struct(&TLVTag::Anonymous).unwrap();
        input_writer.start_array(&TLVTag::Context(0)).unwrap();
        for id in requested {
            input_writer.u32(&TLVTag::Anonymous, id).unwrap();
        }
        input_writer.end_container().unwrap();
        input_writer.end_container().unwrap();
        let request =
            service_area::SelectAreasRequest::new(TLVElement::new(input_writer.as_slice()));
        let mut output = [0; 128];
        let mut writer = WriteBuf::new(&mut output);
        let response = service_area::SelectAreasResponseBuilder::new(
            TLVWriteParent::new((), &mut writer),
            &TLVTag::Anonymous,
        )
        .unwrap();
        smol::block_on(device.handle_select_areas(&area_ctx, request, response)).unwrap();
        assert_eq!(
            TLVElement::new(writer.as_slice())
                .r#struct()
                .unwrap()
                .find_ctx(0)
                .unwrap()
                .u8()
                .unwrap(),
            expected
        );
        assert!(device.selected_areas.borrow().is_empty());
    }
}

#[test]
fn dynamic_map_failure_retains_rooms_and_deletion_cannot_expand_to_full_clean() {
    use std::io::{Read, Write};
    let mut device = device_fixture();
    device
        .capabilities
        .insert(crate::device::Capability::MapSegmentationCapability);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for (status, body) in [
            (200, r#"[{"id":"16","name":"Kitchen"}]"#),
            (503, "{}"),
            (200, "[]"),
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
            let header = String::from_utf8(request).unwrap();
            assert!(header.starts_with("GET "));
            assert_eq!(
                header
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .parse::<hyper::Uri>()
                    .unwrap()
                    .path(),
                "/api/v2/robot/capabilities/MapSegmentationCapability"
            );
            write!(
                stream,
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        listener
    });
    let ex = Box::leak(Box::new(smol::LocalExecutor::new()));
    device.client =
        crate::http::ValetudoClient::new(ex, format!("http://{address}").parse().unwrap());
    smol::block_on(ex.run(async {
        device.refresh_segments(&()).await;
        assert_eq!(device.segments.borrow().len(), 1);
        device.selected_areas.borrow_mut().push(16);
        let version = device.current_state.dataver();
        device.refresh_segments(&()).await; // Failed HTTP is not map deletion.
        assert_eq!(device.segments.borrow().len(), 1);
        assert_eq!(*device.selected_areas.borrow(), vec![16]);
        assert_eq!(device.current_state.dataver(), version);
        device.refresh_segments(&()).await;
        assert!(device.segments.borrow().is_empty());
        assert!(device.selected_areas.borrow().is_empty());
        assert!(device.selection_invalidated.get());
        assert!(device.start_cleaning().await.is_err());
    }));
    let listener = server.join().unwrap();
    listener.set_nonblocking(true).unwrap();
    assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
}

#[test]
fn audit_diagnostic_command_event_metadata() {
    for c in [<&crate::general_diagnostics::GeneralDiagnostics as rs_matter::dm::clusters::gen_diag::ClusterHandler>::CLUSTER, <&crate::wifi_diagnostics::WifiDiagnostics as rs_matter::dm::clusters::wifi_diag::ClusterHandler>::CLUSTER] {
 println!("AUDIT cluster={} revision={} commands={:?} events={:?}", c.id,c.revision,c.commands.iter().filter(|x|(c.with_cmds)(x,c.revision,c.feature_map)).map(|x|x.id).collect::<Vec<_>>(), c.events.iter().filter(|x|(c.with_events)(x,c.revision,c.feature_map)).map(|x|x.id).collect::<Vec<_>>());
 }
}

#[test]
fn audit_general_diagnostics_revision_two_requires_time_snapshot() {
    let c=<&crate::general_diagnostics::GeneralDiagnostics as rs_matter::dm::clusters::gen_diag::ClusterHandler>::CLUSTER;
    assert!(c.revision >= 2);
    assert!(
        c.commands
            .iter()
            .any(|x| x.id == 1 && (c.with_cmds)(x, c.revision, c.feature_map)),
        "General Diagnostics revision 2 omits mandatory TimeSnapshot command"
    );
}
#[test]
fn audit_wifi_does_not_advertise_unimplemented_events() {
    let c=<&crate::wifi_diagnostics::WifiDiagnostics as rs_matter::dm::clusters::wifi_diag::ClusterHandler>::CLUSTER;
    let events: Vec<_> = c
        .events
        .iter()
        .filter(|x| (c.with_events)(x, c.revision, c.feature_map))
        .map(|x| x.id)
        .collect();
    assert!(
        events.is_empty(),
        "Wi-Fi diagnostics advertises events with no producer: {:?}",
        events
    );
}

#[test]
fn startup_events_are_emitted_in_order_and_stored_with_correct_priority() {
    use std::cell::RefCell;
    struct Recorder {
        events: Events<1024>,
        seen: RefCell<Vec<(u16, u32, u32, EventPriority)>>,
    }
    impl EventEmitter for Recorder {
        fn emit_event<F>(
            &self,
            ep: u16,
            cl: u32,
            id: u32,
            priority: EventPriority,
            f: F,
        ) -> Result<u64, Error>
        where
            F: FnOnce(EventTLVWrite<'_>) -> Result<(), Error>,
        {
            let n = self
                .events
                .push(ep, cl, id, priority, DummyKvBlobStoreAccess, f)?;
            self.seen.borrow_mut().push((ep, cl, id, priority));
            Ok(n)
        }
    }
    let recorder = Recorder {
        events: Events::new_default(),
        seen: RefCell::new(vec![]),
    };
    emit_startup_events(&recorder, 10009).unwrap();
    assert_eq!(
        *recorder.seen.borrow(),
        vec![
            (0, 0x28, 0, EventPriority::Critical),
            (0, 0x33, 3, EventPriority::Critical)
        ]
    );
    let c=<&crate::general_diagnostics::GeneralDiagnostics as rs_matter::dm::clusters::gen_diag::ClusterHandler>::CLUSTER;
    assert_eq!(
        c.events
            .iter()
            .filter(|x| (c.with_events)(x, c.revision, c.feature_map))
            .map(|x| x.id)
            .collect::<Vec<_>>(),
        vec![3]
    );
}

#[test]
fn startup_payloads_encode_real_version_and_unspecified_boot_reason() {
    for version in [Some(10009), None] {
        let mut bytes = [0; 64];
        let mut writer = WriteBuf::new(&mut bytes);
        write_lifecycle_payload(&mut writer, version).unwrap();
        let field = TLVElement::new(writer.as_slice())
            .structure()
            .unwrap()
            .find_ctx(0)
            .unwrap();
        assert_eq!(field.u32().unwrap(), version.unwrap_or(0));
    }
}

#[test]
fn event_numbering_survives_restart_with_the_same_persistence() {
    let path = std::env::temp_dir().join(format!("vm-event-restart-{}", rand::random::<u64>()));
    std::fs::create_dir(&path).unwrap();
    let emit = || {
        let mut events = Events::<1024>::new_default();
        let mut kv = DirKvBlobStore::new(path.clone());
        let mut buf = [0; 4096];
        smol::block_on(events.load_persist(&mut kv, &mut buf)).unwrap();
        let shared = SharedKvBlobStore::new(kv, buf);
        events
            .push(0, 0x28, 0, EventPriority::Critical, shared, |mut w| {
                write_lifecycle_payload(&mut w, Some(10009))
            })
            .unwrap()
    };
    let first = emit();
    let second = emit();
    assert!(second > first, "restart reused event numbers");
    std::fs::remove_dir_all(path).unwrap();
}
