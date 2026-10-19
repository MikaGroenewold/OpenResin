//! Camera: RTSP/RTP/H.264 client, depacketizer and the local MJPEG relay,
//! against a fake printer camera streaming real H.264 on 127.0.0.1.

mod common;

use std::time::{Duration, Instant};

use openresin_net::camera::CameraRelay;
use openresin_net::rtsp::{Depacketizer, RtspSession};
use openresin_net::NetError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use common::h264::{self, annexb, assert_bars, decode, rtp_packet, split_nals, Packetization, HEIGHT, WIDTH};
use common::rtsp_mock::{RtspConfig, RtspMock};
use common::within;

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn decoder() -> openh264::decoder::Decoder {
    openh264::decoder::Decoder::new().unwrap()
}

/// Reads access units until `n` decodable frames came out; checks that every
/// access unit is bit-identical to one the encoder produced.
async fn receive_frames(s: &mut RtspSession, mock: &RtspMock, n: usize, expect_params_prepended: bool) {
    let encoded = &mock.aus;
    let params = annexb(&[mock.sps.clone(), mock.pps.clone()]);
    let mut dec = decoder();
    let mut decoded = 0;
    let mut aus = 0;
    while decoded < n {
        let au = within(10.0, "access unit", s.next_access_unit())
            .await
            .expect("access unit");
        aus += 1;
        assert!(aus < 200, "no decodable frames");
        let nals = split_nals(&au);
        let types: Vec<u8> = nals.iter().map(|n| h264::nal_type(n)).collect();
        if types.contains(&5) {
            assert!(
                types.starts_with(&[7, 8]),
                "IDR without parameter sets in front: {types:?}"
            );
            if expect_params_prepended {
                assert!(au.starts_with(&params), "SDP parameter sets not prepended");
            }
        }
        assert!(
            encoded.contains(&nals),
            "access unit {aus} ({types:?}, {} bytes) differs from what was sent",
            au.len()
        );
        if let Some((w, h, rgb)) = decode(&mut dec, &au) {
            assert_bars(w, h, &rgb, 40);
            decoded += 1;
        }
    }
}

