//! SDCP V3 control channel (WebSocket JSON) against a fake printer on 127.0.0.1.

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use openresin_net::discovery::DiscoveredPrinter;
use openresin_net::sdcp::{cmd, SdcpClient, SdcpEvent, SdcpState};
use openresin_net::{MachineState, NetError, PrinterClient, Protocol};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

use common::sdcp_mock::{response_message, Action, SdcpMock, BRAND_ID, MAINBOARD_ID};
use common::within;

async fn wait_state(c: &SdcpClient, what: &str, pred: impl FnMut(&SdcpState) -> bool) -> SdcpState {
    let mut rx = c.watch_state();
    let r = within(5.0, what, rx.wait_for(pred))
        .await
        .expect("state channel open")
        .clone();
    r
}

async fn next_event(
    rx: &mut broadcast::Receiver<SdcpEvent>,
    what: &str,
    mut pred: impl FnMut(&SdcpEvent) -> bool,
) -> SdcpEvent {
    within(5.0, what, async {
        loop {
            let e = rx.recv().await.expect("event channel open");
            if pred(&e) {
                return e;
            }
        }
    })
    .await
}

/// (Cmd, Data) of every request after the connect handshake.
fn sent(mock: &SdcpMock) -> Vec<(u32, Value)> {
    let st = mock.state();
    st.requests[2..]
        .iter()
        .map(|r| (r["Data"]["Cmd"].as_u64().unwrap() as u32, r["Data"]["Data"].clone()))
        .collect()
}

