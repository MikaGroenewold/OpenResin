//! SDCP file transfer against a fake printer on 127.0.0.1: chunked HTTP
//! upload (V3), printer-pulls-file upload (older boards), download and range
//! reads of stored files.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use openresin_net::sdcp::{cmd, file_md5, SdcpClient, CHUNK_SIZE};
use openresin_net::{NetError, PrinterClient};
use serde_json::json;

use common::sdcp_mock::SdcpMock;
use common::{md5_hex, random_bytes, within, ProgressLog, TempDir};

const MIB: usize = 1024 * 1024;

async fn upload(c: &SdcpClient, data: &[u8], name: &str) -> (Result<(), NetError>, ProgressLog) {
    let dir = TempDir::new("sdcp-up");
    let path = dir.file("local-file.bin", data);
    let log = ProgressLog::default();
    let cb = log.callback();
    let r = within(30.0, "upload", c.upload(&path, name, &cb, &AtomicBool::new(false))).await;
    (r, log)
}

#[tokio::test]
async fn multi_chunk_upload_is_reassembled_exactly() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    let data = random_bytes(5 * MIB / 2, 11);
    let total = data.len() as u64;
    let (r, log) = upload(&c, &data, "big model.goo").await;
    r.unwrap();
    let st = mock.state();
    assert_eq!(st.chunks.len(), 3);
    let md5 = md5_hex(&data);
    let uuid = st.chunks[0].uuid.clone();
    assert_eq!(uuid.len(), 32);
    assert!(uuid.chars().all(|c| c.is_ascii_hexdigit()));
    for (i, ch) in st.chunks.iter().enumerate() {
        assert_eq!(
            ch.fields,
            ["S-File-MD5", "Check", "Offset", "Uuid", "TotalSize", "File"]
        );
        assert_eq!(ch.md5, md5, "S-File-MD5 is the MD5 of the whole file");
        assert_eq!(ch.check, "1");
        assert_eq!(ch.offset, (i * CHUNK_SIZE) as u64);
        assert_eq!(ch.uuid, uuid, "the same Uuid for all chunks");
        assert_eq!(ch.total, total);
        assert_eq!(ch.filename.as_deref(), Some("big model.goo"));
        assert_eq!(ch.content_type.as_deref(), Some("application/octet-stream"));
    }
    let lens: Vec<usize> = st.chunks.iter().map(|c| c.len).collect();
    assert_eq!(lens, [MIB, MIB, MIB / 2]);
    assert_eq!(st.completed.len(), 1);
    assert!(st.completed[0].md5_ok, "printer-side MD5 check failed");
    assert_eq!(st.files.get("big model.goo").map(Vec::as_slice), Some(&data[..]));
    assert_eq!(st.posts, 3);
    drop(st);
    log.assert_complete(total);
    assert_eq!(
        log.calls(),
        [(0, total), (MIB as u64, total), (2 * MIB as u64, total), (total, total)]
    );
    // the WebSocket is not involved in V3 uploads
    assert_eq!(mock.state().commands(), [cmd::ATTRIBUTES, cmd::STATUS]);
}

#[tokio::test]
async fn upload_chunk_boundaries() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    for (len, chunks) in [(100usize, 1usize), (MIB, 1), (MIB + 1, 2), (2 * MIB, 2)] {
        let data = random_bytes(len, len as u64);
        let before = mock.state().chunks.len();
        let name = format!("f{len}.goo");
        let (r, log) = upload(&c, &data, &name).await;
        r.unwrap();
        let st = mock.state();
        assert_eq!(st.chunks.len() - before, chunks, "{len} bytes");
        assert_eq!(st.files[&name], data, "{len} bytes");
        drop(st);
        log.assert_complete(len as u64);
    }
}

#[tokio::test]
async fn empty_file_upload_is_refused() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    let (r, _) = upload(&c, &[], "empty.goo").await;
    assert!(matches!(r, Err(NetError::Unsupported(_))), "{r:?}");
    assert_eq!(mock.state().posts, 0);
}

