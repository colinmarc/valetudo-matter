//! Exercise actual cluster handlers and their encoded responses without a fabric
//! or robot. Context methods outside notification fail if unexpectedly used.
use super::*;
use rs_matter::{
    dm::{
        self,
        clusters::decl::{rvc_clean_mode, rvc_operational_state, rvc_run_mode},
    },
    error::Error,
    im::EventPriority,
    tlv::{FromTLV, TLVElement, TLVTag, TLVWrite, TLVWriteParent},
    utils::storage::WriteBuf,
};

#[derive(Default)]
struct Context {
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
        0x54
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

fn mode_request(buffer: &mut [u8], mode: u8) -> usize {
    let mut writer = WriteBuf::new(buffer);
    writer.start_struct(&TLVTag::Anonymous).unwrap();
    writer.u8(&TLVTag::Context(0), mode).unwrap();
    writer.end_container().unwrap();
    writer.as_slice().len()
}

#[test]
fn unsupported_modes_return_cluster_status_and_empty_text_without_http() {
    let device = super::cycle_tests::device();
    let mut request_buffer = [0; 32];
    let length = mode_request(&mut request_buffer, 2); // Mapping absent.
    let request = TLVElement::new(&request_buffer[..length]);
    let mut response_buffer = [0; 128];
    let mut writer = WriteBuf::new(&mut response_buffer);
    let response = rvc_run_mode::ChangeToModeResponseBuilder::new(
        TLVWriteParent::new((), &mut writer),
        &TLVTag::Anonymous,
    )
    .unwrap();
    smol::block_on(rvc_run_mode::ClusterAsyncHandler::handle_change_to_mode(
        &device,
        Context::default(),
        rvc_run_mode::ChangeToModeRequest::from_tlv(&request).unwrap(),
        response,
    ))
    .unwrap();
    let fields = TLVElement::new(writer.as_slice()).structure().unwrap();
    assert_eq!(fields.find_ctx(0).unwrap().u8().unwrap(), 1);
    assert_eq!(fields.find_ctx(1).unwrap().utf8().unwrap(), "");

    let mut writer = WriteBuf::new(&mut response_buffer);
    let response = rvc_clean_mode::ChangeToModeResponseBuilder::new(
        TLVWriteParent::new((), &mut writer),
        &TLVTag::Anonymous,
    )
    .unwrap();
    smol::block_on(rvc_clean_mode::ClusterAsyncHandler::handle_change_to_mode(
        &device,
        Context::default(),
        rvc_clean_mode::ChangeToModeRequest::from_tlv(&request).unwrap(),
        response,
    ))
    .unwrap();
    let fields = TLVElement::new(writer.as_slice()).structure().unwrap();
    assert_eq!(fields.find_ctx(0).unwrap().u8().unwrap(), 1);
    assert_eq!(fields.find_ctx(1).unwrap().utf8().unwrap(), "");
}

#[test]
fn actual_resume_handler_rejects_docked_without_an_operation() {
    let device = super::cycle_tests::device();
    device.current_state.set(DeviceState {
        value: StatusValue::Docked,
        flag: StatusFlag::None,
    });
    let mut buffer = [0; 128];
    let mut writer = WriteBuf::new(&mut buffer);
    let response = rvc_operational_state::OperationalCommandResponseBuilder::new(
        TLVWriteParent::new((), &mut writer),
        &TLVTag::Anonymous,
    )
    .unwrap();
    smol::block_on(rvc_operational_state::ClusterAsyncHandler::handle_resume(
        &device,
        Context::default(),
        response,
    ))
    .unwrap();
    let fields = TLVElement::new(writer.as_slice()).structure().unwrap();
    assert_eq!(
        fields
            .find_ctx(0)
            .unwrap()
            .structure()
            .unwrap()
            .find_ctx(0)
            .unwrap()
            .u8()
            .unwrap(),
        3
    );
    assert_eq!(device.current_state.get().value, StatusValue::Docked);
}

#[test]
fn actual_run_handler_returns_generic_failure_on_unknown_state() {
    let device = super::cycle_tests::device();
    device.current_state.set(DeviceState {
        value: StatusValue::Unknown,
        flag: StatusFlag::Unknown,
    });
    let mut request_buffer = [0; 32];
    let length = mode_request(&mut request_buffer, 1);
    let request = TLVElement::new(&request_buffer[..length]);
    let mut response_buffer = [0; 128];
    let mut writer = WriteBuf::new(&mut response_buffer);
    let response = rvc_run_mode::ChangeToModeResponseBuilder::new(
        TLVWriteParent::new((), &mut writer),
        &TLVTag::Anonymous,
    )
    .unwrap();
    smol::block_on(rvc_run_mode::ClusterAsyncHandler::handle_change_to_mode(
        &device,
        Context::default(),
        rvc_run_mode::ChangeToModeRequest::from_tlv(&request).unwrap(),
        response,
    ))
    .unwrap();
    let fields = TLVElement::new(writer.as_slice()).structure().unwrap();
    assert_eq!(fields.find_ctx(0).unwrap().u8().unwrap(), 2);
    assert!(!fields.find_ctx(1).unwrap().utf8().unwrap().is_empty());
}

#[test]
fn mapping_start_stop_and_clean_mode_notify_after_real_api_acknowledgements() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for (path, expected) in [
            (
                "/api/v2/robot/capabilities/MappingPassCapability",
                serde_json::json!({"action":"start_mapping"}),
            ),
            (
                "/api/v2/robot/capabilities/FanSpeedControlCapability/preset",
                serde_json::json!({"name":"high"}),
            ),
            (
                "/api/v2/robot/capabilities/BasicControlCapability",
                serde_json::json!({"action":"stop"}),
            ),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
            }
            let headers = String::from_utf8(bytes).unwrap();
            let request_line: Vec<_> = headers.lines().next().unwrap().split_whitespace().collect();
            assert_eq!(request_line[0], "PUT");
            assert_eq!(request_line[1].parse::<hyper::Uri>().unwrap().path(), path);
            assert_eq!(request_line[2], "HTTP/1.1");
            let length = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                expected
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        }
    });
    let executor = Box::leak(Box::new(smol::LocalExecutor::new()));
    let mut device = super::cycle_tests::device();
    device.client = ValetudoClient::new(executor, format!("http://{address}").parse().unwrap());
    device
        .capabilities
        .insert(Capability::MappingPassCapability);
    device.cleaning_presets.insert(Preset::High);
    device.cleaning_presets.insert(Preset::Medium);
    device.fan_speed_modes = true;
    let ctx = Context::default();
    smol::block_on(executor.run(async {
        let mut request_buffer = [0; 32];
        let length = mode_request(&mut request_buffer, 2);
        let request = TLVElement::new(&request_buffer[..length]);
        let mut response_buffer = [0; 128];
        let mut writer = WriteBuf::new(&mut response_buffer);
        let response = rvc_run_mode::ChangeToModeResponseBuilder::new(
            TLVWriteParent::new((), &mut writer),
            &TLVTag::Anonymous,
        )
        .unwrap();
        rvc_run_mode::ClusterAsyncHandler::handle_change_to_mode(
            &device,
            &ctx,
            rvc_run_mode::ChangeToModeRequest::from_tlv(&request).unwrap(),
            response,
        )
        .await
        .unwrap();
        assert_eq!(
            TLVElement::new(writer.as_slice())
                .structure()
                .unwrap()
                .find_ctx(0)
                .unwrap()
                .u8()
                .unwrap(),
            0
        );
        assert_eq!(device.cycle_mode.get(), CycleMode::Mapping);
        assert_eq!(ctx.notifications.get(), 1);
        // A matching streamed activity observation promotes optional timing
        // eligibility after the actual Matter command/API acknowledgement.
        device.apply_attributes(
            &[StateAttribute::StatusStateAttribute {
                value: StatusValue::Moving,
                flag: StatusFlag::Mapping,
                error: None,
            }],
            &(),
            0x54,
            0x55,
            0x61,
        );

        let length = mode_request(&mut request_buffer, 6);
        let request = TLVElement::new(&request_buffer[..length]);
        let mut writer = WriteBuf::new(&mut response_buffer);
        let response = rvc_clean_mode::ChangeToModeResponseBuilder::new(
            TLVWriteParent::new((), &mut writer),
            &TLVTag::Anonymous,
        )
        .unwrap();
        rvc_clean_mode::ClusterAsyncHandler::handle_change_to_mode(
            &device,
            &ctx,
            rvc_clean_mode::ChangeToModeRequest::from_tlv(&request).unwrap(),
            response,
        )
        .await
        .unwrap();
        assert_eq!(
            TLVElement::new(writer.as_slice())
                .structure()
                .unwrap()
                .find_ctx(0)
                .unwrap()
                .u8()
                .unwrap(),
            0
        );
        assert_eq!(device.current_preset.get(), Some(Preset::High));
        assert_eq!(ctx.notifications.get(), 2); // Before SSE, which may repeat the same value.

        // An explicit stop while the robot reports an error completes with that
        // error, rather than claiming success merely because HTTP stop succeeded.
        device.current_state.set(DeviceState {
            value: StatusValue::Error,
            flag: StatusFlag::None,
        });
        let length = mode_request(&mut request_buffer, 0);
        let request = TLVElement::new(&request_buffer[..length]);
        let mut writer = WriteBuf::new(&mut response_buffer);
        let response = rvc_run_mode::ChangeToModeResponseBuilder::new(
            TLVWriteParent::new((), &mut writer),
            &TLVTag::Anonymous,
        )
        .unwrap();
        rvc_run_mode::ClusterAsyncHandler::handle_change_to_mode(
            &device,
            &ctx,
            rvc_run_mode::ChangeToModeRequest::from_tlv(&request).unwrap(),
            response,
        )
        .await
        .unwrap();
        assert_eq!(
            TLVElement::new(writer.as_slice())
                .structure()
                .unwrap()
                .find_ctx(0)
                .unwrap()
                .u8()
                .unwrap(),
            0
        );
        assert_eq!(device.cycle_mode.get(), CycleMode::Idle);
        assert_eq!(ctx.notifications.get(), 3);
        assert!(matches!(
            device.pending_events.borrow().front(),
            Some(OperationalEvent::CompletedWithTiming(
                2,
                CompletionTiming {
                    total_seconds: Some(_),
                    paused_seconds: Some(0)
                }
            ))
        ));
    }));
    server.join().unwrap();
}