fn assert_envelope(req: &Value) {
    assert_eq!(req["Id"], BRAND_ID, "Id from discovery is echoed: {req}");
    assert_eq!(req["Topic"], format!("sdcp/request/{MAINBOARD_ID}"));
    let d = &req["Data"];
    assert_eq!(d["MainboardID"], MAINBOARD_ID);
    assert_eq!(d["From"], 0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let ts = d["TimeStamp"].as_u64().expect("TimeStamp is a number");
    assert!(ts.abs_diff(now) < 120, "TimeStamp must be in seconds: {ts}");
    let rid = d["RequestID"].as_str().expect("RequestID");
    assert!(
        !rid.is_empty() && rid.chars().all(|c| c.is_ascii_hexdigit()),
        "RequestID {rid:?}"
    );
    assert!(d["Cmd"].is_u64());
    assert!(d["Data"].is_object());
}

fn printing(s: &mut Value, sub: i64, layer: u64) {
    s["CurrentStatus"] = json!([1]);
    s["TempOfUVLED"] = json!(35.5);
    s["TempOfBox"] = json!(26.0);
    s["TimeLapseStatus"] = json!(1);
    s["PrintInfo"] = json!({
        "Status": sub, "CurrentLayer": layer, "TotalLayer": 200, "CurrentTicks": 60_000,
        "TotalTicks": 240_000, "Filename": "a.goo", "ErrorNumber": 0, "TaskId": "t"
    });
}

#[tokio::test]
async fn connect_handshake_and_initial_snapshot() {
    let mock = SdcpMock::start().await;
    let c = within(
        5.0,
        "connect",
        SdcpClient::connect_with_port(&mock.printer(), mock.addr.port()),
    )
    .await
    .expect("connect");
    assert_eq!(c.ip, mock.addr.ip());
    assert_eq!(c.port, mock.addr.port());
    assert_eq!(c.id, BRAND_ID);
    assert_eq!(c.mainboard_id, MAINBOARD_ID);
    assert_eq!(c.protocol_version, "V3.0.0");
    {
        let st = mock.state();
        assert_eq!(st.ws_connections, 1);
        // the client asks for attributes and status right away
        assert_eq!(st.commands(), [cmd::ATTRIBUTES, cmd::STATUS]);
        for r in &st.requests {
            assert_envelope(r);
            assert_eq!(r["Data"]["Data"], json!({}));
        }
        let ids: Vec<&str> = st
            .requests
            .iter()
            .map(|r| r["Data"]["RequestID"].as_str().unwrap())
            .collect();
        assert_ne!(ids[0], ids[1], "request ids must be unique");
    }
    // the pushes that preceded the responses are already applied
    let s = c.state();
    assert_eq!(s.status.state, MachineState::Idle);
    assert_eq!(s.status.uv_led_temp, Some(28.5));
    assert_eq!(s.status.box_temp, Some(24.0));
    assert_eq!(s.status.time_lapse, Some(false));
    assert_eq!(s.status.free_storage, Some(5_000_000_000));
    assert_eq!(s.status.camera_connected, Some(true));
    assert_eq!(s.attributes.as_ref().unwrap()["MachineName"], "ELEGOO Mars 5 Ultra");
    assert!(s.raw_status.as_ref().unwrap()["Status"]["PrintInfo"].is_object());
}

#[tokio::test]
async fn status_pushes_drive_state_transitions() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    let mut ev = c.subscribe();

    mock.push_status(|s| printing(s, 3, 50));
    let s = wait_state(&c, "printing", |s| s.status.state == MachineState::Printing)
        .await
        .status;
    assert_eq!(s.detail.as_deref(), Some("exposing"));
    assert_eq!(s.filename.as_deref(), Some("a.goo"));
    assert_eq!((s.current_layer, s.total_layers), (Some(50), Some(200)));
    assert_eq!(s.progress(), Some(0.25));
    assert_eq!((s.elapsed_s, s.total_s), (Some(60), Some(240)));
    assert_eq!((s.uv_led_temp, s.box_temp), (Some(35.5), Some(26.0)));
    assert_eq!(s.time_lapse, Some(true));
    assert_eq!(s.error_code, None);
    // attribute-derived fields survive status updates
    assert_eq!(s.free_storage, Some(5_000_000_000));
    assert_eq!(s.camera_connected, Some(true));
    match next_event(&mut ev, "status event", |e| matches!(e, SdcpEvent::Status(_))).await {
        SdcpEvent::Status(st) => assert_eq!(st.state, MachineState::Printing),
        _ => unreachable!(),
    }

    type Step = (fn(&mut Value), MachineState);
    let steps: &[Step] = &[
        (|s| s["PrintInfo"]["Status"] = json!(1), MachineState::Homing),
        (|s| s["PrintInfo"]["Status"] = json!(5), MachineState::Pausing),
        (|s| s["PrintInfo"]["Status"] = json!(6), MachineState::Paused),
        (|s| s["PrintInfo"]["Status"] = json!(4), MachineState::Printing),
        (|s| s["PrintInfo"]["Status"] = json!(7), MachineState::Stopping),
        (|s| s["PrintInfo"]["Status"] = json!(10), MachineState::FileChecking),
        (
            |s| {
                s["CurrentStatus"] = json!([0]);
                s["PrintInfo"]["Status"] = json!(9);
                s["PrintInfo"]["CurrentLayer"] = json!(200);
            },
            MachineState::Completed,
        ),
        (|s| s["PrintInfo"]["Status"] = json!(8), MachineState::Stopped),
        (|s| s["CurrentStatus"] = json!([2]), MachineState::FileTransfer),
        (|s| s["CurrentStatus"] = json!([3]), MachineState::ExposureTest),
        (|s| s["CurrentStatus"] = json!([4]), MachineState::SelfTest),
        (
            |s| {
                s["CurrentStatus"] = json!([0]);
                s["PrintInfo"]["Status"] = json!(0);
                s["PrintInfo"]["ErrorNumber"] = json!(4);
            },
            MachineState::Error,
        ),
        (|s| s["PrintInfo"]["ErrorNumber"] = json!(0), MachineState::Idle),
    ];
    for (f, want) in steps {
        mock.push_status(f);
        let s = wait_state(&c, &format!("{want:?}"), |s| s.status.state == *want)
            .await
            .status;
        if *want == MachineState::Error {
            assert_eq!(s.error_code, Some(4));
        }
        if *want == MachineState::Completed {
            assert_eq!(s.progress(), Some(1.0));
        }
    }
}

