use super::*;
use rs_matter::{dm, im::EventPriority, tlv::TLVElement};
use service_area::ClusterAsyncHandler;
use std::cell::Cell;
#[derive(Default)]
pub(super) struct Context {
    notifications: Cell<usize>,
}
impl dm::AttrChangeNotifier for Context {
    fn notify_attr_changed(&self, _: u16, _: u32, _: u32) {
        self.notifications.set(self.notifications.get() + 1);
    }
    fn notify_cluster_changed(&self, _: u16, _: u32) {
        self.notifications.set(self.notifications.get() + 1);
    }
    fn notify_endpoint_changed(&self, _: u16) {
        self.notifications.set(self.notifications.get() + 1);
    }
    fn notify_all_changed(&self) {
        self.notifications.set(self.notifications.get() + 1);
    }
}
impl dm::OwnAttrChangeNotifier for Context {
    fn notify_own_attr_changed(&self, _: u32) {
        self.notifications.set(self.notifications.get() + 1);
    }
    fn notify_own_cluster_changed(&self) {
        self.notifications.set(self.notifications.get() + 1);
    }
    fn notify_own_endpoint_changed(&self) {
        self.notifications.set(self.notifications.get() + 1);
    }
}
impl dm::EventEmitter for Context {
    fn emit_event<F>(&self, _: u16, _: u32, _: u32, _: EventPriority, _: F) -> Result<u64, Error>
    where
        F: FnOnce(dm::events::EventTLVWrite<'_>) -> Result<(), Error>,
    {
        panic!("unexpected direct event");
    }
}
impl dm::OwnEventEmitter for Context {
    fn emit_own_event<F>(&self, _: u32, _: EventPriority, _: F) -> Result<u64, Error>
    where
        F: FnOnce(dm::events::EventTLVWrite<'_>) -> Result<(), Error>,
    {
        panic!("unexpected direct event");
    }
}
impl dm::HandlerContext for Context {
    fn matter(&self) -> &rs_matter::Matter<'_> {
        panic!("unexpected Matter access");
    }
    fn crypto(&self) -> impl rs_matter::crypto::Crypto + '_ {
        rs_matter::crypto::default_crypto(rand::thread_rng(), dm::devices::test::DAC_PRIVKEY)
    }
    fn kv(&self) -> impl rs_matter::persist::KvBlobStoreAccess + '_ {
        rs_matter::persist::DummyKvBlobStoreAccess
    }
    fn networks(&self) -> impl dm::clusters::net_comm::NetworksAccess + '_ {
        dm::clusters::net_comm::DummyNetworkAccess
    }
    fn handler(&self) -> impl dm::AsyncHandler + '_ {
        dm::EmptyHandler
    }
    fn buffers(&self) -> impl rs_matter::utils::storage::pooled::BufferAccess<dm::IMBuffer> + '_ {
        rs_matter::utils::storage::pooled::PooledBuffers::<1, dm::IMBuffer>::new(0)
    }
}
impl dm::OperationContext for Context {
    fn exchange(&self) -> &rs_matter::transport::exchange::Exchange<'_> {
        panic!("unexpected exchange");
    }
    fn endpt(&self) -> u16 {
        1
    }
    fn cluster(&self) -> u32 {
        0x150
    }
}
impl dm::InvokeContext for Context {
    fn cmd(&self) -> &dm::CmdDetails<'_> {
        panic!("unexpected command details");
    }
    fn data(&self) -> &TLVElement<'_> {
        panic!("unexpected raw data");
    }
}

impl dm::ReadContext for Context {
    fn attr(&self) -> &dm::AttrDetails<'_> {
        panic!("unexpected attribute details");
    }
}

