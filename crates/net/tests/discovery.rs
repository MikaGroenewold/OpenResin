//! Discovery: reply parsing and the UDP request/collect loop (loopback only;
//! nothing is broadcast).

mod common;

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openresin_net::discovery::{discover_addr, discover_on, parse_reply, DISCOVERY_MESSAGE};
use openresin_net::Protocol;
use tokio::net::UdpSocket;

use common::chitu_mock::ChituMock;
use common::sdcp_mock::{SdcpMock, BRAND_ID, MAINBOARD_ID};
use common::within;

fn from() -> IpAddr {
    "127.0.0.1".parse().unwrap()
}

#[test]
fn sdcp_v3_reply() {
    let r = br#"{"Id":"979d4C788A4a78bC777A870F1A02867A","Data":{"Name":"Mars","MachineName":"ELEGOO Mars 5 Ultra","BrandName":"ELEGOO","MainboardIP":"192.0.2.42","MainboardID":"000000000001d354","ProtocolVersion":"V3.0.0","FirmwareVersion":"V1.5.0","Extra":7}}"#;
    let p = parse_reply(r, from()).unwrap();
    assert_eq!(p.protocol, Protocol::Sdcp);
    assert_eq!(
        p.ip.to_string(),
        "192.0.2.42",
        "MainboardIP wins over the source address"
    );
    assert_eq!(p.name, "Mars");
    assert_eq!(p.model.as_deref(), Some("ELEGOO Mars 5 Ultra"));
    assert_eq!(p.brand.as_deref(), Some("ELEGOO"));
    assert_eq!(p.id.as_deref(), Some("979d4C788A4a78bC777A870F1A02867A"));
    assert_eq!(p.mainboard_id.as_deref(), Some("000000000001d354"));
    assert_eq!(p.protocol_version.as_deref(), Some("V3.0.0"));
    assert_eq!(p.firmware_version.as_deref(), Some("V1.5.0"));
    assert_eq!(p.extra.get("Extra").map(String::as_str), Some("7"));
    assert!(!p.extra.contains_key("Name"), "known fields are not repeated in extra");
    // display line used by the CLI
    let line = p.to_string();
    assert!(
        line.contains("SDCP V3.0.0") && line.contains("Mars") && line.contains("ELEGOO Mars 5 Ultra"),
        "{line}"
    );
}

#[test]
fn sdcp_reply_variants() {
    // padded with NULs and whitespace, no MainboardIP: source address is used
    let mut r = br#"  {"Id":"x","Data":{"Name":"Saturn","MainboardID":"abc","ProtocolVersion":"V3.0.0"}}"#.to_vec();
    r.extend_from_slice(b"\0\0\r\n");
    let p = parse_reply(&r, from()).unwrap();
    assert_eq!(p.ip, from());
    assert_eq!(p.name, "Saturn");
    assert_eq!(p.model, None);

    // flat document without the Data wrapper
    let p = parse_reply(br#"{"MachineName":"Saturn 4 Ultra","MainboardID":"m1"}"#, from()).unwrap();
    assert_eq!(p.name, "Saturn 4 Ultra", "MachineName is the fallback name");
    assert_eq!(p.mainboard_id.as_deref(), Some("m1"));
    assert_eq!(p.id, None);

    // unparsable MainboardIP falls back to the source address
    let p = parse_reply(
        br#"{"Data":{"Name":"n","MainboardID":"m","MainboardIP":"not-an-ip"}}"#,
        from(),
    )
    .unwrap();
    assert_eq!(p.ip, from());
}

#[test]
fn older_sdcp_reply_with_attributes() {
    // pre-V3 boards (e.g. early Saturn 3 Ultra firmware) nest everything in Data.Attributes
    let r = br#"{"Id":"0a69ee780fbd40d7bfb95b312250bf46","Data":{"Attributes":{"Name":"Saturn3Ultra","MachineName":"Saturn3Ultra","BrandName":"ELEGOO","ProtocolVersion":"V1.0.0","FirmwareVersion":"V1.4.2","MainboardIP":"192.0.2.7","MainboardID":"ff80800000000001"},"Status":{"CurrentStatus":0}}}"#;
    let p = parse_reply(r, from()).unwrap();
    assert_eq!(p.protocol, Protocol::Sdcp);
    assert_eq!(p.name, "Saturn3Ultra");
    assert_eq!(p.mainboard_id.as_deref(), Some("ff80800000000001"));
    assert_eq!(p.protocol_version.as_deref(), Some("V1.0.0"));
    assert_eq!(p.firmware_version.as_deref(), Some("V1.4.2"));
    assert_eq!(p.ip.to_string(), "192.0.2.7");
    assert_eq!(p.id.as_deref(), Some("0a69ee780fbd40d7bfb95b312250bf46"));
}

#[test]
fn legacy_chitu_reply() {
    let r = b"ok MAC:00:e0:4c:68:01:02 IP:192.0.2.50 VER:V1.4.3 ID:2a,3b,4c NAME:CBD Printer";
    let p = parse_reply(r, from()).unwrap();
    assert_eq!(p.protocol, Protocol::ChituUdp);
    assert_eq!(p.ip.to_string(), "192.0.2.50");
    assert_eq!(p.name, "CBD Printer", "names with spaces are kept");
    assert_eq!(p.firmware_version.as_deref(), Some("V1.4.3"));
    assert_eq!(p.id.as_deref(), Some("2a,3b,4c"));
    assert_eq!(p.extra.get("MAC").map(String::as_str), Some("00:e0:4c:68:01:02"));
    assert!(p.to_string().contains("Chitu UDP"));

    // without "ok" and without IP: the source address is used
    let p = parse_reply(b"MAC:00:11:22:33:44:55 NAME:Box", from()).unwrap();
    assert_eq!(p.ip, from());
    assert_eq!(p.name, "Box");
    let p = parse_reply(b"ok MAC:00:11:22:33:44:55", from()).unwrap();
    assert_eq!(p.name, "Chitu printer");
}

#[test]
fn malformed_replies_are_ignored() {
    let bad: &[&[u8]] = &[
        b"",
        b"\0\0\0",
        b"ok",
        b"ok   ",
        b"{",
        b"{\"Data\": [1,2",
        b"[1,2,3]",
        b"{}",
        b"{\"Id\": 5, \"Data\": {\"Foo\": 1}}",
        b"hello world",
        b"error: unknown command",
        b"OK:1",
        b"\xff\xfe\xfd",
        DISCOVERY_MESSAGE,
    ];
    for b in bad {
        assert!(
            parse_reply(b, from()).is_none(),
            "accepted {:?}",
            String::from_utf8_lossy(b)
        );
    }
}

/// A UDP responder that answers each `M99999` with the given datagrams.
async fn responder(replies: Vec<Vec<u8>>) -> (SocketAddr, Arc<Mutex<usize>>, tokio::task::JoinHandle<()>) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    let count = Arc::new(Mutex::new(0usize));
    let c2 = count.clone();
    let task = tokio::spawn(async move {
        let mut b = [0u8; 1024];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut b).await else {
                continue;
            };
            if &b[..n] == DISCOVERY_MESSAGE {
                *c2.lock().unwrap() += 1;
                for r in &replies {
                    let _ = sock.send_to(r, from).await;
                }
            }
        }
    });
    (addr, count, task)
}