#[tokio::test]
async fn attribute_pushes_and_other_topics() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    let mut ev = c.subscribe();
    mock.push_attributes(|a| {
        a["RemainingMemory"] = json!(1234);
        a["CameraStatus"] = json!(0);
    });
    let s = wait_state(&c, "attributes", |s| s.status.free_storage == Some(1234)).await;
    assert_eq!(s.status.camera_connected, Some(false));
    assert_eq!(s.attributes.unwrap()["RemainingMemory"], 1234);
    assert!(
        matches!(next_event(&mut ev, "attr event", |e| matches!(e, SdcpEvent::Attributes(_))).await, SdcpEvent::Attributes(a) if a["CameraStatus"] == 0)
    );

    mock.push_json(&json!({"Data": {"Data": {"ErrorCode": 1}}, "Topic": format!("sdcp/error/{MAINBOARD_ID}")}));
    let e = next_event(&mut ev, "error event", |e| matches!(e, SdcpEvent::Error(_))).await;
    assert!(matches!(e, SdcpEvent::Error(v) if v["Data"]["Data"]["ErrorCode"] == 1));
    mock.push_json(&json!({"Data": {"Data": [{"Type": 1}]}, "Topic": format!("sdcp/notice/{MAINBOARD_ID}")}));
    let e = next_event(&mut ev, "notice event", |e| matches!(e, SdcpEvent::Notice(_))).await;
    assert!(matches!(e, SdcpEvent::Notice(_)));
}

#[tokio::test]
async fn older_message_shapes_are_understood() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    // pre-V3 firmwares wrap status and attributes in Data
    mock.push_json(&json!({
        "Id": BRAND_ID,
        "Data": {"Attributes": {"RemainingMemory": 77, "CameraStatus": 1}, "MainboardID": MAINBOARD_ID, "TimeStamp": 1},
        "Topic": format!("sdcp/attributes/{MAINBOARD_ID}")
    }));
    wait_state(&c, "nested attributes", |s| s.status.free_storage == Some(77)).await;
    mock.push_json(&json!({
        "Id": BRAND_ID,
        "Data": {"Status": {"CurrentStatus": 1, "PrintInfo": {"Status": 6, "CurrentLayer": 1, "TotalLayer": 4}}, "MainboardID": MAINBOARD_ID, "TimeStamp": 1},
        "Topic": format!("sdcp/status/{MAINBOARD_ID}")
    }));
    let s = wait_state(&c, "nested status", |s| s.status.state == MachineState::Paused).await;
    assert_eq!(s.status.progress(), Some(0.25));
    assert_eq!(s.status.free_storage, Some(77));
}

#[tokio::test]
async fn non_json_frames_are_skipped_and_binary_json_is_parsed() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    mock.push_raw(Message::Text("pong".into()));
    mock.push_raw(Message::Text("this is not json".into()));
    mock.push_raw(Message::Text("{\"Topic\": \"sdcp/unknown/x\"}".into()));
    let mut st = mock.state().status.clone();
    printing(&mut st, 6, 10);
    let msg = common::sdcp_mock::status_message(&st);
    mock.push_raw(Message::Binary(msg.to_string().into_bytes()));
    let s = wait_state(&c, "binary status", |s| s.status.state == MachineState::Paused).await;
    assert_eq!(s.status.current_layer, Some(10));
    // the connection is still usable
    c.refresh().await.unwrap();
}

