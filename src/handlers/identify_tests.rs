//! Exercise Identify state and presentation without a real robot or fabric.
use super::*;
use crate::http::ValetudoClient;
use identify::ClusterAsyncHandler;
use rs_matter::{dm, im::EventPriority, tlv::TLVElement};
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
        3
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

fn device(executor: &'static smol::LocalExecutor<'static>, base: hyper::Uri) -> Device {
    let mut device = crate::device::cycle_tests::device();
    device.client = ValetudoClient::new(executor, base);
    device
}

#[test]
fn same_duration_restarts_and_reports_then_cancel_prevents_any_http() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let executor = Box::leak(Box::new(smol::LocalExecutor::new()));
    let mut robot = device(
        executor,
        format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
    );
    robot.capabilities.insert(Capability::LocateCapability);
    let context = Context::default();
    smol::block_on(executor.run(async {
        robot.identify_robot(&context, 10).await.unwrap();
        // Simulate an existing countdown near expiry, then repeat the same
        // duration. Remaining time must be restarted, not ignored.
        robot.identify_deadline.set(Some(
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        ));
        robot.identify_robot(&context, 10).await.unwrap();
        assert_eq!(robot.identify_time(&context).await.unwrap(), 10);
        assert_eq!(context.notifications.get(), 2);
        robot.identify_robot(&context, 0).await.unwrap();
        robot.present_identify_once().await;
        assert_eq!(robot.identify_time(&context).await.unwrap(), 0);
        assert!(robot.identify_deadline.get().is_none());
        assert_eq!(context.notifications.get(), 3);
    }));
    assert!(matches!(listener.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock));
}

#[test]
fn countdown_is_silent_but_expiration_is_reported_once() {
    let executor = Box::leak(Box::new(smol::LocalExecutor::new()));
    let robot = device(executor, "http://127.0.0.1:1".parse().unwrap());
    let context = Context::default();
    robot.identify_time.set(10);
    robot.identify_deadline.set(Some(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
    ));
    robot.tick_identify(&context);
    assert_eq!(robot.identify_time.get(), 2);
    assert_eq!(context.notifications.get(), 0);
    robot.identify_deadline.set(Some(
        std::time::Instant::now() - std::time::Duration::from_secs(1),
    ));
    robot.tick_identify(&context);
    robot.tick_identify(&context);
    assert_eq!(robot.identify_time.get(), 0);
    assert_eq!(context.notifications.get(), 1);
}

#[test]
fn identify_type_matches_available_presentation() {
    let executor = Box::leak(Box::new(smol::LocalExecutor::new()));
    let mut robot = device(executor, "http://127.0.0.1:1".parse().unwrap());
    let context = Context::default();
    assert_eq!(
        smol::block_on(robot.identify_type(&context)).unwrap(),
        identify::IdentifyTypeEnum::None
    );
    robot.capabilities.insert(Capability::SpeakerTestCapability);
    assert_eq!(
        smol::block_on(robot.identify_type(&context)).unwrap(),
        identify::IdentifyTypeEnum::AudibleBeep
    );
    robot.capabilities.insert(Capability::LocateCapability);
    assert_eq!(
        smol::block_on(robot.identify_type(&context)).unwrap(),
        identify::IdentifyTypeEnum::AudibleBeep
    );
}

#[test]
fn presentation_prefers_locate_with_real_http_and_stops_after_expiry() {
    use std::io::{Read, Write};
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
        let first: Vec<_> = header.lines().next().unwrap().split_whitespace().collect();
        assert_eq!(first[0], "PUT");
        assert_eq!(
            first[1].parse::<hyper::Uri>().unwrap().path(),
            "/api/v2/robot/capabilities/LocateCapability"
        );
        let size = header
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        let mut body = vec![0; size];
        stream.read_exact(&mut body).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"action":"locate"})
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
        listener
    });
    let executor = Box::leak(Box::new(smol::LocalExecutor::new()));
    let mut robot = device(executor, format!("http://{address}").parse().unwrap());
    robot.capabilities = Capability::LocateCapability | Capability::SpeakerTestCapability;
    smol::block_on(executor.run(async {
        robot.identify_robot(&Context::default(), 10).await.unwrap();
        robot.present_identify_once().await;
        robot.identify_deadline.set(Some(
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        ));
        robot.present_identify_once().await;
    }));
    let listener = server.join().unwrap();
    listener.set_nonblocking(true).unwrap();
    assert!(matches!(listener.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock));
}
