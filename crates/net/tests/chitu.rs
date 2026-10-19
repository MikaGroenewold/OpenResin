//! Legacy Chitu WiFi protocol against a fake mainboard on 127.0.0.1 (UDP).

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use openresin_net::chitu::{ChituClient, CHUNK_SIZE};
use openresin_net::{MachineState, NetError, PrinterClient, Protocol};

use common::chitu_mock::{control_commands, ChituMock, Packet};
use common::{random_bytes, within, ProgressLog, TempDir};

async fn connect(mock: &ChituMock) -> ChituClient {
    within(5.0, "connect", ChituClient::connect_addr(mock.addr))
        .await
        .expect("connect")
}

fn controls(mock: &ChituMock) -> Vec<String> {
    control_commands(&mock.state().commands)
}

#[tokio::test]
async fn connect_verifies_the_printer_answers() {
    let mock = ChituMock::start().await;
    let c = connect(&mock).await;
    assert_eq!(c.ip, mock.addr.ip());
    assert_eq!(mock.state().commands.first().map(String::as_str), Some("M4000"));
    let r = c.command("M115").await.unwrap();
    assert_eq!(r, "ok");
    assert!(mock.state().commands.contains(&"M115".to_string()));
}

#[tokio::test]
async fn silent_printer_fails_to_connect_in_time() {
    let mock = ChituMock::start_with(|s| s.silent = true).await;
    let started = Instant::now();
    let r = within(8.0, "connect", ChituClient::connect_addr(mock.addr)).await;
    assert!(matches!(r, Err(NetError::Timeout)), "{:?}", r.err());
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn status_follows_print_state() {
    let mock = ChituMock::start_with(|s| {
        s.files.insert("model.ctb".into(), vec![0; 1000]);
    })
    .await;
    let c = connect(&mock).await;

    c.refresh().await.unwrap();
    let s = c.status();
    assert_eq!(s.state, MachineState::Idle, "idle printer reported as {:?}", s.state);
    assert_eq!(s.detail.as_deref(), Some("legacy WiFi"));
    assert_eq!(s.current_layer, None);
    assert_eq!(s.progress(), None);

    c.start_print("model.ctb").await.unwrap();
    mock.state().done = 250;
    c.refresh().await.unwrap();
    let s = c.status();
    assert_eq!(s.state, MachineState::Printing);
    // byte progress is reported as permille "layers"
    assert_eq!((s.current_layer, s.total_layers), (Some(250), Some(1000)));
    assert_eq!(s.progress(), Some(0.25));

    c.pause().await.unwrap();
    c.refresh().await.unwrap();
    assert_eq!(c.status().state, MachineState::Paused);

    c.resume().await.unwrap();
    mock.state().done = 500;
    c.refresh().await.unwrap();
    let s = c.status();
    assert_eq!(s.state, MachineState::Printing);
    assert_eq!(s.progress(), Some(0.5));

    c.stop().await.unwrap();
    c.refresh().await.unwrap();
    assert_eq!(c.status().state, MachineState::Idle);

    assert_eq!(controls(&mock), ["M6030 ':model.ctb'", "M25", "M24", "M33 I5"]);
}

#[tokio::test]
async fn paused_flag_from_m4000_alone() {
    // printer paused from its touch screen: only the D: flag says so
    let mock = ChituMock::start_with(|s| {
        s.printing = Some("x.ctb".into());
        s.done = 10;
        s.total = 40;
        s.paused = true;
    })
    .await;
    let c = connect(&mock).await;
    c.refresh().await.unwrap();
    assert_eq!(c.status().state, MachineState::Paused);
    assert_eq!(c.status().current_layer, Some(250));
}

#[tokio::test]
async fn background_poll_publishes_status() {
    let mock = ChituMock::start_with(|s| {
        s.printing = Some("x.ctb".into());
        s.done = 1;
        s.total = 4;
    })
    .await;
    let c = connect(&mock).await;
    let mut rx = c.watch();
    let s = within(5.0, "first poll", rx.wait_for(|s| s.state != MachineState::Unknown))
        .await
        .unwrap()
        .clone();
    assert_eq!(s.state, MachineState::Printing);
    assert_eq!(s.current_layer, Some(250));
    // the printer disappears: the poll reports it offline
    mock.state().silent = true;
    let s = within(12.0, "offline", rx.wait_for(|s| s.state == MachineState::Offline))
        .await
        .unwrap()
        .clone();
    assert_eq!(s.state, MachineState::Offline);
}

#[tokio::test]
async fn command_errors_are_reported() {
    let mock = ChituMock::start().await;
    let c = connect(&mock).await;
    match c.start_print("missing.ctb").await {
        Err(NetError::Rejected(m)) => assert_eq!(m, "Error: file not found"),
        other => panic!("{other:?}"),
    }
    match c.delete("missing.ctb").await {
        Err(NetError::Rejected(m)) => assert!(m.contains("Deletion failed"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn file_list_in_one_datagram() {
    let mock = ChituMock::start_with(|s| {
        s.files.insert("a.ctb".into(), vec![0; 12345]);
        s.files.insert("my model (2).ctb".into(), vec![0; 7]);
    })
    .await;
    let c = connect(&mock).await;
    let files = c.list_files().await.unwrap();
    let got: Vec<(String, bool, Option<u64>)> = files.into_iter().map(|f| (f.path, f.is_dir, f.size)).collect();
    assert_eq!(
        got,
        [
            ("a.ctb".to_string(), false, Some(12345)),
            ("my model (2).ctb".to_string(), false, Some(7))
        ]
    );
    assert_eq!(controls(&mock), ["M20"]);
}

#[tokio::test]
async fn file_list_split_over_datagrams() {
    let mock = ChituMock::start_with(|s| {
        s.m20_split = true;
        for i in 0..5 {
            s.files.insert(format!("f{i}.ctb"), vec![0; i * 100]);
        }
    })
    .await;
    let c = connect(&mock).await;
    let started = Instant::now();
    let files = c.list_files().await.unwrap();
    assert_eq!(files.len(), 5);
    assert_eq!(files[3].path, "f3.ctb");
    assert_eq!(files[3].size, Some(300));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "did not stop at 'End file list'"
    );
}

#[tokio::test]
async fn empty_file_list() {
    let mock = ChituMock::start().await;
    let c = connect(&mock).await;
    assert!(c.list_files().await.unwrap().is_empty());
}

#[tokio::test]
async fn delete_removes_the_file() {
    let mock = ChituMock::start_with(|s| {
        s.files.insert("old.ctb".into(), vec![1]);
    })
    .await;
    let c = connect(&mock).await;
    c.delete("old.ctb").await.unwrap();
    assert!(mock.state().files.is_empty());
    assert_eq!(controls(&mock), ["M30 old.ctb"]);
}

async fn upload(c: &ChituClient, data: &[u8], name: &str) -> (Result<(), NetError>, ProgressLog) {
    let dir = TempDir::new("chitu-up");
    let path = dir.file("in.ctb", data);
    let log = ProgressLog::default();
    let cb = log.callback();
    let r = within(30.0, "upload", c.upload(&path, name, &cb, &AtomicBool::new(false))).await;
    (r, log)
}

fn expected_packets(len: usize) -> Vec<Packet> {
    (0..len.div_ceil(CHUNK_SIZE))
        .map(|i| Packet {
            offset: (i * CHUNK_SIZE) as u32,
            len: CHUNK_SIZE.min(len - i * CHUNK_SIZE),
            checksum_ok: true,
        })
        .collect()
}

#[tokio::test]
async fn upload_reassembles_exactly() {
    let mock = ChituMock::start().await;
    let c = connect(&mock).await;
    let data = random_bytes(50_000, 1);
    let (r, log) = upload(&c, &data, "part.ctb").await;
    r.unwrap();
    let st = mock.state();
    assert_eq!(st.files.get("part.ctb").map(Vec::as_slice), Some(&data[..]));
    assert_eq!(st.packets, expected_packets(data.len()));
    assert_eq!(control_commands(&st.commands), ["M28 part.ctb", "M29"]);
    drop(st);
    log.assert_complete(data.len() as u64);
    assert_eq!(log.calls()[0], (0, data.len() as u64));
    assert_eq!(log.calls().len(), 1 + data.len().div_ceil(CHUNK_SIZE));
}

#[tokio::test]
async fn upload_of_exact_chunk_multiple_and_tiny_files() {
    let mock = ChituMock::start().await;
    let c = connect(&mock).await;
    let data = random_bytes(CHUNK_SIZE * 2, 2);
    upload(&c, &data, "two.ctb").await.0.unwrap();
    let tiny = vec![42u8];
    upload(&c, &tiny, "one.ctb").await.0.unwrap();
    let st = mock.state();
    assert_eq!(st.files["two.ctb"], data);
    assert_eq!(st.files["one.ctb"], tiny);
    assert_eq!(st.packets.len(), 3);
}

#[tokio::test]
async fn upload_resends_lost_packets() {
    let lost = (3 * CHUNK_SIZE) as u32;
    let mock = ChituMock::start_with(|s| s.lose_offsets = vec![lost]).await;
    let c = connect(&mock).await;
    let data = random_bytes(10 * CHUNK_SIZE + 17, 3);
    let (r, log) = upload(&c, &data, "lossy.ctb").await;
    r.unwrap();
    let st = mock.state();
    assert_eq!(st.files["lossy.ctb"], data);
    let at_lost = st.packets.iter().filter(|p| p.offset == lost).count();
    assert_eq!(at_lost, 2, "lost packet was not sent again");
    drop(st);
    log.assert_complete(data.len() as u64);
}

#[tokio::test]
async fn upload_honours_resend_requests() {
    let at = (2 * CHUNK_SIZE) as u32;
    let mock = ChituMock::start_with(|s| s.resend_offsets = vec![at, at]).await;
    let c = connect(&mock).await;
    let data = random_bytes(5 * CHUNK_SIZE, 4);
    upload(&c, &data, "resend.ctb").await.0.unwrap();
    let st = mock.state();
    assert_eq!(st.files["resend.ctb"], data);
    assert_eq!(st.packets.iter().filter(|p| p.offset == at).count(), 3);
}

#[tokio::test]
async fn upload_fails_when_printer_wants_other_data() {
    let mock = ChituMock::start_with(|s| s.resend_other = Some((CHUNK_SIZE as u32, 0))).await;
    let c = connect(&mock).await;
    let data = random_bytes(3 * CHUNK_SIZE, 5);
    match upload(&c, &data, "x.ctb").await.0 {
        Err(NetError::Protocol(m)) => assert!(m.contains("offset 0"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn upload_can_be_cancelled() {
    let mock = ChituMock::start().await;
    let c = connect(&mock).await;
    let dir = TempDir::new("chitu-cancel");
    let data = random_bytes(40 * CHUNK_SIZE, 6);
    let path = dir.file("in.ctb", &data);
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let cb = move |sent: u64, _total: u64| {
        if sent >= (5 * CHUNK_SIZE) as u64 {
            c2.store(true, Ordering::Relaxed);
        }
    };
    let r = within(10.0, "upload", c.upload(&path, "big.ctb", &cb, &cancel)).await;
    assert!(matches!(r, Err(NetError::Cancelled)), "{r:?}");
    let st = mock.state();
    assert_eq!(st.packets.len(), 5, "kept sending after cancel");
    // the partial file is closed and removed again
    assert_eq!(control_commands(&st.commands), ["M28 big.ctb", "M29", "M30 big.ctb"]);
    assert!(!st.files.contains_key("big.ctb"));
}

#[tokio::test]
async fn printer_client_auto_detects_chitu() {
    let mock = ChituMock::start_with(|s| {
        s.files.insert("a.ctb".into(), vec![0; 10]);
    })
    .await;
    let unused_tcp = common::free_tcp_port();
    let pc = within(
        8.0,
        "connect",
        PrinterClient::connect_with_ports(mock.addr.ip(), mock.addr.port(), unused_tcp),
    )
    .await
    .expect("connect");
    assert_eq!(pc.protocol(), Protocol::ChituUdp);
    assert!(!pc.supports_camera());
    assert!(!pc.can_download("/local/a.ctb"));
    assert_eq!(pc.remote_name("Mäx (1).ctb"), "M_x__1_.ctb");

    // status is forwarded from the poll task
    let mut rx = pc.watch();
    within(5.0, "status", rx.wait_for(|s| s.status.state == MachineState::Idle))
        .await
        .unwrap();
    assert!(pc.state().attributes.is_none());

    // features the protocol does not have
    for r in [
        pc.read_range("/local/a.ctb", 0, 4).await.map(|_| ()),
        pc.set_time_lapse(true).await,
        pc.rename("x").await,
        pc.enable_video(true).await.map(|_| ()),
    ] {
        assert!(matches!(r, Err(NetError::Unsupported(_))), "{r:?}");
    }
    let dir = TempDir::new("chitu-pc");
    let r = pc
        .download(
            "/local/a.ctb",
            &dir.0.join("a.ctb"),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .await;
    assert!(matches!(r, Err(NetError::Unsupported(_))));
    assert_eq!(pc.history(10).await.unwrap(), serde_json::json!([]));

    let files = pc.list_files("/ignored").await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "a.ctb");

    // upload through the unified client, under the sanitized name
    let data = random_bytes(3000, 7);
    let path = dir.file("in.ctb", &data);
    let name = pc.remote_name("my part.ctb");
    pc.upload(&path, &name, &|_, _| {}, &AtomicBool::new(false))
        .await
        .unwrap();
    assert_eq!(mock.state().files["my_part.ctb"], data);

    pc.start_print("my_part.ctb").await.unwrap();
    pc.pause().await.unwrap();
    pc.resume().await.unwrap();
    pc.stop().await.unwrap();
    pc.refresh().await.unwrap();

    let failed = pc.delete(&["a.ctb".into(), "nope.ctb".into()]).await.unwrap();
    assert_eq!(failed, ["nope.ctb"]);
    assert!(!mock.state().files.contains_key("a.ctb"));

    assert_eq!(
        controls(&mock),
        [
            "M20",
            "M28 my_part.ctb",
            "M29",
            "M6030 ':my_part.ctb'",
            "M25",
            "M24",
            "M33 I5",
            "M30 a.ctb",
            "M30 nope.ctb"
        ]
    );
}

#[tokio::test]
async fn printer_client_reports_unreachable_host() {
    let port = common::free_udp_port();
    let started = Instant::now();
    let r = within(
        12.0,
        "connect",
        PrinterClient::connect_with_ports("127.0.0.1".parse().unwrap(), port, common::free_tcp_port()),
    )
    .await;
    match r {
        Err(NetError::Protocol(m)) => assert!(m.contains("no printer answered at 127.0.0.1"), "{m}"),
        Err(e) => panic!("unexpected error {e}"),
        Ok(_) => panic!("connected to nothing"),
    }
    assert!(started.elapsed() < Duration::from_secs(10));
}