#[tokio::test]
async fn every_command_sends_the_right_cmd_and_payload() {
    let mock = SdcpMock::start_with(|s| {
        s.files.insert("a.goo".into(), vec![1, 2, 3]);
        s.folders.push("/local/sub".into());
    })
    .await;
    let c = mock.client().await;

    c.refresh().await.unwrap();
    c.start_print("a.goo", 5).await.unwrap();
    c.pause().await.unwrap();
    c.resume().await.unwrap();
    c.stop().await.unwrap();
    c.rename("Workshop Mars").await.unwrap();
    c.set_time_lapse(true).await.unwrap();
    c.set_time_lapse(false).await.unwrap();
    assert_eq!(
        c.enable_video(true).await.unwrap().as_deref(),
        Some("rtsp://127.0.0.1:554/video")
    );
    assert_eq!(c.enable_video(false).await.unwrap(), None);
    c.list_files("/local").await.unwrap();
    c.delete(&["/local/a.goo".into()], &["/local/sub".into()])
        .await
        .unwrap();
    c.history().await.unwrap();
    c.task_details(&["t1".into(), "t2".into()]).await.unwrap();
    let raw = c.request(133, json!({"Custom": true})).await.unwrap();
    assert_eq!(raw, json!({"Ack": 0}), "request returns Data.Data");

    assert_eq!(
        sent(&mock),
        [
            (cmd::STATUS, json!({})),
            (cmd::START_PRINT, json!({"Filename": "a.goo", "StartLayer": 5})),
            (cmd::PAUSE, json!({})),
            (cmd::RESUME, json!({})),
            (cmd::STOP, json!({})),
            (cmd::RENAME, json!({"Name": "Workshop Mars"})),
            (cmd::TIME_LAPSE, json!({"Enable": 1})),
            (cmd::TIME_LAPSE, json!({"Enable": 0})),
            (cmd::VIDEO_STREAM, json!({"Enable": 1})),
            (cmd::VIDEO_STREAM, json!({"Enable": 0})),
            (cmd::FILE_LIST, json!({"Url": "/local"})),
            (
                cmd::DELETE_FILES,
                json!({"FileList": ["/local/a.goo"], "FolderList": ["/local/sub"]})
            ),
            (cmd::HISTORY, json!({})),
            (cmd::TASK_DETAILS, json!({"Id": ["t1", "t2"]})),
            (133, json!({"Custom": true})),
        ]
    );
    assert_eq!(
        (
            cmd::START_PRINT,
            cmd::PAUSE,
            cmd::STOP,
            cmd::RESUME,
            cmd::RENAME,
            cmd::FILE_LIST
        ),
        (128, 129, 130, 131, 192, 258)
    );
    assert_eq!(
        (
            cmd::DELETE_FILES,
            cmd::HISTORY,
            cmd::TASK_DETAILS,
            cmd::VIDEO_STREAM,
            cmd::TIME_LAPSE
        ),
        (259, 320, 321, 386, 387)
    );
    let st = mock.state();
    for r in &st.requests {
        assert_envelope(r);
    }
    let mut ids: Vec<&str> = st
        .requests
        .iter()
        .map(|r| r["Data"]["RequestID"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), st.requests.len(), "request ids must be unique");
    // the printer applied them
    assert_eq!(st.attributes["Name"], "Workshop Mars");
    assert!(st.files.is_empty() && st.folders.is_empty());
}

#[tokio::test]
async fn print_lifecycle_is_reflected_in_state() {
    let mock = SdcpMock::start_with(|s| {
        s.files.insert("a.goo".into(), vec![0; 10]);
    })
    .await;
    let c = mock.client().await;
    c.start_print("a.goo", 0).await.unwrap();
    // status is pushed before the response, so it is current when the call returns
    let s = c.state().status;
    assert_eq!(s.state, MachineState::Printing);
    assert_eq!(s.filename.as_deref(), Some("a.goo"));
    assert_eq!(s.total_s, Some(3600));
    c.pause().await.unwrap();
    assert_eq!(c.state().status.state, MachineState::Paused);
    c.resume().await.unwrap();
    assert_eq!(c.state().status.state, MachineState::Printing);
    c.stop().await.unwrap();
    assert_eq!(c.state().status.state, MachineState::Stopped);
    c.set_time_lapse(true).await.unwrap();
    assert_eq!(c.state().status.time_lapse, Some(true));
}

