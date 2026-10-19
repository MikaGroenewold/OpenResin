//! Status and protocol parsing: every field and state the app relies on.

use openresin_net::chitu::{data_packet, parse_m27, parse_m4000, sanitize_name, CHUNK_SIZE};
use openresin_net::sdcp::parse_status;
use openresin_net::{FileEntry, MachineState, PrinterStatus, Protocol};
use serde_json::{json, Value};

fn status(current: Value, sub: Option<i64>) -> Value {
    let mut pi = json!({"CurrentLayer": 0, "TotalLayer": 0, "ErrorNumber": 0, "Filename": ""});
    if let Some(s) = sub {
        pi["Status"] = json!(s);
    }
    json!({"Status": {"CurrentStatus": current, "PrintInfo": pi}, "Topic": "sdcp/status/x"})
}

#[test]
fn sdcp_state_mapping_covers_every_status() {
    use MachineState::*;
    let cases: &[(Value, Option<i64>, MachineState, Option<&str>)] = &[
        (json!([0]), Some(0), Idle, Some("idle")),
        (json!([0]), None, Idle, None),
        (json!([0]), Some(8), Stopped, Some("stopped")),
        (json!([0]), Some(9), Completed, Some("complete")),
        (json!([1]), Some(0), Printing, Some("idle")),
        (json!([1]), Some(1), Homing, Some("homing")),
        (json!([1]), Some(2), Printing, Some("descending")),
        (json!([1]), Some(3), Printing, Some("exposing")),
        (json!([1]), Some(4), Printing, Some("lifting")),
        (json!([1]), Some(5), Pausing, Some("pausing")),
        (json!([1]), Some(6), Paused, Some("paused")),
        (json!([1]), Some(7), Stopping, Some("stopping")),
        (json!([1]), Some(8), Stopped, Some("stopped")),
        (json!([1]), Some(9), Completed, Some("complete")),
        (json!([1]), Some(10), FileChecking, Some("checking file")),
        (json!([1]), Some(99), Printing, Some("unknown")),
        (json!([2]), None, FileTransfer, None),
        (json!([3]), None, ExposureTest, None),
        (json!([4]), None, SelfTest, None),
        (json!([7]), None, Unknown, None),
        (json!([]), None, Unknown, None),
        // pre-V3 firmwares send a plain number
        (json!(1), Some(6), Paused, Some("paused")),
        (json!(2), None, FileTransfer, None),
    ];
    for (cur, sub, want, detail) in cases {
        let s = parse_status(&status(cur.clone(), *sub));
        assert_eq!(s.state, *want, "CurrentStatus {cur} / PrintInfo.Status {sub:?}");
        assert_eq!(s.detail.as_deref(), *detail, "detail for {cur} / {sub:?}");
    }
    // no CurrentStatus at all
    assert_eq!(parse_status(&json!({"Status": {}})).state, Unknown);
    assert_eq!(parse_status(&json!({})).state, Unknown);
}

#[test]
fn sdcp_print_fields_and_units() {
    let v = json!({
        "Status": {
            "CurrentStatus": [1],
            "TempOfUVLED": 41.25,
            "TempOfBox": 27,
            "TimeLapseStatus": 1,
            "PrintInfo": {
                "Status": 3, "CurrentLayer": 120, "TotalLayer": 480,
                "CurrentTicks": 1_234_567, "TotalTicks": 7_200_999,
                "Filename": "benchy.goo", "ErrorNumber": 0, "TaskId": "abc"
            }
        },
        "MainboardID": "x", "TimeStamp": 1, "Topic": "sdcp/status/x"
    });
    let s = parse_status(&v);
    assert_eq!(s.state, MachineState::Printing);
    assert_eq!(s.detail.as_deref(), Some("exposing"));
    assert_eq!(s.filename.as_deref(), Some("benchy.goo"));
    assert_eq!(s.current_layer, Some(120));
    assert_eq!(s.total_layers, Some(480));
    assert_eq!(s.progress(), Some(0.25));
    // ticks are milliseconds; whole seconds are reported
    assert_eq!(s.elapsed_s, Some(1234));
    assert_eq!(s.total_s, Some(7200));
    assert_eq!(s.uv_led_temp, Some(41.25));
    assert_eq!(s.box_temp, Some(27.0));
    assert_eq!(s.time_lapse, Some(true));
    assert_eq!(s.error_code, None);
    // filled in from attributes, never from status
    assert_eq!(s.free_storage, None);
    assert_eq!(s.camera_connected, None);
}