#[test]
fn concurrent_fabrics_cannot_issue_home_resume_or_second_start_before_start_ack() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (received_tx, received_rx) = smol::channel::bounded(1);
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(time::Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"action":"start"})
        );
        received_tx.send_blocking(()).unwrap();
        release_rx
            .recv_timeout(time::Duration::from_secs(5))
            .unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
    });
    let executor = Box::leak(Box::new(smol::LocalExecutor::new()));
    let mut device = super::cycle_tests::device();
    device.client = ValetudoClient::new(executor, format!("http://{address}").parse().unwrap());
    let ctx = Context::default();
    smol::block_on(executor.run(async {
        smol::future::zip(
            async {
                let mut input = [0; 32];
                let length = mode_request(&mut input, 1);
                let request = TLVElement::new(&input[..length]);
                let mut output = [0; 128];
                let mut writer = WriteBuf::new(&mut output);
                let response = rvc_run_mode::ChangeToModeResponseBuilder::new(
                    TLVWriteParent::new((), &mut writer),
                    &TLVTag::Anonymous,
                )
                .unwrap();
                rvc_run_mode::ClusterAsyncHandler::handle_change_to_mode(
                    &device,
                    &ctx,
                    rvc_run_mode::ChangeToModeRequest::from_tlv(&request).unwrap(),
                    response,
                )
                .await
                .unwrap();
                assert_eq!(
                    TLVElement::new(writer.as_slice())
                        .structure()
                        .unwrap()
                        .find_ctx(0)
                        .unwrap()
                        .u8()
                        .unwrap(),
                    0
                );
            },
            async {
                received_rx.recv().await.unwrap();
                let mut output = [0; 128];
                let mut writer = WriteBuf::new(&mut output);
                let response = rvc_operational_state::OperationalCommandResponseBuilder::new(
                    TLVWriteParent::new((), &mut writer),
                    &TLVTag::Anonymous,
                )
                .unwrap();
                rvc_operational_state::ClusterAsyncHandler::handle_go_home(&device, &ctx, response)
                    .await
                    .unwrap();
                assert_eq!(
                    TLVElement::new(writer.as_slice())
                        .structure()
                        .unwrap()
                        .find_ctx(0)
                        .unwrap()
                        .structure()
                        .unwrap()
                        .find_ctx(0)
                        .unwrap()
                        .u8()
                        .unwrap(),
                    3
                );
                let mut writer = WriteBuf::new(&mut output);
                let response = rvc_operational_state::OperationalCommandResponseBuilder::new(
                    TLVWriteParent::new((), &mut writer),
                    &TLVTag::Anonymous,
                )
                .unwrap();
                rvc_operational_state::ClusterAsyncHandler::handle_resume(&device, &ctx, response)
                    .await
                    .unwrap();
                assert_eq!(
                    TLVElement::new(writer.as_slice())
                        .structure()
                        .unwrap()
                        .find_ctx(0)
                        .unwrap()
                        .structure()
                        .unwrap()
                        .find_ctx(0)
                        .unwrap()
                        .u8()
                        .unwrap(),
                    1
                );
                let mut input = [0; 32];
                let length = mode_request(&mut input, 1);
                let request = TLVElement::new(&input[..length]);
                let mut writer = WriteBuf::new(&mut output);
                let response = rvc_run_mode::ChangeToModeResponseBuilder::new(
                    TLVWriteParent::new((), &mut writer),
                    &TLVTag::Anonymous,
                )
                .unwrap();
                rvc_run_mode::ClusterAsyncHandler::handle_change_to_mode(
                    &device,
                    &ctx,
                    rvc_run_mode::ChangeToModeRequest::from_tlv(&request).unwrap(),
                    response,
                )
                .await
                .unwrap();
                assert_eq!(
                    TLVElement::new(writer.as_slice())
                        .structure()
                        .unwrap()
                        .find_ctx(0)
                        .unwrap()
                        .u8()
                        .unwrap(),
                    2
                );
                assert_eq!(ctx.notifications.get(), 0);
                release_tx.send(()).unwrap();
            },
        )
        .await;
    }));
    assert_eq!(device.cycle_mode.get(), CycleMode::Cleaning);
    assert_eq!(ctx.notifications.get(), 1);
    assert!(device.try_command().is_some());
    server.join().unwrap();
}