#[tokio::test]
async fn file_lists_internal_usb_and_folders() {
    let mock = SdcpMock::start_with(|s| {
        s.files.insert("a.goo".into(), vec![0; 3]);
        s.files.insert("b.ctb".into(), vec![0; 4]);
        s.files.insert("sub/c.goo".into(), vec![0; 5]);
        s.folders.push("/local/sub".into());
    })
    .await;
    let c = mock.client().await;
    let l: Vec<(String, bool, Option<u64>)> = c
        .list_files("/local")
        .await
        .unwrap()
        .into_iter()
        .map(|f| (f.path, f.is_dir, f.size))
        .collect();
    assert_eq!(
        l,
        [
            ("/local/sub".to_string(), true, None),
            ("/local/a.goo".to_string(), false, None),
            ("/local/b.ctb".to_string(), false, None),
        ]
    );
    let sub = c.list_files("/local/sub").await.unwrap();
    assert_eq!(sub.len(), 1);
    assert_eq!(sub[0].path, "/local/sub/c.goo");

    // no USB stick: the printer answers Ack -1
    match c.list_files("/usb").await {
        Err(NetError::Rejected(m)) => assert_eq!(m, "command 258 failed with code -1"),
        other => panic!("{other:?}"),
    }
    mock.state().usb = Some([("u.goo".to_string(), vec![0u8; 2])].into_iter().collect());
    let usb = c.list_files("/usb").await.unwrap();
    assert_eq!(usb.len(), 1);
    assert_eq!(usb[0].path, "/usb/u.goo");
    assert!(!usb[0].is_dir);

    // firmwares that report sizes
    mock.set_handler(|_, cmd, _| {
        (cmd == 258).then(|| {
            Action::Reply(json!({"Ack": 0, "FileList": [
                {"name": "/local/x.goo", "type": 1, "FileSize": 99},
                {"name": "/local/y.goo", "type": 1, "size": 7},
                {"type": 1}
            ]}))
        })
    });
    let l = c.list_files("/local").await.unwrap();
    assert_eq!(l[0].size, Some(99));
    assert_eq!(l[1].size, Some(7));
    assert_eq!(l[2].path, "", "entries without a name do not break the list");
    // missing FileList is an empty directory
    mock.set_handler(|_, cmd, _| (cmd == 258).then(|| Action::Reply(json!({"Ack": 0}))));
    assert!(c.list_files("/local").await.unwrap().is_empty());
}

#[tokio::test]
async fn delete_reports_what_failed() {
    let mock = SdcpMock::start_with(|s| {
        s.files.insert("a.goo".into(), vec![0]);
        s.folders.push("/local/sub".into());
    })
    .await;
    let c = mock.client().await;
    let failed = c
        .delete(
            &["/local/a.goo".into(), "/local/missing.goo".into()],
            &["/local/sub".into(), "/local/nosub".into()],
        )
        .await
        .unwrap();
    assert_eq!(failed, ["/local/missing.goo", "/local/nosub"]);
    assert!(mock.state().files.is_empty());
}

#[tokio::test]
async fn history_and_task_details() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    assert_eq!(c.history().await.unwrap(), ["task-3", "task-2", "task-1"]);
    let d = c.task_details(&["task-3".into()]).await.unwrap();
    let list = d.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["TaskId"], "task-3");
    assert_eq!(list[0]["TaskName"], "/local/task-3.goo");
    assert_eq!(list[0]["TaskStatus"], 1);
    // missing HistoryData / HistoryDetailList
    mock.set_handler(|_, cmd, _| matches!(cmd, 320 | 321).then(|| Action::Reply(json!({"Ack": 0}))));
    assert!(c.history().await.unwrap().is_empty());
    assert_eq!(c.task_details(&["x".into()]).await.unwrap(), Value::Null);
}