#[test]
fn sdcp_missing_and_empty_fields() {
    let s =
        parse_status(&json!({"Status": {"CurrentStatus": [0], "TimeLapseStatus": 0, "PrintInfo": {"Filename": ""}}}));
    assert_eq!(s.state, MachineState::Idle);
    assert_eq!(s.filename, None, "empty file name means no file");
    assert_eq!(s.time_lapse, Some(false));
    assert_eq!(s.current_layer, None);
    assert_eq!(s.progress(), None);
    assert_eq!(s.uv_led_temp, None);
    let s = parse_status(&json!({"Status": {"CurrentStatus": [0]}}));
    assert_eq!(s.time_lapse, None);
    assert_eq!(s.detail, None);
}

#[test]
fn sdcp_error_number() {
    let mut v = status(json!([0]), Some(0));
    v["Status"]["PrintInfo"]["ErrorNumber"] = json!(3);
    let s = parse_status(&v);
    assert_eq!(s.state, MachineState::Error, "idle with an error number is an error");
    assert_eq!(s.error_code, Some(3));
    // while printing the state is kept and the code reported
    let mut v = status(json!([1]), Some(3));
    v["Status"]["PrintInfo"]["ErrorNumber"] = json!(2);
    let s = parse_status(&v);
    assert_eq!(s.state, MachineState::Printing);
    assert_eq!(s.error_code, Some(2));
}

#[test]
fn sdcp_older_firmware_nests_status_in_data() {
    // pre-V3 boards: {"Data": {"Status": {...}}}
    let v = json!({
        "Id": "x",
        "Data": {
            "Status": {"CurrentStatus": 1, "PrintInfo": {"Status": 6, "CurrentLayer": 5, "TotalLayer": 10, "Filename": "a.ctb"}},
            "MainboardID": "m", "TimeStamp": 1
        },
        "Topic": "sdcp/status/m"
    });
    let s = parse_status(&v);
    assert_eq!(s.state, MachineState::Paused);
    assert_eq!(s.filename.as_deref(), Some("a.ctb"));
    assert_eq!(s.progress(), Some(0.5));
}

#[test]
fn progress_is_clamped_and_needs_a_total() {
    let p = |c, t| {
        PrinterStatus {
            current_layer: c,
            total_layers: t,
            ..Default::default()
        }
        .progress()
    };
    assert_eq!(p(Some(0), Some(100)), Some(0.0));
    assert_eq!(p(Some(100), Some(100)), Some(1.0));
    assert_eq!(p(Some(150), Some(100)), Some(1.0));
    assert_eq!(p(Some(5), Some(0)), None);
    assert_eq!(p(None, Some(10)), None);
    assert_eq!(p(Some(3), None), None);
}

#[test]
fn busy_states() {
    use MachineState::*;
    let busy = [
        Printing,
        Pausing,
        Paused,
        Stopping,
        FileTransfer,
        Homing,
        FileChecking,
        ExposureTest,
        SelfTest,
    ];
    let free = [Unknown, Offline, Idle, Stopped, Completed, Error];
    for s in busy {
        assert!(s.is_busy(), "{s:?} must refuse new prints");
    }
    for s in free {
        assert!(!s.is_busy(), "{s:?} must accept new prints");
    }
    // the app's own BUSY list (DevicesView.tsx) is a subset of is_busy()
    for name in [
        "printing",
        "paused",
        "pausing",
        "stopping",
        "file_transfer",
        "homing",
        "file_checking",
    ] {
        let s: MachineState = serde_json::from_value(json!(name)).unwrap();
        assert!(s.is_busy(), "{name}");
    }
}

#[test]
fn machine_state_names_match_the_app() {
    // app/src/api.ts `MachineState`
    use MachineState::*;
    let names = [
        (Unknown, "unknown"),
        (Offline, "offline"),
        (Idle, "idle"),
        (Printing, "printing"),
        (Pausing, "pausing"),
        (Paused, "paused"),
        (Stopping, "stopping"),
        (Stopped, "stopped"),
        (Completed, "completed"),
        (FileTransfer, "file_transfer"),
        (Homing, "homing"),
        (FileChecking, "file_checking"),
        (ExposureTest, "exposure_test"),
        (SelfTest, "self_test"),
        (Error, "error"),
    ];
    for (s, n) in names {
        assert_eq!(serde_json::to_value(s).unwrap(), json!(n));
        assert_eq!(serde_json::from_value::<MachineState>(json!(n)).unwrap(), s);
    }
    assert_eq!(MachineState::default(), Unknown);
}

