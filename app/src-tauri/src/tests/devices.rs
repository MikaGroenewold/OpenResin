//! Device commands. Nothing here contacts a real printer: saved devices use
//! documentation addresses (192.0.2.0/24, never connected to), connection
//! attempts go to the loopback interface only, and the camera relay reads
//! from an MJPEG server the test runs on 127.0.0.1.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::state::SavedDevice;

fn device(ip: &str, name: &str) -> SavedDevice {
    SavedDevice {
        ip: ip.into(),
        name: name.into(),
        model: Some("ELEGOO Saturn 4 Ultra".into()),
    }
}

#[test]
fn saved_devices_persist() {
    let app = TestApp::new();
    let list: Vec<SavedDevice> = app.call("saved_devices", json!({}));
    assert!(list.is_empty());

    let list: Vec<SavedDevice> = app.call("save_device", json!({ "device": device("192.0.2.10", "Saturn") }));
    assert_eq!(list.len(), 1);
    let list: Vec<SavedDevice> = app.call("save_device", json!({ "device": device("192.0.2.11", "Mars") }));
    assert_eq!(list.len(), 2);
    // same address again (as typed, with stray blanks): replaced, not duplicated
    let list: Vec<SavedDevice> = app.call("save_device", json!({ "device": device(" 192.0.2.10 ", "Renamed") }));
    assert_eq!(list.len(), 2, "{list:?}");
    assert_eq!(list.iter().filter(|d| d.ip == "192.0.2.10").count(), 1, "{list:?}");
    assert_eq!(list.iter().find(|d| d.ip == "192.0.2.10").unwrap().name, "Renamed");

    // model is optional
    let list: Vec<SavedDevice> = app.call("save_device", json!({ "device": { "ip": "::1", "name": "IPv6" } }));
    assert!(list.iter().any(|d| d.ip == "::1" && d.model.is_none()));

    for bad in ["", "printer.local", "192.0.2.300", "1.2.3"] {
        let e = app.fail("save_device", json!({ "device": device(bad, "x") }));
        assert!(e.contains("not a valid IP address"), "{bad}: {e}");
    }

    let cfg: Value = app.call("get_config", json!({}));
    assert_eq!(cfg["devices"].as_array().unwrap().len(), 3);

    let again = app.reopen();
    let list: Vec<SavedDevice> = again.call("saved_devices", json!({}));
    let ips: Vec<&str> = list.iter().map(|d| d.ip.as_str()).collect();
    assert_eq!(ips, ["192.0.2.11", "192.0.2.10", "::1"]);
    assert_eq!(list[0].model.as_deref(), Some("ELEGOO Saturn 4 Ultra"));

    let list: Vec<SavedDevice> = again.call("forget_device", json!({ "ip": "192.0.2.11" }));
    assert_eq!(list.len(), 2);
    let list: Vec<SavedDevice> = again.call("forget_device", json!({ "ip": "192.0.2.99" }));
    assert_eq!(list.len(), 2, "forgetting an unknown device is harmless");
    let third = again.reopen();
    let list: Vec<SavedDevice> = third.call("saved_devices", json!({}));
    assert!(list.iter().all(|d| d.ip != "192.0.2.11"));
}