#[tokio::test]
async fn error_acks_become_readable_errors() {
    let mock = SdcpMock::start_with(|s| {
        s.files.insert("a.goo".into(), vec![0]);
    })
    .await;
    let c = mock.client().await;
    let rejected = |r: Result<(), NetError>| match r {
        Err(NetError::Rejected(m)) => m,
        other => panic!("expected a rejection, got {other:?}"),
    };
    assert_eq!(
        rejected(c.start_print("missing.goo", 0).await),
        "file not found (code 2)"
    );
    c.start_print("a.goo", 0).await.unwrap();
    assert_eq!(rejected(c.start_print("a.goo", 0).await), "printer is busy (code 1)");

    let codes = [
        (3, "MD5 verification failed (code 3)"),
        (4, "file read failed (code 4)"),
        (5, "resolution does not match this printer (code 5)"),
        (6, "unrecognised file format (code 6)"),
        (7, "file was sliced for a different machine model (code 7)"),
        (42, "command 128 failed with code 42"),
    ];
    for (code, text) in codes {
        mock.set_handler(move |_, cmd, _| (cmd == 128).then(|| Action::Reply(json!({"Ack": code}))));
        assert_eq!(rejected(c.start_print("a.goo", 0).await), text);
    }
    mock.set_handler(|_, cmd, d| match (cmd, d["Enable"].as_i64()) {
        (386, Some(1)) => Some(Action::Reply(json!({"Ack": 1}))),
        (386, _) => Some(Action::Reply(json!({"Ack": 2}))),
        (129, _) => Some(Action::Reply(json!({"Ack": 9}))),
        (255, _) => Some(Action::Reply(json!({"Ack": 2}))),
        _ => None,
    });
    match c.enable_video(true).await {
        Err(NetError::Rejected(m)) => assert_eq!(m, "too many camera streams are open (code 1)"),
        other => panic!("{other:?}"),
    }
    match c.enable_video(false).await {
        Err(NetError::Rejected(m)) => assert_eq!(m, "printer has no camera (code 2)"),
        other => panic!("{other:?}"),
    }
    assert_eq!(rejected(c.pause().await), "command 129 failed with code 9");
    match c.request(cmd::TERMINATE_TRANSFER, json!({})).await {
        Err(NetError::Rejected(m)) => assert_eq!(m, "file is already being verified (code 2)"),
        other => panic!("{other:?}"),
    }
    // a response without Ack (or without Data) counts as success
    mock.set_handler(|_, cmd, _| (cmd == 131).then(|| Action::Reply(json!({}))));
    c.resume().await.unwrap();
}

#[tokio::test]
async fn responses_are_matched_by_request_id() {
    let mock = SdcpMock::start_with(|s| {
        s.files.insert("slow.goo".into(), vec![0]);
    })
    .await;
    let c = mock.client().await;
    mock.set_handler(|st, cmd, _| {
        (cmd == 258).then(|| {
            let list: Vec<Value> = st
                .files
                .keys()
                .map(|k| json!({"name": format!("/local/{k}"), "type": 1}))
                .collect();
            Action::Delay(Duration::from_millis(400), json!({"Ack": 0, "FileList": list}))
        })
    });
    // a stray response nobody asked for is ignored
    mock.push_json(&response_message(
        &json!({"Data": {"Cmd": 258, "RequestID": "deadbeefdeadbeef"}}),
        json!({"Ack": 0, "FileList": [{"name": "/local/WRONG", "type": 1}]}),
    ));
    let t0 = Instant::now();
    let (files, history) = tokio::join!(
        async {
            let r = c.list_files("/local").await;
            (r, t0.elapsed())
        },
        async {
            let r = c.history().await;
            (r, t0.elapsed())
        }
    );
    let (files, files_t) = files;
    let (history, history_t) = history;
    assert_eq!(files.unwrap()[0].path, "/local/slow.goo");
    assert_eq!(history.unwrap(), ["task-3", "task-2", "task-1"]);
    assert!(
        history_t < files_t,
        "history ({history_t:?}) waited for the slow file list ({files_t:?})"
    );
    assert!(files_t >= Duration::from_millis(400));
}