#[test]
fn unknown_restart_cycle_rejects_new_start_and_resume_without_dispatch() {
    let device = super::cycle_tests::device();
    device.current_state.set(DeviceState {
        value: StatusValue::Paused,
        flag: StatusFlag::Resumable,
    });
    device.cycle_known.set(false);
    assert!(device.cycle_active()); // Selection/start guards must treat it as busy.
    let mut input = [0; 32];
    let length = mode_request(&mut input, 1);
    let request = TLVElement::new(&input[..length]);
    let mut output = [0; 128];
    let mut writer = WriteBuf::new(&mut output);
    let response = rvc_run_mode::ChangeToModeResponseBuilder::new(
        TLVWriteParent::new((), &mut writer),
        &TLVTag::Anonymous,
    )
    .unwrap();
    smol::block_on(rvc_run_mode::ClusterAsyncHandler::handle_change_to_mode(
        &device,
        Context::default(),
        rvc_run_mode::ChangeToModeRequest::from_tlv(&request).unwrap(),
        response,
    ))
    .unwrap();
    assert_eq!(
        TLVElement::new(writer.as_slice())
            .structure()
            .unwrap()
            .find_ctx(0)
            .unwrap()
            .u8()
            .unwrap(),
        3
    );
    let mut writer = WriteBuf::new(&mut output);
    let response = rvc_operational_state::OperationalCommandResponseBuilder::new(
        TLVWriteParent::new((), &mut writer),
        &TLVTag::Anonymous,
    )
    .unwrap();
    smol::block_on(rvc_operational_state::ClusterAsyncHandler::handle_resume(
        &device,
        Context::default(),
        response,
    ))
    .unwrap();
    assert_eq!(
        TLVElement::new(writer.as_slice())
            .structure()
            .unwrap()
            .find_ctx(0)
            .unwrap()
            .structure()
            .unwrap()
            .find_ctx(0)
            .unwrap()
            .u8()
            .unwrap(),
        1
    );
}