#[test]
fn connect_to_loopback_without_a_printer_fails_fast() {
    let app = TestApp::new();
    let started = Instant::now();
    let e = app.fail("connect_printer", json!({ "ip": "127.0.0.1" }));
    let took = started.elapsed();
    assert!(e.contains("127.0.0.1"), "{e}");
    assert!(e.contains("no printer answered"), "{e}");
    assert!(took < Duration::from_secs(15), "connecting took {took:?}");

    let started = Instant::now();
    let e = app.fail("connect_printer", json!({ "ip": "my printer" }));
    assert!(e.contains("not a valid IP address"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(1));
    // nothing is kept for the failed connections
    let e = app.fail("printer_files", json!({ "ip": "127.0.0.1" }));
    assert_eq!(e, "not connected to 127.0.0.1");
}

#[test]
fn printer_commands_need_a_connection() {
    let app = TestApp::new();
    let ip = "127.0.0.1";
    let file = app.dir.join("job.goo");
    std::fs::write(&file, b"x").unwrap();
    let not_connected = format!("not connected to {ip}");
    let started = Instant::now();
    for (cmd, args) in [
        ("printer_files", json!({ "ip": ip })),
        ("printer_files", json!({ "ip": ip, "dir": "/usb" })),
        ("printer_delete", json!({ "ip": ip, "files": ["/local/a.goo"] })),
        ("printer_history", json!({ "ip": ip })),
        ("printer_command", json!({ "ip": ip, "command": "pause" })),
        (
            "printer_command",
            json!({ "ip": ip, "command": "start", "file": "a.goo" }),
        ),
        ("printer_command", json!({ "ip": ip, "command": "launch" })),
        (
            "upload_to_printer",
            json!({ "ip": ip, "path": s(&file), "start": false }),
        ),
        ("upload_to_printer", json!({ "ip": ip, "start": true })),
        (
            "printer_download",
            json!({ "ip": ip, "path": "/local/a.goo", "dest": s(&app.dir.join("dl.goo")) }),
        ),
        ("printer_thumbnail", json!({ "ip": ip, "path": "/local/a.goo" })),
        ("printer_set_time_lapse", json!({ "ip": ip, "enable": true })),
        ("printer_rename", json!({ "ip": ip, "name": "New name" })),
    ] {
        assert_eq!(app.fail(cmd, args), not_connected, "{cmd}");
    }
    assert!(started.elapsed() < Duration::from_secs(2), "no network round trips");
    assert!(!app.dir.join("dl.goo").exists());

    let e = app.fail("printer_rename", json!({ "ip": ip, "name": "   " }));
    assert!(e.contains("must not be empty"), "{e}");
    // cancelling and disconnecting idle devices is harmless
    app.run("cancel_upload", json!({ "ip": ip }));
    app.run("cancel_download", json!({ "ip": ip, "path": "/local/a.goo" }));
    app.run("disconnect_printer", json!({ "ip": ip }));
    app.run("camera_stop", json!({ "ip": ip }));
    let e = app.fail(
        "camera_snapshot",
        json!({ "ip": ip, "path": s(&app.dir.join("snap.jpg")) }),
    );
    assert_eq!(e, "camera is not running");
    let e = app.fail("camera_start", json!({ "ip": ip }));
    assert_eq!(e, not_connected, "without a URL the printer is asked for one");
    // missing required arguments are reported by name
    let e = app.fail("upload_to_printer", json!({ "ip": ip }));
    assert!(e.contains("start"), "{e}");
}

/// Serves an endless MJPEG stream of `jpeg` frames on 127.0.0.1 until the
/// client disconnects. Returns the port.
fn mjpeg_server(jpeg: Vec<u8>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { return };
            let jpeg = jpeg.clone();
            std::thread::spawn(move || {
                s.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut req = [0u8; 2048];
                let _ = s.read(&mut req);
                if s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\n\r\n")
                    .is_err()
                {
                    return;
                }
                for _ in 0..600 {
                    let head = format!(
                        "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                        jpeg.len()
                    );
                    if s.write_all(head.as_bytes())
                        .and_then(|_| s.write_all(&jpeg))
                        .and_then(|_| s.write_all(b"\r\n"))
                        .is_err()
                    {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
        }
    });
    port
}

fn http_get(url: &str) -> (String, Vec<u8>) {
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    assert!(host.starts_with("127.0.0.1:"), "relay must listen on loopback: {url}");
    let mut s = TcpStream::connect(host).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    // one write: the relay parses the request from the first segment it reads
    let req = format!("GET /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").expect("http response");
    (
        String::from_utf8_lossy(&buf[..split]).into_owned(),
        buf[split + 4..].to_vec(),
    )
}

#[test]
fn camera_relay_from_a_stream_url() {
    let app = TestApp::new();
    let img = image::RgbImage::from_fn(80, 60, |x, y| image::Rgb([x as u8 * 3, y as u8 * 4, 90]));
    let mut jpeg = std::io::Cursor::new(Vec::new());
    img.write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
    let jpeg = jpeg.into_inner();
    let port = mjpeg_server(jpeg.clone());
    let source = format!("http://127.0.0.1:{port}/video");
    // the key is a loopback address; with an explicit URL no printer is contacted
    let ip = "127.0.0.1";

    let urls: Value = app.call("camera_start", json!({ "ip": ip, "url": source }));
    assert_eq!(urls["source"], source.as_str());
    let stream = urls["stream_url"].as_str().unwrap().to_string();
    let snapshot = urls["snapshot_url"].as_str().unwrap().to_string();
    assert!(stream.starts_with("http://127.0.0.1:"), "{stream}");

    // wait for the first frame, then save a snapshot
    let path = app.dir.join("snap.jpg");
    let started = Instant::now();
    loop {
        match app.try_invoke("camera_snapshot", json!({ "ip": ip, "path": s(&path) })) {
            Ok(_) => break,
            Err(e) => {
                assert_eq!(e, "no camera frame received yet");
                assert!(started.elapsed() < Duration::from_secs(15), "no frame from the relay");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    assert_eq!(std::fs::read(&path).unwrap(), jpeg, "snapshot is the streamed frame");

    // the relay serves the frame to the UI
    let (head, body) = http_get(&snapshot);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.to_ascii_lowercase().contains("image/jpeg"), "{head}");
    assert_eq!(body, jpeg);

    // starting again reuses the running relay
    let again: Value = app.call("camera_start", json!({ "ip": ip, "url": source }));
    assert_eq!(again["stream_url"], stream.as_str());
    assert_eq!(again["source"], "");

    app.run("camera_stop", json!({ "ip": ip }));
    let e = app.fail("camera_snapshot", json!({ "ip": ip, "path": s(&path) }));
    assert_eq!(e, "camera is not running");
    // the local relay is gone
    let port: u16 = stream
        .rsplit(':')
        .next()
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let started = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(started.elapsed() < Duration::from_secs(5), "relay still listening");
        std::thread::sleep(Duration::from_millis(50));
    }
}