#[tokio::test]
async fn unanswered_request_times_out_and_late_answers_are_dropped() {
    let mock = SdcpMock::start().await;
    let mut c = mock.client().await;
    c.set_request_timeout(Duration::from_millis(300));
    mock.set_handler(|_, cmd, _| match cmd {
        129 => Some(Action::NoReply),
        131 => Some(Action::Delay(Duration::from_millis(700), json!({"Ack": 0}))),
        _ => None,
    });
    let t0 = Instant::now();
    assert!(matches!(within(3.0, "pause", c.pause()).await, Err(NetError::Timeout)));
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert!(matches!(
        within(3.0, "resume", c.resume()).await,
        Err(NetError::Timeout)
    ));
    // the late answer to resume arrives now and must not confuse the next request
    tokio::time::sleep(Duration::from_millis(600)).await;
    mock.set_handler(|_, cmd, _| (cmd == 130).then(|| Action::Delay(Duration::from_millis(100), json!({"Ack": 7}))));
    match within(3.0, "stop", c.stop()).await {
        Err(NetError::Rejected(m)) => assert_eq!(m, "command 130 failed with code 7"),
        other => panic!("{other:?}"),
    }
    c.refresh().await.unwrap();
}

#[tokio::test]
async fn dropped_connection_fails_requests_immediately() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    let mut ev = c.subscribe();
    mock.set_handler(|_, cmd, _| (cmd == 130).then_some(Action::Close));
    let t0 = Instant::now();
    let r = within(5.0, "stop", c.stop()).await;
    assert!(matches!(r, Err(NetError::Closed)), "{r:?}");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "request hung for {:?}",
        t0.elapsed()
    );
    next_event(&mut ev, "disconnect", |e| matches!(e, SdcpEvent::Disconnected)).await;
    assert_eq!(
        wait_state(&c, "offline", |s| s.status.state == MachineState::Offline)
            .await
            .status
            .state,
        MachineState::Offline
    );
    // later requests fail fast too
    let t0 = Instant::now();
    assert!(matches!(within(5.0, "pause", c.pause()).await, Err(NetError::Closed)));
    assert!(t0.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn printer_closing_the_socket_marks_it_offline() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    let mut ev = c.subscribe();
    mock.close_all();
    next_event(&mut ev, "disconnect", |e| matches!(e, SdcpEvent::Disconnected)).await;
    wait_state(&c, "offline", |s| s.status.state == MachineState::Offline).await;
}

#[tokio::test]
async fn connect_errors() {
    let mock = SdcpMock::start().await;
    // nothing listening on the WebSocket port
    let r = within(
        10.0,
        "connect",
        SdcpClient::connect_with_port(&mock.printer(), common::free_tcp_port()),
    )
    .await;
    assert!(r.is_err());
    let mut p: DiscoveredPrinter = mock.printer();
    p.protocol = Protocol::ChituUdp;
    assert!(matches!(
        SdcpClient::connect_with_port(&p, mock.addr.port()).await,
        Err(NetError::Unsupported(_))
    ));
    let mut p = mock.printer();
    p.mainboard_id = None;
    assert!(matches!(
        SdcpClient::connect_with_port(&p, mock.addr.port()).await,
        Err(NetError::Protocol(_))
    ));
    assert_eq!(mock.state().ws_connections, 0);
}

#[tokio::test]
async fn minimal_firmware_without_snapshot_answers_still_connects() {
    let mock = SdcpMock::start().await;
    mock.set_handler(|_, cmd, _| matches!(cmd, 0 | 1).then(|| Action::Reply(json!({"Ack": 3}))));
    let c = within(5.0, "connect", mock.client()).await;
    assert_eq!(c.state().status.state, MachineState::Unknown);
    assert!(c.state().attributes.is_none());
}