fn header<'a>(r: &'a common::rtsp_mock::RtspRequest, name: &str) -> &'a str {
    r.header(name).unwrap_or_else(|| panic!("{} without {name}", r.method))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rtsp_handshake_matches_what_the_printer_needs() {
    let mock = RtspMock::start(RtspConfig::default()).await;
    let mut s = within(10.0, "open", RtspSession::open(&mock.url)).await.expect("open");
    {
        let log = mock.log();
        assert_eq!(log.methods(), ["DESCRIBE", "SETUP", "PLAY"]);
        let (d, setup, play) = (&log.requests[0], &log.requests[1], &log.requests[2]);
        assert_eq!(d.url, mock.url);
        assert_eq!(header(d, "Accept"), "application/sdp");
        for (i, r) in log.requests.iter().enumerate() {
            assert_eq!(header(r, "CSeq"), (i + 1).to_string());
            assert_eq!(header(r, "User-Agent"), "OpenResin");
        }
        // control URL resolved against Content-Base
        assert_eq!(setup.url, format!("{}/track1", mock.url));
        // exact spelling; even/odd pair below 32768 (quirks 3 and 4)
        let t = header(setup, "Transport");
        let (rtp, rtcp) = log.client_ports[0];
        assert_eq!(t, format!("RTP/AVP/UDP;unicast;client_port={rtp}-{rtcp}"));
        assert_eq!(rtp % 2, 0);
        assert_eq!(rtcp, rtp + 1);
        assert!((5000..32768).contains(&rtp), "client port {rtp}");
        // PLAY on the base URL with the bare session id (no ;timeout)
        assert_eq!(play.url, format!("{}/", mock.url));
        assert_eq!(header(play, "Session"), RtspMock::session_id(0));
        assert_eq!(header(play, "Range"), "npt=0.000-");
    }
    assert!(s.parameter_sets.is_empty(), "this SDP carries no sprop-parameter-sets");
    receive_frames(&mut s, &mock, 3, false).await;
    // SPS/PPS seen in-band are remembered
    assert_eq!(s.parameter_sets, annexb(&[mock.sps.clone(), mock.pps.clone()]));
    assert!(mock.log().punches >= 1, "no packet was sent to the server RTP port");
    within(5.0, "teardown", s.teardown()).await;
    wait_teardown(&mock).await;
    assert_eq!(mock.log().methods().last().unwrap(), "TEARDOWN");
    within(2.0, "stream stops", async {
        while mock.log().active_streams != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_packetization_is_depacketized_bit_exactly() {
    for mode in [
        Packetization::RawAnnexB,
        Packetization::SingleNalFuA,
        Packetization::StapA,
    ] {
        for marker in [true, false] {
            let cfg = RtspConfig {
                mode,
                marker,
                frame_interval: Duration::from_millis(30),
                ..Default::default()
            };
            let mock = RtspMock::start(cfg).await;
            let biggest = mock.aus.iter().flatten().map(Vec::len).max().unwrap();
            assert!(
                biggest > 3 * 1000,
                "largest NAL unit ({biggest} B) needs several fragments"
            );
            let mut s = within(10.0, "open", RtspSession::open(&mock.url)).await.expect("open");
            receive_frames(&mut s, &mock, 12, false).await;
            s.teardown().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdp_parameter_sets_are_prepended_to_idr_frames() {
    let cfg = RtspConfig {
        mode: Packetization::SingleNalFuA,
        sprop_in_sdp: true,
        inband_params: false,
        content_base: false,
        quirky_headers: false,
        ..Default::default()
    };
    let mock = RtspMock::start(cfg).await;
    let mut s = within(10.0, "open", RtspSession::open(&mock.url)).await.expect("open");
    assert_eq!(s.parameter_sets, annexb(&[mock.sps.clone(), mock.pps.clone()]));
    {
        let log = mock.log();
        // without Content-Base the request URL is the base
        assert_eq!(log.requests[1].url, format!("{}/track1", mock.url));
        assert_eq!(log.requests[2].url, mock.url);
    }
    receive_frames(&mut s, &mock, 12, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn falls_back_to_tcp_interleaving() {
    let cfg = RtspConfig {
        tcp_only: true,
        mode: Packetization::SingleNalFuA,
        ..Default::default()
    };
    let mock = RtspMock::start(cfg).await;
    let mut s = within(10.0, "open", RtspSession::open(&mock.url)).await.expect("open");
    {
        let log = mock.log();
        assert_eq!(log.methods(), ["DESCRIBE", "SETUP", "SETUP", "PLAY"]);
        assert_eq!(
            header(&log.requests[2], "Transport"),
            "RTP/AVP/TCP;unicast;interleaved=0-1"
        );
    }
    receive_frames(&mut s, &mock, 5, false).await;
    s.teardown().await;
    wait_teardown(&mock).await;
}

async fn wait_teardown(mock: &RtspMock) {
    within(3.0, "TEARDOWN reaches the camera", async {
        while mock.log().teardowns.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(mock.log().teardowns, [RtspMock::session_id(0)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_is_kept_alive() {
    // timeout=10 -> GET_PARAMETER every 5 s
    let cfg = RtspConfig {
        session_timeout: Some(10),
        ..Default::default()
    };
    let mock = RtspMock::start(cfg).await;
    let mut s = within(10.0, "open", RtspSession::open(&mock.url)).await.expect("open");
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(5600) {
        within(10.0, "access unit", s.next_access_unit())
            .await
            .expect("access unit");
    }
    let log = mock.log();
    let ka: Vec<_> = log.requests.iter().filter(|r| r.method == "GET_PARAMETER").collect();
    assert_eq!(ka.len(), 1, "{:?}", log.methods());
    assert_eq!(header(ka[0], "Session"), RtspMock::session_id(0));
    assert_eq!(ka[0].url, mock.url);
}

#[tokio::test]
async fn describe_and_sdp_errors() {
    let mock = RtspMock::start(RtspConfig {
        describe_status: 404,
        ..Default::default()
    })
    .await;
    match RtspSession::open(&mock.url).await {
        Err(NetError::Protocol(m)) => assert_eq!(m, "RTSP DESCRIBE returned 404"),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("opened"),
    }
    let audio_only = "v=0\r\nm=audio 0 RTP/AVP 0\r\na=control:a\r\n".to_string();
    let mock = RtspMock::start(RtspConfig {
        sdp_override: Some(audio_only),
        ..Default::default()
    })
    .await;
    assert!(matches!(RtspSession::open(&mock.url).await, Err(NetError::Unsupported(m)) if m.contains("no video")));
    let h265 = "v=0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H265/90000\r\na=control:v\r\n".to_string();
    let mock = RtspMock::start(RtspConfig {
        sdp_override: Some(h265),
        ..Default::default()
    })
    .await;
    assert!(matches!(RtspSession::open(&mock.url).await, Err(NetError::Unsupported(m)) if m.contains("not H.264")));
    assert!(matches!(
        RtspSession::open("http://127.0.0.1/x").await,
        Err(NetError::Protocol(_))
    ));
    let port = common::free_tcp_port();
    assert!(within(
        10.0,
        "refused",
        RtspSession::open(&format!("rtsp://127.0.0.1:{port}/video"))
    )
    .await
    .is_err());
}

// ------------------------------------------------------------------ relay

async fn get(url: &str) -> reqwest::Response {
    within(5.0, "GET", http().get(url).send()).await.expect("GET")
}

fn jpeg_rgb(jpeg: &[u8]) -> image::RgbImage {
    image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
        .expect("valid JPEG")
        .to_rgb8()
}

/// Reads parts from a multipart/x-mixed-replace stream.
async fn mjpeg_parts(resp: &mut reqwest::Response, n: usize) -> Vec<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut parts = Vec::new();
    while parts.len() < n {
        let chunk = within(10.0, "stream data", resp.chunk())
            .await
            .unwrap()
            .expect("stream continues");
        buf.extend_from_slice(&chunk);
        loop {
            let Some(head_end) = common::http::find(&buf, b"\r\n\r\n") else {
                break;
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            assert!(head.starts_with("--frame\r\n"), "bad part header {head:?}");
            assert!(head.contains("Content-Type: image/jpeg"));
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .expect("part length")
                .parse()
                .unwrap();
            if buf.len() < head_end + 4 + len + 2 {
                break;
            }
            parts.push(buf[head_end + 4..head_end + 4 + len].to_vec());
            assert_eq!(&buf[head_end + 4 + len..head_end + 6 + len], b"\r\n");
            buf.drain(..head_end + 6 + len);
        }
    }
    parts
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_turns_the_printer_stream_into_jpeg_frames() {
    // default config = the Mars 5 Ultra quirks: raw Annex B, in-band SPS/PPS,
    // malformed header lines
    let mock = RtspMock::start(RtspConfig::default()).await;
    let relay = within(5.0, "start", CameraRelay::start(&mock.url))
        .await
        .expect("relay");
    let f = relay
        .wait_frame(Duration::from_secs(10))
        .await
        .expect("a frame within 10 s");
    assert_eq!((f.width, f.height), (WIDTH as u32, HEIGHT as u32));
    assert!(f.seq >= 1);
    let img = jpeg_rgb(&f.jpeg);
    assert_eq!(img.dimensions(), (WIDTH as u32, HEIGHT as u32));
    assert_bars(WIDTH, HEIGHT, img.as_raw(), 45);
    assert_eq!(relay.status(), "streaming");

    // local URLs: loopback, random token
    let prefix = relay.stream_url.strip_suffix("/stream.mjpeg").unwrap();
    assert_eq!(relay.snapshot_url, format!("{prefix}/snapshot.jpg"));
    let rest = prefix.strip_prefix("http://127.0.0.1:").expect("relay on loopback");
    let (_, token) = rest.split_once('/').unwrap();
    assert_eq!(token.len(), 32);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));

    let snap = get(&relay.snapshot_url).await;
    assert_eq!(snap.status(), 200);
    assert_eq!(snap.headers()["content-type"], "image/jpeg");
    let img = jpeg_rgb(&snap.bytes().await.unwrap());
    assert_eq!(img.dimensions(), (WIDTH as u32, HEIGHT as u32));

    let mut stream = get(&relay.stream_url).await;
    assert_eq!(stream.status(), 200);
    assert_eq!(
        stream.headers()["content-type"],
        "multipart/x-mixed-replace; boundary=frame"
    );
    let parts = mjpeg_parts(&mut stream, 3).await;
    for p in &parts {
        let img = jpeg_rgb(p);
        assert_eq!(img.dimensions(), (WIDTH as u32, HEIGHT as u32));
        assert_bars(WIDTH, HEIGHT, img.as_raw(), 45);
    }
    assert_ne!(parts[0], parts[2], "the stream repeats one frame");
    drop(stream);

    // anything without the exact token is refused
    let port_base = rest.split_once('/').unwrap().0;
    for bad in [
        format!("http://127.0.0.1:{port_base}/stream.mjpeg"),
        format!("http://127.0.0.1:{port_base}/snapshot.jpg"),
        format!("http://127.0.0.1:{port_base}/{}/stream.mjpeg", "0".repeat(32)),
        format!("http://127.0.0.1:{port_base}/{token}x/snapshot.jpg"),
        format!("http://127.0.0.1:{port_base}/{token}/"),
        format!("http://127.0.0.1:{port_base}/"),
    ] {
        assert_eq!(get(&bad).await.status(), 404, "{bad}");
    }

    let latest = relay.latest().unwrap();
    assert!(latest.seq >= f.seq);
    within(5.0, "shutdown", relay.shutdown()).await;
    // the viewer slot is released with TEARDOWN (quirk 7)
    wait_teardown(&mock).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_with_standard_rtp_and_sdp_parameter_sets() {
    let cfg = RtspConfig {
        mode: Packetization::StapA,
        sprop_in_sdp: true,
        inband_params: false,
        quirky_headers: false,
        ..Default::default()
    };
    let mock = RtspMock::start(cfg).await;
    let relay = CameraRelay::start(&mock.url).await.unwrap();
    let f = relay.wait_frame(Duration::from_secs(10)).await.expect("frame");
    assert_eq!((f.width, f.height), (WIDTH as u32, HEIGHT as u32));
    assert_bars(WIDTH, HEIGHT, jpeg_rgb(&f.jpeg).as_raw(), 45);
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_reconnects_when_the_camera_drops() {
    let cfg = RtspConfig {
        close_after_frames: Some(6),
        ..Default::default()
    };
    let mock = RtspMock::start(cfg).await;
    let relay = CameraRelay::start(&mock.url).await.unwrap();
    let first = relay.wait_frame(Duration::from_secs(10)).await.expect("frame");
    // the camera hangs up: the relay reports it ...
    within(10.0, "error status", async {
        while !relay.status().starts_with("camera error") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let seq_at_drop = relay.latest().unwrap().seq;
    // ... and comes back on its own; frame numbers keep counting so viewers
    // (the MJPEG server compares them) never mistake a new frame for an old one
    within(10.0, "streaming again", async {
        let mut prev = seq_at_drop;
        while prev <= seq_at_drop + 3 {
            let seq = relay.latest().unwrap().seq;
            assert!(seq >= prev, "frame number went back from {prev} to {seq}");
            prev = seq;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(relay.status(), "streaming");
    assert!(mock.log().connections >= 2);
    assert!(relay.latest().unwrap().seq > first.seq);
    relay.shutdown().await;
}

/// A fake HTTP/MJPEG camera sending 64x48 frames (left half red, right half
/// blue) every 50 ms after `first_frame_delay`.
async fn fake_mjpeg_camera(first_frame_delay: Duration) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let cam = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cam_addr = cam.local_addr().unwrap();
    let img = image::RgbImage::from_fn(64, 48, |x, _| {
        if x < 32 {
            image::Rgb([230, 20, 20])
        } else {
            image::Rgb([20, 20, 230])
        }
    });
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(&img)
        .unwrap();
    let cam_task = tokio::spawn(async move {
        let (mut s, _) = cam.accept().await.unwrap();
        let mut b = [0u8; 1024];
        let _ = s.read(&mut b).await;
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=x\r\n\r\n")
            .await
            .unwrap();
        tokio::time::sleep(first_frame_delay).await;
        for _ in 0..400 {
            let part = b"--x\r\nContent-Type: image/jpeg\r\n\r\n";
            if s.write_all(part).await.is_err()
                || s.write_all(&jpeg).await.is_err()
                || s.write_all(b"\r\n").await.is_err()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    (cam_addr, cam_task)
}

#[tokio::test]
async fn mjpeg_source_is_relayed() {
    // the first frame takes a while
    let (cam_addr, cam_task) = fake_mjpeg_camera(Duration::from_millis(800)).await;
    let relay = CameraRelay::start(&format!("http://{cam_addr}/video")).await.unwrap();
    // nothing to show yet
    assert_eq!(get(&relay.snapshot_url).await.status(), 503);
    let f = relay.wait_frame(Duration::from_secs(5)).await.expect("frame");
    assert_eq!((f.width, f.height), (64, 48));
    let mut stream = get(&relay.stream_url).await;
    let parts = mjpeg_parts(&mut stream, 2).await;
    let got = jpeg_rgb(&parts[1]);
    assert_eq!(got.dimensions(), (64, 48));
    let left = got.get_pixel(10, 20).0;
    let right = got.get_pixel(54, 20).0;
    assert!(left[0] > 180 && left[2] < 80, "{left:?}");
    assert!(right[2] > 180 && right[0] < 80, "{right:?}");
    // frames are rate limited (15 fps)
    let a = relay.latest().unwrap().seq;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let b = relay.latest().unwrap().seq;
    assert!(b > a && b - a <= 10, "{a} -> {b}");
    relay.shutdown().await;
    cam_task.abort();
}

/// (host:port, path) of a relay URL.
fn split_url(url: &str) -> (String, String) {
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    assert!(host.starts_with("127.0.0.1:"), "relay must listen on loopback: {url}");
    (host.to_string(), format!("/{path}"))
}

/// Sends an HTTP request in several small TCP writes with pauses in between,
/// like a slow client or a proxy forwarding byte by byte.
async fn send_in_pieces(host: &str, pieces: &[String]) -> TcpStream {
    let mut s = TcpStream::connect(host).await.unwrap();
    s.set_nodelay(true).unwrap();
    for (i, p) in pieces.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        s.write_all(p.as_bytes()).await.unwrap();
        s.flush().await.unwrap();
    }
    s
}

/// `GET <path> HTTP/1.1` plus headers in 9 writes, cut through the method,
/// the token and the blank line that ends the head.
fn split_request(path: &str) -> Vec<String> {
    let mid = path.len() / 2;
    vec![
        "GE".into(),
        "T ".into(),
        path[..mid].into(),
        path[mid..].into(),
        " HTTP/1.1\r\nHost: 127.0.0.1\r\n".into(),
        "Accept: */*\r\nUser-Agent: slow".into(),
        "-client\r\n".into(),
        "\r".into(),
        "\n".into(),
    ]
}

/// Reads until `done(bytes so far)` or the peer closes; returns the bytes.
async fn read_until(s: &mut TcpStream, done: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let mut buf = Vec::new();
    within(10.0, "relay response", async {
        let mut tmp = [0u8; 16 * 1024];
        while !done(&buf) {
            match s.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
    })
    .await;
    buf
}

/// The first complete JPEG part after the response head, if there is one.
fn first_part(resp: &[u8]) -> Option<Vec<u8>> {
    let head_end = common::http::find(resp, b"\r\n\r\n")? + 4;
    let body = &resp[head_end..];
    let part_head_end = common::http::find(body, b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&body[..part_head_end]).to_string();
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))?
        .parse()
        .ok()?;
    body.get(part_head_end + 4..part_head_end + 4 + len).map(<[u8]>::to_vec)
}

#[tokio::test]
async fn relay_handles_requests_split_over_several_writes() {
    let (cam_addr, cam_task) = fake_mjpeg_camera(Duration::ZERO).await;
    let relay = CameraRelay::start(&format!("http://{cam_addr}/video")).await.unwrap();
    relay.wait_frame(Duration::from_secs(5)).await.expect("frame");

    let (host, path) = split_url(&relay.stream_url);
    let mut s = send_in_pieces(&host, &split_request(&path)).await;
    let resp = read_until(&mut s, |b| first_part(b).is_some()).await;
    let text = String::from_utf8_lossy(&resp[..resp.len().min(300)]).to_string();
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text:?}");
    assert!(
        text.contains("Content-Type: multipart/x-mixed-replace; boundary=frame\r\n"),
        "{text:?}"
    );
    let jpeg = first_part(&resp).expect("a frame");
    assert_eq!(jpeg_rgb(&jpeg).dimensions(), (64, 48));
    drop(s);

    // the snapshot endpoint too
    let (_, snap_path) = split_url(&relay.snapshot_url);
    let mut s = send_in_pieces(&host, &split_request(&snap_path)).await;
    let resp = read_until(&mut s, |_| false).await; // Connection: close
    let head_end = common::http::find(&resp, b"\r\n\r\n").expect("response head") + 4;
    let head = String::from_utf8_lossy(&resp[..head_end]).to_string();
    assert!(
        head.starts_with("HTTP/1.1 200 OK\r\n") && head.contains("Content-Type: image/jpeg"),
        "{head:?}"
    );
    assert_eq!(jpeg_rgb(&resp[head_end..]).dimensions(), (64, 48));

    relay.shutdown().await;
    cam_task.abort();
}

#[tokio::test]
async fn split_requests_with_a_wrong_token_are_still_refused() {
    let (cam_addr, cam_task) = fake_mjpeg_camera(Duration::ZERO).await;
    let relay = CameraRelay::start(&format!("http://{cam_addr}/video")).await.unwrap();
    relay.wait_frame(Duration::from_secs(5)).await.expect("frame");
    let (host, path) = split_url(&relay.stream_url);
    let token = path.split('/').nth(1).unwrap().to_string();
    let bad_paths = [
        format!("/{}/stream.mjpeg", "0".repeat(32)),
        format!("/{token}x/stream.mjpeg"),
        format!("/{}/stream.mjpeg", &token[..31]),
        format!("/{token}/stream.mjpegx"),
        format!("/{}/snapshot.jpg", "f".repeat(32)),
        "/stream.mjpeg".to_string(),
        format!("/x/{token}/stream.mjpeg"),
    ];
    for bad in bad_paths {
        let mut s = send_in_pieces(&host, &split_request(&bad)).await;
        let resp = read_until(&mut s, |_| false).await;
        let text = String::from_utf8_lossy(&resp).to_string();
        assert!(text.starts_with("HTTP/1.1 404 Not Found\r\n"), "{bad}: {text:?}");
        assert!(!text.contains("image/jpeg"), "{bad}: {text:?}");
    }
    relay.shutdown().await;
    cam_task.abort();
}

#[tokio::test]
async fn oversized_or_unfinished_requests_get_no_stream() {
    let (cam_addr, cam_task) = fake_mjpeg_camera(Duration::ZERO).await;
    let relay = CameraRelay::start(&format!("http://{cam_addr}/video")).await.unwrap();
    relay.wait_frame(Duration::from_secs(5)).await.expect("frame");
    let (host, path) = split_url(&relay.stream_url);

    // a valid request line followed by too many header bytes
    let mut pieces = vec![format!("GET {path} HTTP/1.1\r\n")];
    pieces.extend((0..10).map(|i| format!("X-Filler-{i}: {}\r\n", "a".repeat(1000))));
    let mut s = TcpStream::connect(&host).await.unwrap();
    for p in &pieces {
        // the relay may close as soon as the limit is hit
        if s.write_all(p.as_bytes()).await.is_err() {
            break;
        }
    }
    let resp = read_until(&mut s, |_| false).await;
    let text = String::from_utf8_lossy(&resp).to_string();
    assert!(text.starts_with("HTTP/1.1 431 "), "{text:?}");
    assert!(!text.contains("multipart"), "{text:?}");

    // the head never ends: closed after a timeout without an answer
    let t0 = Instant::now();
    let mut s = send_in_pieces(
        &host,
        &[format!("GET {path} HTTP/1.1\r\n"), "Host: 127.0.0.1\r\n".into()],
    )
    .await;
    let resp = read_until(&mut s, |_| false).await;
    assert!(
        resp.is_empty(),
        "answered an unfinished request: {:?}",
        String::from_utf8_lossy(&resp)
    );
    assert!(t0.elapsed() < Duration::from_secs(8));
    relay.shutdown().await;
    cam_task.abort();
}

// ------------------------------------------------------------ depacketizer

fn pkt(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
    rtp_packet(seq, ts, 7, marker, payload)
}

#[test]
fn rtp_header_extensions_csrcs_and_padding() {
    let mut d = Depacketizer::default();
    // V=2, P=1, X=1, CC=2
    let mut p = vec![0x80 | 0x20 | 0x10 | 2, 0x80 | 96, 0, 1, 0, 0, 0, 9, 0, 0, 0, 7];
    p.extend_from_slice(&[1, 1, 1, 1, 2, 2, 2, 2]); // CSRCs
    p.extend_from_slice(&[0xBE, 0xDE, 0, 1, 9, 9, 9, 9]); // one-word extension
    p.extend_from_slice(&[0x41, 0xAA, 0xBB]); // NAL
    p.extend_from_slice(&[0, 0, 3]); // padding
    let au = d.push(&p).expect("access unit");
    assert_eq!(au.data, [0, 0, 0, 1, 0x41, 0xAA, 0xBB]);
    assert!(!au.idr && !au.has_sps);
}

#[test]
fn invalid_packets_are_ignored() {
    let mut d = Depacketizer::default();
    assert!(d.push(&[0x80, 0x80, 0, 1]).is_none(), "short packet");
    let mut v1 = pkt(1, 1, true, &[0x41, 1]);
    v1[0] = 0x40; // version 1
    assert!(d.push(&v1).is_none());
    assert!(d.push(&pkt(2, 1, true, &[])).is_none(), "empty payload");
    // extension header longer than the packet
    let mut x = pkt(3, 1, true, &[0xBE]);
    x[0] |= 0x10;
    assert!(d.push(&x).is_none());
    // still works afterwards
    assert_eq!(
        d.push(&pkt(4, 2, true, &[0x41, 5])).unwrap().data,
        [0, 0, 0, 1, 0x41, 5]
    );
}

#[test]
fn sequence_wrap_is_not_loss() {
    let mut d = Depacketizer::default();
    assert!(d.push(&pkt(65535, 10, false, &[0x7C, 0x85, 1])).is_none());
    let au = d
        .push(&pkt(0, 10, true, &[0x7C, 0x45, 2]))
        .expect("frame across the wrap");
    assert_eq!(au.data, [0, 0, 0, 1, 0x65, 1, 2]);
    assert!(au.idr);
}

#[test]
fn loss_drops_only_the_damaged_frame() {
    let mut d = Depacketizer::default();
    assert!(d.push(&pkt(1, 100, false, &[0x7C, 0x85, 1])).is_none());
    // seq 2 lost
    assert!(
        d.push(&pkt(3, 100, true, &[0x7C, 0x45, 3])).is_none(),
        "damaged frame delivered"
    );
    let au = d.push(&pkt(4, 200, true, &[0x41, 4])).expect("next frame");
    assert_eq!(au.data, [0, 0, 0, 1, 0x41, 4]);
}

#[test]
fn losing_the_last_packet_of_a_frame_does_not_deliver_it_truncated() {
    let mut d = Depacketizer::default();
    assert!(d.push(&pkt(1, 100, false, &[0x7C, 0x85, 1])).is_none());
    assert!(d.push(&pkt(2, 100, false, &[0x7C, 0x05, 2])).is_none());
    // seq 3 (FU-A end with the marker bit) is lost; the next frame starts
    let r = d.push(&pkt(4, 200, true, &[0x41, 4]));
    assert!(r.is_none(), "truncated frame delivered: {:?}", r.map(|a| a.data));
    let au = d.push(&pkt(5, 300, true, &[0x41, 5])).expect("the stream recovers");
    assert_eq!(au.data, [0, 0, 0, 1, 0x41, 5]);
    // without a gap, a frame without marker still ends at the next timestamp
    assert!(d.push(&pkt(6, 400, false, &[0x41, 6])).is_none());
    assert_eq!(
        d.push(&pkt(7, 500, true, &[0x41, 7])).unwrap().data,
        [0, 0, 0, 1, 0x41, 6]
    );
}

#[test]
fn fu_a_without_start_is_dropped() {
    let mut d = Depacketizer::default();
    assert!(d.push(&pkt(1, 5, false, &[0x7C, 0x05, 1])).is_none());
    assert!(d.push(&pkt(2, 5, true, &[0x7C, 0x45, 2])).is_none());
    assert!(d.push(&pkt(3, 6, true, &[0x41, 1])).is_some());
}

#[test]
fn frames_end_on_timestamp_change_without_marker() {
    let mut d = Depacketizer::default();
    assert!(d.push(&pkt(1, 100, false, &[0x67, 1])).is_none());
    assert!(d.push(&pkt(2, 100, false, &[0x68, 2])).is_none());
    assert!(d.push(&pkt(3, 100, false, &[0x65, 3])).is_none());
    let au = d
        .push(&pkt(4, 200, false, &[0x41, 4]))
        .expect("previous frame completes");
    assert_eq!(au.data, [0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3]);
    assert!(au.idr && au.has_sps);
    let au = d.push(&pkt(5, 300, true, &[0x41, 5])).expect("frame 200 completes");
    assert_eq!(au.data, [0, 0, 0, 1, 0x41, 4]);
}

#[test]
fn truncated_stap_a_keeps_complete_units() {
    let mut d = Depacketizer::default();
    // second unit claims 9 bytes but only 1 is there
    let au = d
        .push(&pkt(1, 1, true, &[24, 0, 2, 0x67, 7, 0, 9, 0x68]))
        .expect("frame");
    assert_eq!(au.data, [0, 0, 0, 1, 0x67, 7]);
    assert!(au.has_sps && !au.idr);
}

#[test]
fn raw_annexb_quirk_flags_are_detected() {
    let mut d = Depacketizer::default();
    assert!(d.push(&pkt(1, 9, false, &[0, 0, 0, 1, 0x67, 0x42])).is_none());
    assert!(d.push(&pkt(2, 9, false, &[0, 0, 1, 0x68, 0xCE])).is_none());
    assert!(d
        .push(&pkt(3, 9, false, &[0x1C, 0x80, 0, 0, 0, 1, 0x65, 1, 2]))
        .is_none());
    assert!(d.push(&pkt(4, 9, false, &[0x1C, 0x00, 3, 4])).is_none());
    let au = d.push(&pkt(5, 9, true, &[0x1C, 0x40, 5])).expect("frame");
    assert!(au.idr && au.has_sps);
    assert_eq!(
        au.data,
        [0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x65, 1, 2, 3, 4, 5]
    );
    // a P frame afterwards is not an IDR
    let au = d.push(&pkt(6, 10, true, &[0, 0, 0, 1, 0x41, 9])).expect("frame");
    assert!(!au.idr && !au.has_sps);
}