#[test]
fn api_activity_observed_before_http_ack_is_not_lost_on_ack() {
    let device = super::cycle_tests::device();
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Cleaning,
            flag: StatusFlag::Segment,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    assert!(device.cycle_observed_active.get());
    device.set_cycle_mode(CycleMode::Cleaning, &Context::default());
    assert!(device.cycle_observed_active.get());
    assert!(device.start_pending_since.get().is_none());
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Docked,
            flag: StatusFlag::None,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    assert_eq!(device.cycle_mode.get(), CycleMode::Idle);
    assert_eq!(
        *device.pending_events.borrow(),
        [OperationalEvent::Completed(0)]
    );
}

#[test]
fn restarted_ambiguous_paused_state_does_not_load_history_or_emit_historical_completion() {
    use std::io::{Read, Write};
    let directory = std::env::temp_dir().join(format!(
        "valetudo-no-history-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    let history = br#"{"version":1,"mode":"Mapping","observed_active":true,"state":{"value":"paused","flag":"resumable"},"paused_activity":{"value":"returning","flag":"none"}}"#;
    std::fs::write(directory.join("rvc-lifecycle.json"), history).unwrap();
    std::fs::write(directory.join("fabric.bin"), b"untouched").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for (path, body) in [
            (
                "/api/v2/robot/capabilities",
                r#"["BasicControlCapability"]"#,
            ),
            (
                "/api/v2/robot/state/attributes",
                r#"[{"__class":"StatusStateAttribute","value":"paused","flag":"resumable"}]"#,
            ),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(time::Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            let mut byte = [0];
            while !headers.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap();
            let first: Vec<_> = headers.lines().next().unwrap().split_whitespace().collect();
            assert_eq!(first[0], "GET");
            assert_eq!(first[1].parse::<hyper::Uri>().unwrap().path(), path);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
        listener.set_nonblocking(true).unwrap();
        listener
    });
    let executor = Box::leak(Box::new(smol::LocalExecutor::new()));
    let device = smol::block_on(executor.run(Device::init(ValetudoClient::new(
        executor,
        format!("http://{address}").parse().unwrap(),
    ))))
    .unwrap();
    let listener = server.join().unwrap();
    // The actual operation could be paused cleaning or paused docking. The
    // identical public flags do not prove either; disk history is never loaded.
    assert!(!device.cycle_known.get());
    assert!(!device.cycle_observed_active.get());
    assert!(!device.resume_context_trusted.get());
    assert_eq!(device.paused_activity.get(), None);
    assert!(
        smol::block_on(rvc_run_mode::ClusterAsyncHandler::current_mode(
            &device,
            Context::default()
        ))
        .is_err()
    );
    let mut output = [0; 128];
    let mut writer = WriteBuf::new(&mut output);
    let response = rvc_operational_state::OperationalCommandResponseBuilder::new(
        TLVWriteParent::new((), &mut writer),
        &TLVTag::Anonymous,
    )
    .unwrap();
    smol::block_on(rvc_operational_state::ClusterAsyncHandler::handle_resume(
        &device,
        Context::default(),
        response,
    ))
    .unwrap();
    assert_eq!(
        TLVElement::new(writer.as_slice())
            .structure()
            .unwrap()
            .find_ctx(0)
            .unwrap()
            .structure()
            .unwrap()
            .find_ctx(0)
            .unwrap()
            .u8()
            .unwrap(),
        1
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Idle,
            flag: StatusFlag::None,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    assert!(device.cycle_known.get());
    assert!(device.pending_events.borrow().is_empty());
    assert_eq!(
        std::fs::read(directory.join("rvc-lifecycle.json")).unwrap(),
        history
    );
    assert_eq!(
        std::fs::read(directory.join("fabric.bin")).unwrap(),
        b"untouched"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn disconnected_status_revokes_resume_trust_until_new_active_observation() {
    let device = super::cycle_tests::device();
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Cleaning,
            flag: StatusFlag::Segment,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    assert!(device.resume_context_trusted.get());
    device.apply_snapshot(&[], &(), 0x54, 0x55, 0x61);
    assert!(!device.resume_context_trusted.get());
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Paused,
            flag: StatusFlag::Resumable,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    assert!(!device.resume_context_trusted.get());
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Returning,
            flag: StatusFlag::None,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Paused,
            flag: StatusFlag::Resumable,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    assert!(device.resume_context_trusted.get());
    assert_eq!(
        device.paused_activity.get().unwrap().value,
        StatusValue::Returning
    );
}

#[test]
fn late_start_ack_does_not_resurrect_an_already_observed_completed_cycle() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (received_tx, received_rx) = smol::channel::bounded(1);
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(time::Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"action":"start"})
        );
        received_tx.send_blocking(()).unwrap();
        release_rx
            .recv_timeout(time::Duration::from_secs(5))
            .unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
    });
    let executor = Box::leak(Box::new(smol::LocalExecutor::new()));
    let mut device = super::cycle_tests::device();
    device.client = ValetudoClient::new(executor, format!("http://{address}").parse().unwrap());
    smol::block_on(executor.run(async {
        smol::future::zip(
            async {
                let mut input = [0; 32];
                let length = mode_request(&mut input, 1);
                let request = TLVElement::new(&input[..length]);
                let mut output = [0; 128];
                let mut writer = WriteBuf::new(&mut output);
                let response = rvc_run_mode::ChangeToModeResponseBuilder::new(
                    TLVWriteParent::new((), &mut writer),
                    &TLVTag::Anonymous,
                )
                .unwrap();
                rvc_run_mode::ClusterAsyncHandler::handle_change_to_mode(
                    &device,
                    Context::default(),
                    rvc_run_mode::ChangeToModeRequest::from_tlv(&request).unwrap(),
                    response,
                )
                .await
                .unwrap();
                assert_eq!(
                    TLVElement::new(writer.as_slice())
                        .structure()
                        .unwrap()
                        .find_ctx(0)
                        .unwrap()
                        .u8()
                        .unwrap(),
                    0
                );
            },
            async {
                received_rx.recv().await.unwrap();
                device.apply_attributes(
                    &[StateAttribute::StatusStateAttribute {
                        value: StatusValue::Cleaning,
                        flag: StatusFlag::Segment,
                        error: None,
                    }],
                    &(),
                    0x54,
                    0x55,
                    0x61,
                );
                device.apply_attributes(
                    &[StateAttribute::StatusStateAttribute {
                        value: StatusValue::Idle,
                        flag: StatusFlag::None,
                        error: None,
                    }],
                    &(),
                    0x54,
                    0x55,
                    0x61,
                );
                assert_eq!(device.cycle_mode.get(), CycleMode::Idle);
                assert_eq!(
                    *device.pending_events.borrow(),
                    [OperationalEvent::Completed(0)]
                );
                release_tx.send(()).unwrap();
            },
        )
        .await;
        assert_eq!(
            rvc_run_mode::ClusterAsyncHandler::current_mode(&device, Context::default())
                .await
                .unwrap(),
            0
        );
    }));
    assert!(device.start_pending_since.get().is_none());
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Idle,
            flag: StatusFlag::None,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    assert_eq!(device.cycle_mode.get(), CycleMode::Idle);
    assert_eq!(
        *device.pending_events.borrow(),
        [OperationalEvent::Completed(0)]
    );
    assert_eq!(device.completion_generation.get(), 1);
    server.join().unwrap();
}