#[test]
fn dynamic_map_failure_retains_rooms_and_deletion_cannot_expand_to_full_clean() {
    use std::io::{Read, Write};
    let mut device = crate::device::cycle_tests::device();
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

fn select(device: &Device, context: &Context, requested: &[u32]) -> u8 {
    use rs_matter::{
        tlv::{TLVTag, TLVWrite, TLVWriteParent},
        utils::storage::WriteBuf,
    };
    let mut input = [0; 128];
    let mut w = WriteBuf::new(&mut input);
    w.start_struct(&TLVTag::Anonymous).unwrap();
    w.start_array(&TLVTag::Context(0)).unwrap();
    for id in requested {
        w.u32(&TLVTag::Anonymous, *id).unwrap();
    }
    w.end_container().unwrap();
    w.end_container().unwrap();
    let request = service_area::SelectAreasRequest::new(TLVElement::new(w.as_slice()));
    let mut output = [0; 256];
    let mut writer = WriteBuf::new(&mut output);
    let response = service_area::SelectAreasResponseBuilder::new(
        TLVWriteParent::new((), &mut writer),
        &TLVTag::Anonymous,
    )
    .unwrap();
    smol::block_on(device.handle_select_areas(context, request, response)).unwrap();
    TLVElement::new(writer.as_slice())
        .r#struct()
        .unwrap()
        .find_ctx(0)
        .unwrap()
        .u8()
        .unwrap()
}

#[test]
fn actual_selection_handler_validates_deduplicates_and_serializes_mutations() {
    let device = crate::device::cycle_tests::device();
    let context = Context::default();
    *device.segments.borrow_mut() = vec![
        MapSegment {
            id: "16".into(),
            name: None,
        },
        MapSegment {
            id: "17".into(),
            name: None,
        },
    ];
    assert_eq!(select(&device, &context, &[17, 16, 17]), 0);
    assert_eq!(*device.selected_areas.borrow(), vec![17, 16]);
    assert_eq!(context.notifications.get(), 1);
    assert_ne!(select(&device, &context, &[999]), 0);
    assert_eq!(*device.selected_areas.borrow(), vec![17, 16]);
    let guard = device.try_command().unwrap();
    assert_eq!(select(&device, &context, &[17, 16, 16]), 0);
    assert_eq!(select(&device, &context, &[16]), 2);
    assert_eq!(context.notifications.get(), 1);
    drop(guard);
    device.current_state.replace(crate::device::DeviceState {
        value: StatusValue::Cleaning,
        flag: crate::device::StatusFlag::None,
    });
    assert_eq!(select(&device, &context, &[17, 16]), 0);
    assert_eq!(select(&device, &context, &[16]), 2);
    device
        .current_state
        .replace(crate::device::DeviceState::default());
    device.selection_invalidated.set(true);
    device.selected_areas.borrow_mut().clear();
    assert_eq!(select(&device, &context, &[]), 0);
    assert!(!device.selection_invalidated.get());
}

#[test]
fn segment_start_preserves_original_ids_and_selection_order_as_json() {
    use std::io::{Read, Write};
    let mut device = crate::device::cycle_tests::device();
    let ids = ["room\"east", "16"];
    *device.segments.borrow_mut() = Device::normalize_segments(
        ids.iter()
            .map(|id| MapSegment {
                id: (*id).into(),
                name: None,
            })
            .collect(),
    );
    let selected: Vec<_> = ids
        .iter()
        .map(|id| {
            segment_area_id(&MapSegment {
                id: (*id).into(),
                name: None,
            })
            .unwrap()
        })
        .collect();
    assert_eq!(select(&device, &Context::default(), &selected), 0);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let mut header = Vec::new();
        let mut byte = [0];
        while !header.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            header.push(byte[0]);
        }
        let header = String::from_utf8(header).unwrap();
        assert!(header.starts_with("PUT "));
        let len: usize = header
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|s| s.trim().parse().unwrap())
            })
            .unwrap();
        let mut body = vec![0; len];
        stream.read_exact(&mut body).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"action":"start_segment_action","segment_ids":["room\"east","16"],"iterations":1})
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        )
        .unwrap();
    });
    let ex = Box::leak(Box::new(smol::LocalExecutor::new()));
    device.client =
        crate::http::ValetudoClient::new(ex, format!("http://{address}").parse().unwrap());
    smol::block_on(ex.run(device.start_cleaning())).unwrap();
    server.join().unwrap();
}