#[tokio::test]
async fn unicast_discovery_of_sdcp_mock_returns_on_first_reply() {
    let mock = SdcpMock::start().await;
    let started = Instant::now();
    let p = within(5.0, "discover", discover_addr(mock.udp_addr, Duration::from_secs(3)))
        .await
        .unwrap()
        .expect("printer found");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "waited for the full timeout: {:?}",
        started.elapsed()
    );
    assert_eq!(p.protocol, Protocol::Sdcp);
    assert_eq!(p.ip, from());
    assert_eq!(p.mainboard_id.as_deref(), Some(MAINBOARD_ID));
    assert_eq!(p.id.as_deref(), Some(BRAND_ID));
    assert_eq!(p.name, "Mock Mars");
    assert_eq!(mock.state().discovery_requests, 1);
}

#[tokio::test]
async fn unicast_discovery_of_chitu_mock() {
    let mock = ChituMock::start().await;
    let p = discover_addr(mock.addr, Duration::from_secs(3))
        .await
        .unwrap()
        .expect("printer found");
    assert_eq!(p.protocol, Protocol::ChituUdp);
    assert_eq!(p.name, "Mock Chitu");
    assert_eq!(p.ip, from());
    assert_eq!(mock.state().commands, ["M99999"]);
}

#[tokio::test]
async fn malformed_reply_then_valid_reply() {
    let (addr, _, task) = responder(vec![
        b"garbage".to_vec(),
        b"{not json".to_vec(),
        br#"{"Data":{"Name":"Good","MainboardID":"m","MainboardIP":"127.0.0.1"}}"#.to_vec(),
    ])
    .await;
    let p = discover_addr(addr, Duration::from_secs(3))
        .await
        .unwrap()
        .expect("printer found");
    assert_eq!(p.name, "Good");
    task.abort();
}

#[tokio::test]
async fn silent_host_times_out_with_none() {
    let (addr, count, task) = responder(vec![b"nonsense".to_vec()]).await;
    let started = Instant::now();
    let p = within(5.0, "discover", discover_addr(addr, Duration::from_millis(1500)))
        .await
        .unwrap();
    assert!(p.is_none());
    assert!(started.elapsed() >= Duration::from_millis(1400));
    // the request is repeated every second while waiting
    assert!(*count.lock().unwrap() >= 2, "request was not repeated");
    task.abort();
}

#[tokio::test]
async fn nothing_listening_is_not_an_error() {
    let port = common::free_udp_port();
    let p = within(
        5.0,
        "discover",
        discover_addr(SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(600)),
    )
    .await
    .unwrap();
    assert!(p.is_none());
}

#[tokio::test]
async fn collecting_from_several_targets_dedupes_and_sorts() {
    // replies claim (unroutable documentation) addresses; nothing connects to them
    let (a, _, t1) = responder(vec![
        br#"{"Data":{"Name":"B","MainboardID":"b","MainboardIP":"192.0.2.20"}}"#.to_vec(),
    ])
    .await;
    let (b, _, t2) = responder(vec![
        br#"{"Data":{"Name":"A","MainboardID":"a","MainboardIP":"192.0.2.10"}}"#.to_vec(),
        br#"{"Data":{"Name":"A again","MainboardID":"a","MainboardIP":"192.0.2.10"}}"#.to_vec(),
    ])
    .await;
    let chitu = ChituMock::start().await;
    let found = within(
        5.0,
        "discover",
        discover_on(&[a, b, chitu.addr], Duration::from_millis(1200)),
    )
    .await
    .unwrap();
    let names: Vec<(String, String)> = found.iter().map(|p| (p.ip.to_string(), p.name.clone())).collect();
    assert_eq!(
        names,
        [
            ("127.0.0.1".to_string(), "Mock Chitu".to_string()),
            ("192.0.2.10".to_string(), "A".to_string()),
            ("192.0.2.20".to_string(), "B".to_string()),
        ]
    );
    t1.abort();
    t2.abort();
}