#[test]
fn observation_gap_prevents_stale_ack_and_historical_completion_after_reconnect() {
    let device = super::cycle_tests::device();
    let receipt = device.start_receipt();
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Cleaning,
            flag: StatusFlag::Segment,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    device.apply_snapshot(&[], &(), 0x54, 0x55, 0x61);
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Paused,
            flag: StatusFlag::Resumable,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    assert!(!device.cycle_known.get());
    assert!(!device.cycle_observed_active.get());
    assert_eq!(device.paused_activity.get(), None);
    assert!(
        smol::block_on(rvc_run_mode::ClusterAsyncHandler::current_mode(
            &device,
            Context::default()
        ))
        .is_err()
    );
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Idle,
            flag: StatusFlag::None,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    assert!(!device.acknowledge_start(CycleMode::Cleaning, receipt, &Context::default()));
    assert_eq!(device.cycle_mode.get(), CycleMode::Idle);
    assert!(device.pending_events.borrow().is_empty());
}

#[test]
fn power_source_serialized_percent_does_not_leak_raw_updates_on_flag_reports() {
    use crate::power_source::PowerSource;
    use rs_matter::{
        dm::clusters::decl::power_source::{BatChargeStateEnum, ClusterHandler},
        tlv::ToTLV,
    };
    let device = super::cycle_tests::device();
    let power = PowerSource::new(&device, true);
    let ctx = Context::default();
    let t = std::time::Instant::now();
    let first = BatteryState {
        half_percent: Some(100),
        flag: BatteryFlag::Discharging,
    };
    device.battery.set(first);
    device.power_battery.update(first, t, &ctx);
    let next = BatteryState {
        half_percent: Some(150),
        flag: BatteryFlag::Charging,
    };
    device.battery.set(next);
    device
        .power_battery
        .update(next, t + std::time::Duration::from_secs(1), &ctx);
    assert_eq!(device.battery.get(), next); // RVC continues to consume immediate raw state.
    assert_eq!(
        power.bat_charge_state(&ctx).unwrap(),
        BatChargeStateEnum::IsCharging
    );
    for (now, expected) in [(1, 100), (9, 100), (10, 150)] {
        device.power_battery.update(
            device.battery.get(),
            t + std::time::Duration::from_secs(now),
            &ctx,
        );
        let mut bytes = [0; 32];
        let mut writer = WriteBuf::new(&mut bytes);
        power
            .bat_percent_remaining(&ctx)
            .unwrap()
            .to_tlv(&TLVTag::Anonymous, &mut writer)
            .unwrap();
        assert_eq!(TLVElement::new(writer.as_slice()).u8().unwrap(), expected);
    }
    device.power_battery.update(
        BatteryState::default(),
        t + std::time::Duration::from_secs(11),
        &ctx,
    );
    let mut bytes = [0; 32];
    let mut writer = WriteBuf::new(&mut bytes);
    power
        .bat_percent_remaining(&ctx)
        .unwrap()
        .to_tlv(&TLVTag::Anonymous, &mut writer)
        .unwrap();
    TLVElement::new(writer.as_slice()).null().unwrap();
}