#[tokio::test]
async fn printer_client_over_sdcp() {
    let mock = SdcpMock::start_with(|s| {
        s.files.insert("a.goo".into(), vec![0; 4]);
    })
    .await;
    let pc = within(
        8.0,
        "connect",
        PrinterClient::connect_with_ports(mock.addr.ip(), mock.udp_addr.port(), mock.addr.port()),
    )
    .await
    .expect("connect");
    assert_eq!(mock.state().discovery_requests, 1);
    assert_eq!(pc.protocol(), Protocol::Sdcp);
    assert!(pc.supports_camera());
    assert!(pc.can_download("/local/x"));
    assert!(!pc.can_download("/usb/x"));
    assert_eq!(pc.remote_name("Mäx (1).goo"), "Mäx (1).goo");
    let s = pc.state();
    assert_eq!(s.status.state, MachineState::Idle);
    assert_eq!(s.status.free_storage, Some(5_000_000_000));
    assert_eq!(s.attributes.as_ref().unwrap()["Name"], "Mock Mars");

    // pushes reach the unified state
    let mut rx = pc.watch();
    mock.push_status(|s| printing(s, 3, 100));
    let st = within(
        5.0,
        "printing",
        rx.wait_for(|s| s.status.state == MachineState::Printing),
    )
    .await
    .unwrap()
    .clone();
    assert_eq!(st.status.progress(), Some(0.5));
    assert!(st.attributes.is_some());
    mock.push_status(|s| s["CurrentStatus"] = json!([0]));

    let files = pc.list_files("/local").await.unwrap();
    assert_eq!(files.len(), 1);
    let h = pc.history(2).await.unwrap();
    assert_eq!(h.as_array().unwrap().len(), 2);
    pc.start_print("a.goo").await.unwrap();
    pc.pause().await.unwrap();
    pc.resume().await.unwrap();
    pc.stop().await.unwrap();
    pc.refresh().await.unwrap();
    pc.set_time_lapse(true).await.unwrap();
    pc.rename("Renamed").await.unwrap();
    assert_eq!(
        pc.enable_video(true).await.unwrap().as_deref(),
        Some("rtsp://127.0.0.1:554/video")
    );
    assert_eq!(
        pc.delete(&["/local/a.goo".into(), "/local/zz.goo".into()])
            .await
            .unwrap(),
        ["/local/zz.goo"]
    );
    assert_eq!(
        sent(&mock),
        [
            (cmd::FILE_LIST, json!({"Url": "/local"})),
            (cmd::HISTORY, json!({})),
            (cmd::TASK_DETAILS, json!({"Id": ["task-3", "task-2"]})),
            (cmd::START_PRINT, json!({"Filename": "a.goo", "StartLayer": 0})),
            (cmd::PAUSE, json!({})),
            (cmd::RESUME, json!({})),
            (cmd::STOP, json!({})),
            (cmd::STATUS, json!({})),
            (cmd::TIME_LAPSE, json!({"Enable": 1})),
            (cmd::RENAME, json!({"Name": "Renamed"})),
            (cmd::VIDEO_STREAM, json!({"Enable": 1})),
            (
                cmd::DELETE_FILES,
                json!({"FileList": ["/local/a.goo", "/local/zz.goo"], "FolderList": []})
            ),
        ]
    );

    // no history: no detail request
    mock.state().history.clear();
    assert_eq!(pc.history(5).await.unwrap(), json!([]));
    assert_ne!(sent(&mock).last().unwrap().0, cmd::TASK_DETAILS);

    // the printer goes away
    mock.close_all();
    within(5.0, "offline", rx.wait_for(|s| s.status.state == MachineState::Offline))
        .await
        .unwrap();
}

#[tokio::test]
async fn printer_client_connects_to_the_address_it_was_given() {
    // the reply claims another (loopback) address; we must still use ours
    let mock = SdcpMock::start_with(|s| s.attributes["MainboardIP"] = json!("::1")).await;
    let pc = within(
        8.0,
        "connect",
        PrinterClient::connect_with_ports(mock.addr.ip(), mock.udp_addr.port(), mock.addr.port()),
    )
    .await
    .expect("connect to the address that answered");
    assert_eq!(pc.protocol(), Protocol::Sdcp);
    assert_eq!(mock.state().ws_connections, 1);
}