#[tokio::test]
async fn upload_can_be_cancelled() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    let dir = TempDir::new("sdcp-cancel");
    let data = random_bytes(3 * MIB, 12);
    let path = dir.file("x.goo", &data);
    let cancel = Arc::new(AtomicBool::new(false));
    let log = ProgressLog::default();
    let inner = log.callback();
    let c2 = cancel.clone();
    let cb = move |sent: u64, total: u64| {
        inner(sent, total);
        if sent > 0 {
            c2.store(true, Ordering::Relaxed);
        }
    };
    let r = within(10.0, "upload", c.upload(&path, "x.goo", &cb, &cancel)).await;
    assert!(matches!(r, Err(NetError::Cancelled)), "{r:?}");
    let st = mock.state();
    assert_eq!(st.chunks.len(), 1, "kept uploading after cancel");
    // the printer is told to drop the partial file
    let term = st.data_of(cmd::TERMINATE_TRANSFER);
    assert_eq!(term, [json!({"Uuid": st.chunks[0].uuid.clone(), "FileName": "x.goo"})]);
    assert!(st.partial.is_empty());
    assert!(!st.files.contains_key("x.goo"));
    drop(st);
    assert!(log.calls().last().unwrap().0 < data.len() as u64);
}

#[tokio::test]
async fn rejected_chunk_reports_the_reason() {
    let cases = [
        (
            json!({"success": false, "messages": [{"field": "common_field", "message": "-2"}]}),
            "offset does not match the partial file",
        ),
        (
            json!({"success": false, "messages": [{"field": "common_field", "message": -1}]}),
            "illegal offset",
        ),
        (
            json!({"success": false, "messages": [{"field": "common_field", "message": "-3"}]}),
            "printer cannot open the file",
        ),
        (
            json!({"success": false, "messages": [{"field": "common_field", "message": "-4"}]}),
            "unknown error",
        ),
        (
            json!({"success": false, "messages": [{"field": "Offset", "message": "bad"}, {"field": "common_field", "message": "-1"}]}),
            "Offset: bad, illegal offset",
        ),
        (json!({"success": false}), "upload failed: {\"success\":false}"),
    ];
    for (resp, want) in cases {
        let mock = SdcpMock::start_with(|s| s.reject_chunk = Some((1, 200, resp.to_string()))).await;
        let c = mock.client().await;
        let data = random_bytes(2 * MIB + 5, 13);
        match upload(&c, &data, "r.goo").await.0 {
            Err(NetError::Rejected(m)) => assert_eq!(m, want),
            other => panic!("{other:?}"),
        }
        let st = mock.state();
        assert_eq!(st.chunks.len(), 2, "upload continued after a rejected chunk");
        assert!(st.files.is_empty());
    }
}