fn delayed_operational_ack_preserves_docked(command: &'static str) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (received_tx, received_rx) = smol::channel::bounded(1);
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(time::Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"action":command})
        );
        received_tx.send_blocking(()).unwrap();
        release_rx
            .recv_timeout(time::Duration::from_secs(5))
            .unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
    });
    let executor = Box::leak(Box::new(smol::LocalExecutor::new()));
    let mut device = super::cycle_tests::device();
    device.client = ValetudoClient::new(executor, format!("http://{address}").parse().unwrap());
    device.apply_attributes(
        &[StateAttribute::StatusStateAttribute {
            value: StatusValue::Cleaning,
            flag: StatusFlag::Segment,
            error: None,
        }],
        &(),
        0x54,
        0x55,
        0x61,
    );
    if command == "start" {
        device.apply_attributes(
            &[StateAttribute::StatusStateAttribute {
                value: StatusValue::Paused,
                flag: StatusFlag::Resumable,
                error: None,
            }],
            &(),
            0x54,
            0x55,
            0x61,
        );
    }
    smol::block_on(executor.run(async {
        smol::future::zip(
            async {
                let mut output = [0; 128];
                let mut writer = WriteBuf::new(&mut output);
                let response = rvc_operational_state::OperationalCommandResponseBuilder::new(
                    TLVWriteParent::new((), &mut writer),
                    &TLVTag::Anonymous,
                )
                .unwrap();
                match command {
                    "home" => rvc_operational_state::ClusterAsyncHandler::handle_go_home(
                        &device,
                        Context::default(),
                        response,
                    )
                    .await
                    .unwrap(),
                    "pause" => rvc_operational_state::ClusterAsyncHandler::handle_pause(
                        &device,
                        Context::default(),
                        response,
                    )
                    .await
                    .unwrap(),
                    "start" => rvc_operational_state::ClusterAsyncHandler::handle_resume(
                        &device,
                        Context::default(),
                        response,
                    )
                    .await
                    .unwrap(),
                    _ => unreachable!(),
                };
            },
            async {
                received_rx.recv().await.unwrap();
                device.apply_attributes(
                    &[StateAttribute::StatusStateAttribute {
                        value: StatusValue::Cleaning,
                        flag: StatusFlag::Segment,
                        error: None,
                    }],
                    &(),
                    0x54,
                    0x55,
                    0x61,
                );
                device.apply_attributes(
                    &[StateAttribute::StatusStateAttribute {
                        value: StatusValue::Docked,
                        flag: StatusFlag::None,
                        error: None,
                    }],
                    &(),
                    0x54,
                    0x55,
                    0x61,
                );
                assert_eq!(device.cycle_mode.get(), CycleMode::Idle);
                assert_eq!(
                    *device.pending_events.borrow(),
                    [OperationalEvent::Completed(0)]
                );
                release_tx.send(()).unwrap();
            },
        )
        .await;
        assert_eq!(
            rvc_run_mode::ClusterAsyncHandler::current_mode(&device, Context::default())
                .await
                .unwrap(),
            0
        );
    }));
    server.join().unwrap();
    assert_eq!(
        device.current_state.get().value,
        StatusValue::Docked,
        "late GoHome HTTP ACK overwrote fresher terminal observation"
    );
}