#[test]
fn printer_status_json_matches_the_app() {
    // app/src/api.ts `PrinterStatus`
    let s = PrinterStatus {
        state: MachineState::Paused,
        detail: Some("paused".into()),
        filename: Some("a.goo".into()),
        current_layer: Some(1),
        total_layers: Some(2),
        elapsed_s: Some(3),
        total_s: Some(4),
        uv_led_temp: Some(5.5),
        box_temp: Some(6.5),
        error_code: Some(7),
        free_storage: Some(8),
        camera_connected: Some(true),
        time_lapse: Some(false),
    };
    let v = serde_json::to_value(&s).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "box_temp",
            "camera_connected",
            "current_layer",
            "detail",
            "elapsed_s",
            "error_code",
            "filename",
            "free_storage",
            "state",
            "time_lapse",
            "total_layers",
            "total_s",
            "uv_led_temp"
        ]
    );
    assert_eq!(v["state"], "paused");
    assert_eq!(serde_json::from_value::<PrinterStatus>(v).unwrap(), s);
    // `time_lapse` may be missing in older saved state
    let mut v = serde_json::to_value(PrinterStatus::default()).unwrap();
    v.as_object_mut().unwrap().remove("time_lapse");
    assert_eq!(serde_json::from_value::<PrinterStatus>(v).unwrap().time_lapse, None);
}

#[test]
fn protocol_and_file_entry_json() {
    assert_eq!(serde_json::to_value(Protocol::Sdcp).unwrap(), json!("sdcp"));
    assert_eq!(serde_json::to_value(Protocol::ChituUdp).unwrap(), json!("chitu-udp"));
    let f = FileEntry {
        path: "/local/a.goo".into(),
        is_dir: false,
        size: Some(10),
    };
    assert_eq!(
        serde_json::to_value(&f).unwrap(),
        json!({"path": "/local/a.goo", "is_dir": false, "size": 10})
    );
}

#[test]
fn chitu_m27_replies() {
    assert_eq!(parse_m27("SD printing byte 1200/34000"), Some((1200, 34000)));
    assert_eq!(parse_m27("ok SD printing byte 0/5 extra"), Some((0, 5)));
    assert_eq!(parse_m27("SD printing byte  7 / 9 "), Some((7, 9)));
    assert_eq!(parse_m27("Not SD printing."), None);
    assert_eq!(parse_m27("Error:It's not printing now!"), None);
    assert_eq!(parse_m27("SD printing byte x/9"), None);
    assert_eq!(parse_m27(""), None);
}

#[test]
fn chitu_m4000_replies() {
    let (d, z) = parse_m4000("ok B:0/0 X:0.000 Y:0.000 Z:12.500 F:256/256 D:500/1000/1");
    assert_eq!(d, Some((500, 1000, true)));
    assert_eq!(z, Some(12.5));
    let (d, z) = parse_m4000("ok B:0/0 Z:0.000 D:0/0/0");
    assert_eq!(d, Some((0, 0, false)));
    assert_eq!(z, Some(0.0));
    // two-part D field (no pause flag)
    assert_eq!(parse_m4000("D:3/4").0, Some((3, 4, false)));
    assert_eq!(parse_m4000("ok"), (None, None));
    assert_eq!(parse_m4000("D:a/b Z:zz"), (None, None));
}

#[test]
fn chitu_packet_layout_and_checksum() {
    for len in [0usize, 1, 7, CHUNK_SIZE] {
        let data: Vec<u8> = (0..len).map(|i| (i * 31 + 7) as u8).collect();
        let off = 0xA1B2_C3D4u32;
        let p = data_packet(&data, off);
        assert_eq!(p.len(), len + 6);
        assert_eq!(&p[..len], &data[..]);
        assert_eq!(&p[len..len + 4], &off.to_le_bytes());
        let xor = p[..len + 4].iter().fold(0u8, |a, b| a ^ b);
        assert_eq!(p[len + 4], xor);
        assert_eq!(p[len + 5], 0x83);
    }
    assert_eq!(CHUNK_SIZE, 0x500);
}

#[test]
fn chitu_file_names() {
    assert_eq!(sanitize_name("Mäx test (1).ctb"), "M_x_test__1_.ctb");
    assert_eq!(sanitize_name("plain-name_v2.ctb"), "plain-name_v2.ctb");
    assert_eq!(sanitize_name("a/b\\c:d.ctb"), "a_b_c_d.ctb");
    assert_eq!(sanitize_name("'quoted'.ctb"), "_quoted_.ctb");
}