#[tokio::test]
async fn http_error_page_is_a_rejection() {
    let mock = SdcpMock::start_with(|s| s.reject_chunk = Some((0, 500, "<html>oops</html>".into()))).await;
    let c = mock.client().await;
    match upload(&c, &random_bytes(1000, 14), "e.goo").await.0 {
        Err(NetError::Rejected(m)) => assert_eq!(m, "upload failed: null"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn dropped_connection_mid_upload_is_retried() {
    let mock = SdcpMock::start_with(|s| s.drop_posts = vec![1]).await;
    let c = mock.client().await;
    let data = random_bytes(5 * MIB / 2, 15);
    let t0 = Instant::now();
    let (r, log) = upload(&c, &data, "retry.goo").await;
    r.unwrap();
    assert!(t0.elapsed() >= Duration::from_millis(500), "retry did not back off");
    let st = mock.state();
    assert_eq!(st.posts, 4, "one chunk was sent twice");
    let offsets: Vec<u64> = st.chunks.iter().map(|c| c.offset).collect();
    assert_eq!(offsets, [0, MIB as u64, 2 * MIB as u64]);
    assert!(st.completed[0].md5_ok);
    assert_eq!(st.files["retry.goo"], data);
    drop(st);
    log.assert_complete(data.len() as u64);
}

#[tokio::test]
async fn persistent_connection_drops_fail_the_upload() {
    let mock = SdcpMock::start_with(|s| s.drop_posts = vec![1, 2, 3]).await;
    let c = mock.client().await;
    let data = random_bytes(2 * MIB, 16);
    let t0 = Instant::now();
    let (r, log) = upload(&c, &data, "fail.goo").await;
    assert!(matches!(r, Err(NetError::Http(_))), "{r:?}");
    assert!(t0.elapsed() < Duration::from_secs(10));
    let st = mock.state();
    assert_eq!(st.posts, 4, "three attempts for the failing chunk");
    assert!(st.files.is_empty() && st.completed.is_empty());
    drop(st);
    assert_eq!(log.calls().last().unwrap().0, MIB as u64);
}

#[tokio::test]
async fn printer_gone_during_upload_fails_without_hanging() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    drop(mock); // stops accepting connections
    let t0 = Instant::now();
    let (r, _) = upload(&c, &random_bytes(1000, 17), "gone.goo").await;
    assert!(r.is_err());
    assert!(t0.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn pull_upload_for_older_mainboards() {
    let mock = SdcpMock::start_with(|s| s.protocol_version = "V1.0.0".into()).await;
    let c = mock.client().await;
    assert_eq!(c.protocol_version, "V1.0.0");
    let data = random_bytes(300_000, 18);
    let (r, log) = upload(&c, &data, "my part.ctb").await;
    r.unwrap();
    let st = mock.state();
    assert_eq!(st.posts, 0, "older boards do not take HTTP uploads");
    let req = st.data_of(cmd::DOWNLOAD_FILE);
    assert_eq!(req.len(), 1);
    let url = req[0]["URL"].as_str().unwrap().to_string();
    assert_eq!(
        req[0],
        json!({
            "Check": 0, "CleanCache": 1, "Compress": 0, "FileSize": data.len(),
            "Filename": "my part.ctb", "MD5": md5_hex(&data), "URL": url.clone()
        })
    );
    // http://127.0.0.1:<port>/<random token>/<url-encoded name>
    let rest = url.strip_prefix("http://127.0.0.1:").expect("served on loopback");
    let (_, path) = rest.split_once('/').unwrap();
    let (token, name) = path.split_once('/').unwrap();
    assert_eq!(token.len(), 32);
    assert_eq!(name, "my%20part.ctb");
    let f = &st.pull_fetches[0];
    assert_eq!(f.status, 200);
    assert!(f.md5_ok);
    assert_eq!(f.data, data);
    assert_eq!(f.range_status, 206);
    assert_eq!(
        f.content_range.as_deref(),
        Some(format!("bytes 10-{}/{}", data.len() - 1, data.len()).as_str())
    );
    assert_eq!(f.range_data, data[10..]);
    assert_eq!(f.wrong_token_status, 404);
    assert_eq!(st.files["my part.ctb"], data);
    drop(st);
    log.assert_complete(data.len() as u64);
}

#[tokio::test]
async fn pull_upload_ignores_a_stale_transfer_result() {
    // the last status still shows the previous transfer as done
    let mock = SdcpMock::start_with(|s| {
        s.protocol_version = "V2.0.0".into();
        s.status["FileTransferInfo"] =
            json!({"Status": 2, "DownloadOffset": 5, "CheckOffset": 5, "FileTotalSize": 5, "Filename": "old.ctb"});
        s.pull_delay = Duration::from_millis(400);
    })
    .await;
    let c = mock.client().await;
    let data = random_bytes(10_000, 19);
    let (r, _) = upload(&c, &data, "new.ctb").await;
    r.unwrap();
    let st = mock.state();
    assert_eq!(
        st.pull_fetches.len(),
        1,
        "upload reported success before the printer fetched the file"
    );
    assert_eq!(st.files["new.ctb"], data);
}

#[tokio::test]
async fn pull_upload_failure_is_reported() {
    let mock = SdcpMock::start_with(|s| {
        s.protocol_version = "V1.0.0".into();
        s.pull_result = 3;
    })
    .await;
    let c = mock.client().await;
    match upload(&c, &random_bytes(5000, 20), "bad.ctb").await.0 {
        Err(NetError::Rejected(m)) => assert_eq!(m, "printer reported a failed transfer"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn pull_upload_can_be_cancelled() {
    let mock = SdcpMock::start_with(|s| {
        s.protocol_version = "V1.0.0".into();
        s.pull_delay = Duration::from_secs(20);
    })
    .await;
    let c = mock.client().await;
    let dir = TempDir::new("pull-cancel");
    let path = dir.file("p.ctb", &random_bytes(5000, 21));
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c2.store(true, Ordering::Relaxed);
    });
    let r = within(5.0, "upload", c.upload(&path, "p.ctb", &|_, _| {}, &cancel)).await;
    assert!(matches!(r, Err(NetError::Cancelled)), "{r:?}");
    assert_eq!(
        mock.state().data_of(cmd::TERMINATE_TRANSFER),
        [json!({"Uuid": "", "FileName": "p.ctb"})]
    );
}

#[tokio::test]
async fn download_whole_file() {
    let data = random_bytes(3 * MIB + 123, 22);
    let mock = {
        let d = data.clone();
        SdcpMock::start_with(move |s| {
            s.files.insert("model.goo".into(), d);
        })
        .await
    };
    let c = mock.client().await;
    let dir = TempDir::new("dl");
    let dest = dir.path().join("model.goo");
    let log = ProgressLog::default();
    let cb = log.callback();
    let n = within(
        30.0,
        "download",
        c.download("/local/model.goo", &dest, &cb, &AtomicBool::new(false)),
    )
    .await
    .unwrap();
    assert_eq!(n, data.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert_eq!(file_md5(&dest).await.unwrap(), md5_hex(&data));
    assert!(!dir.path().join("model.part").exists(), "temporary file left behind");
    log.assert_complete(data.len() as u64);
    assert!(log.calls().len() > 1, "progress is reported while downloading");
    let st = mock.state();
    let get = st.http_log.last().unwrap();
    assert_eq!(
        (get.method.as_str(), get.path.as_str(), get.range.as_deref(), get.status),
        ("GET", "/media/mmcblk0p3/model.goo", None, 200)
    );
}

#[tokio::test]
async fn download_can_be_cancelled() {
    let data = random_bytes(8 * MIB, 23);
    let mock = SdcpMock::start_with(move |s| {
        s.files.insert("big.goo".into(), data);
    })
    .await;
    let c = mock.client().await;
    let dir = TempDir::new("dl-cancel");
    let dest = dir.path().join("big.goo");
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let cb = move |done: u64, _| {
        if done > 0 {
            c2.store(true, Ordering::Relaxed);
        }
    };
    let r = within(10.0, "download", c.download("/local/big.goo", &dest, &cb, &cancel)).await;
    assert!(matches!(r, Err(NetError::Cancelled)), "{r:?}");
    assert!(!dest.exists());
    assert!(!dir.path().join("big.part").exists());
}

#[tokio::test]
async fn download_errors() {
    let mock = SdcpMock::start().await;
    let c = mock.client().await;
    let dir = TempDir::new("dl-err");
    let dest = dir.path().join("x.goo");
    let r = c
        .download("/local/missing.goo", &dest, &|_, _| {}, &AtomicBool::new(false))
        .await;
    match r {
        Err(NetError::Http(e)) => assert_eq!(e.status().map(|s| s.as_u16()), Some(404)),
        other => panic!("{other:?}"),
    }
    assert!(!dest.exists() && !dir.path().join("x.part").exists());
    let r = c
        .download("/usb/x.goo", &dest, &|_, _| {}, &AtomicBool::new(false))
        .await;
    assert!(matches!(r, Err(NetError::Unsupported(_))));
}

#[tokio::test]
async fn range_reads_return_exact_bytes() {
    let data = random_bytes(100_000, 24);
    let mock = {
        let d = data.clone();
        SdcpMock::start_with(move |s| {
            s.files.insert("thumb.goo".into(), d);
        })
        .await
    };
    let c = mock.client().await;
    let len = data.len() as u64;
    for (off, n) in [(0u64, 16u64), (1000, 4096), (len - 10, 10), (len - 1, 1), (12_345, 1)] {
        let got = c.read_range("/local/thumb.goo", off, n).await.unwrap();
        assert_eq!(got, data[off as usize..(off + n) as usize], "range {off}+{n}");
    }
    // past the end: what is there
    assert_eq!(
        c.read_range("/local/thumb.goo", len - 10, 100).await.unwrap(),
        data[(len - 10) as usize..]
    );
    // nothing asked, nothing sent
    let before = mock.state().http_log.len();
    assert!(c.read_range("/local/thumb.goo", 5, 0).await.unwrap().is_empty());
    assert_eq!(mock.state().http_log.len(), before);
    // beyond the end of the file
    match c.read_range("/local/thumb.goo", len, 10).await {
        Err(NetError::Http(e)) => assert_eq!(e.status().map(|s| s.as_u16()), Some(416)),
        other => panic!("{other:?}"),
    }
    {
        let st = mock.state();
        let ranges: Vec<Option<&str>> = st.http_log.iter().map(|h| h.range.as_deref()).collect();
        assert_eq!(ranges[1], Some("bytes=1000-5095"));
        assert!(st.http_log.iter().all(|h| h.path == "/media/mmcblk0p3/thumb.goo"));
    }
    assert!(matches!(
        c.read_range("/usb/thumb.goo", 0, 4).await,
        Err(NetError::Unsupported(_))
    ));
}

#[tokio::test]
async fn range_reads_work_when_the_server_ignores_ranges() {
    let data = random_bytes(50_000, 25);
    let mock = {
        let d = data.clone();
        SdcpMock::start_with(move |s| {
            s.files.insert("t.ctb".into(), d);
            s.ignore_range = true;
        })
        .await
    };
    let c = mock.client().await;
    assert_eq!(c.read_range("/local/t.ctb", 60, 4).await.unwrap(), data[60..64]);
    assert_eq!(c.read_range("t.ctb", 0, 112).await.unwrap(), data[..112]);
    assert_eq!(c.read_range("/local/t.ctb", 49_990, 50).await.unwrap(), data[49_990..]);
    assert!(mock.state().http_log.iter().all(|h| h.status == 200));
}

#[tokio::test]
async fn stored_file_urls_are_encoded() {
    let mock = SdcpMock::start_with(|s| {
        s.files.insert("my model ü #1.goo".into(), b"GOO!".to_vec());
        s.files.insert("sub/x.goo".into(), b"SUB!".to_vec());
    })
    .await;
    let c = mock.client().await;
    let base = format!("http://127.0.0.1:{}/media/mmcblk0p3/", mock.addr.port());
    assert_eq!(c.file_url("/local/a b.goo").unwrap(), format!("{base}a%20b.goo"));
    assert_eq!(c.file_url("/local/sub/x.goo").unwrap(), format!("{base}sub/x.goo"));
    assert_eq!(c.file_url("bare.goo").unwrap(), format!("{base}bare.goo"));
    assert_eq!(c.file_url("/usb/x.goo"), None);
    assert_eq!(c.file_url("/local2/x.goo"), None);
    assert_eq!(c.read_range("/local/my model ü #1.goo", 0, 4).await.unwrap(), b"GOO!");
    assert_eq!(c.read_range("/local/sub/x.goo", 0, 4).await.unwrap(), b"SUB!");
    assert_eq!(
        mock.state().http_log[0].path,
        "/media/mmcblk0p3/my%20model%20%C3%BC%20%231.goo"
    );
}

#[tokio::test]
async fn printer_client_file_transfers() {
    let stored = random_bytes(200_000, 26);
    let mock = {
        let d = stored.clone();
        SdcpMock::start_with(move |s| {
            s.files.insert("stored.goo".into(), d);
        })
        .await
    };
    let pc = within(
        8.0,
        "connect",
        PrinterClient::connect_with_ports(mock.addr.ip(), mock.udp_addr.port(), mock.addr.port()),
    )
    .await
    .unwrap();
    let dir = TempDir::new("pc-files");
    let data = random_bytes(MIB + 77, 27);
    let path = dir.file("in.goo", &data);
    let log = ProgressLog::default();
    let cb = log.callback();
    pc.upload(&path, &pc.remote_name("up loaded.goo"), &cb, &AtomicBool::new(false))
        .await
        .unwrap();
    log.assert_complete(data.len() as u64);
    assert_eq!(mock.state().files["up loaded.goo"], data);

    assert_eq!(
        pc.read_range("/local/stored.goo", 100, 50).await.unwrap(),
        stored[100..150]
    );
    let dest = dir.path().join("out.goo");
    let n = pc
        .download("/local/stored.goo", &dest, &|_, _| {}, &AtomicBool::new(false))
        .await
        .unwrap();
    assert_eq!(n, stored.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), stored);
}