#[test]
fn audit_delayed_home_ack_must_not_overwrite_observed_docked() {
    delayed_operational_ack_preserves_docked("home");
}
#[test]
fn delayed_pause_ack_must_not_overwrite_observed_docked() {
    delayed_operational_ack_preserves_docked("pause");
}
#[test]
fn delayed_resume_ack_must_not_overwrite_observed_docked() {
    delayed_operational_ack_preserves_docked("start");
}

#[test]
fn operational_ack_cannot_restore_state_after_observation_gap_or_newer_state() {
    for next in [
        StatusValue::Docked,
        StatusValue::Error,
        StatusValue::Unknown,
    ] {
        for optimistic in [
            StatusValue::Paused,
            StatusValue::Cleaning,
            StatusValue::Returning,
        ] {
            let d = super::cycle_tests::device();
            d.current_state.set(DeviceState {
                value: StatusValue::Cleaning,
                flag: StatusFlag::Segment,
            });
            let receipt = d.current_state.dataver();
            d.apply_attributes(
                &[StateAttribute::StatusStateAttribute {
                    value: next,
                    flag: StatusFlag::None,
                    error: None,
                }],
                &(),
                0x54,
                0x55,
                0x61,
            );
            assert!(!d.acknowledge_state(
                receipt,
                DeviceState {
                    value: optimistic,
                    flag: StatusFlag::None
                },
                &Context::default()
            ));
            assert_eq!(d.current_state.get().value, next);
        }
    }
}

#[test]
fn time_snapshot_response_encodes_monotonic_ms_and_unknown_wall_clock() {
    use rs_matter::dm::clusters::gen_diag::{self, ClusterHandler};
    let general =
        crate::general_diagnostics::GeneralDiagnostics::new(true, "192.0.2.1".parse().unwrap(), 7);
    let mut bytes = [0; 128];
    let mut writer = WriteBuf::new(&mut bytes);
    let response = gen_diag::TimeSnapshotResponseBuilder::new(
        TLVWriteParent::new((), &mut writer),
        &TLVTag::Anonymous,
    )
    .unwrap();
    (&general)
        .handle_time_snapshot(Context::default(), response)
        .unwrap();
    let fields = TLVElement::new(writer.as_slice()).structure().unwrap();
    assert!(fields.find_ctx(0).unwrap().u64().unwrap() < 1000);
    assert!(fields.find_ctx(1).unwrap().null().is_ok());
}
