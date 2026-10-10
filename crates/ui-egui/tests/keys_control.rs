//! Control channel round trip over a real socket on an ephemeral port.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use mailcraft_engine::sample::sample_session;
use mailcraft_ui_egui::App;
use mailcraft_ui_egui::control::{Control, encode_png};
use serde_json::{Value, json};

/// Sends each request line and collects the response lines (client side, on its own thread).
fn client(port: u16, requests: Vec<String>) -> Vec<Value> {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let mut out = Vec::new();
    for r in requests {
        writer.write_all(r.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
        writer.flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        out.push(serde_json::from_str(line.trim()).unwrap());
    }
    out
}

#[test]
fn control_round_trip() {
    let mut control = Control::start(0).expect("bind an ephemeral port");
    let port = control.port();
    assert_ne!(port, 0);
    let ctx = egui::Context::default();
    let mut app = App::new(sample_session());
    let composes_before = app.s.composes.len();

    let requests = vec![
        json!({"id": 1, "cmd": "ping"}).to_string(),
        json!({"id": 2, "cmd": "query", "what": "status"}).to_string(),
        json!({"id": 3, "cmd": "query", "what": "list_rows"}).to_string(),
        json!({"id": 4, "cmd": "query", "what": "folder_rows"}).to_string(),
        json!({"id": 5, "cmd": "run", "command": "nav.selectAll", "params": {}}).to_string(),
        json!({"id": 6, "cmd": "query", "what": "selection"}).to_string(),
        json!({"id": 7, "cmd": "key", "keys": "Mod+Shift+M"}).to_string(),
        json!({"id": 8, "cmd": "query", "what": "composes"}).to_string(),
        json!({"id": 9, "cmd": "run", "command": "no.such"}).to_string(),
        "this is not json".to_string(),
        json!({"id": 11, "cmd": "dance"}).to_string(),
        json!({"id": 12, "cmd": "resize", "w": 1024, "h": 700}).to_string(),
        json!({"id": 13, "cmd": "resize", "w": -5, "h": 700}).to_string(),
        json!({"id": 14, "cmd": "query", "what": "ui"}).to_string(),
        json!({"id": 15, "cmd": "query", "what": "commands"}).to_string(),
        json!({"id": 16, "cmd": "key", "keys": "Mod+R", "surface": {"compose": 999}}).to_string(),
    ];
    let n = requests.len();
    let handle = std::thread::spawn(move || client(port, requests));

    // The UI thread: poll every "frame" until the client is done.
    let start = Instant::now();
    while !handle.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(30), "control round trip timed out");
        control.poll(&ctx, &mut app);
        std::thread::sleep(Duration::from_millis(2));
    }
    let r = handle.join().unwrap();
    assert_eq!(r.len(), n);

    assert_eq!(r[0], json!({"id": 1, "ok": true, "result": {"pong": true}}));
    assert_eq!(r[1]["id"], 2);
    assert!(r[1]["result"]["items"].as_u64().unwrap() > 0, "{}", r[1]);
    assert_eq!(r[1]["result"]["module"], "Mail");
    assert!(r[2]["result"].as_array().is_some_and(|a| !a.is_empty()), "{}", r[2]);
    assert!(r[3]["result"].as_array().is_some_and(|a| !a.is_empty()));
    assert_eq!(r[4]["ok"], true, "{}", r[4]);
    let selected = r[5]["result"]["items"].as_array().map_or(0, Vec::len);
    assert!(selected > 1, "Select All selected {selected}");
    assert_eq!(r[6]["ok"], true);
    assert_eq!(r[6]["result"]["command"], "mail.newEmail");
    assert_eq!(r[7]["result"].as_array().map_or(0, Vec::len), composes_before + 1);
    assert_eq!(r[8]["ok"], false);
    assert_eq!(r[8]["id"], 9);
    assert!(r[8]["error"].as_str().unwrap().contains("no.such"), "{}", r[8]);
    assert_eq!(r[9]["ok"], false);
    assert!(r[9]["error"].as_str().unwrap().contains("invalid JSON"));
    assert_eq!(r[10]["ok"], false);
    assert_eq!(r[11]["ok"], true);
    assert_eq!(r[11]["result"], json!({"w": 1024.0, "h": 700.0}));
    assert_eq!(r[12]["ok"], false);
    assert_eq!(r[13]["result"]["searchActive"], false);
    assert!(r[14]["result"].as_array().is_some_and(|a| a.iter().any(|c| c["id"] == "mail.reply")));
    assert_eq!(r[15]["ok"], true, "{}", r[15]);
    assert_eq!(app.s.composes.len(), composes_before + 1);
}

#[test]
fn two_clients_and_drop() {
    let mut control = Control::start(0).unwrap();
    let port = control.port();
    let ctx = egui::Context::default();
    let mut app = App::new(sample_session());
    let a = std::thread::spawn(move || client(port, vec![json!({"id": "a", "cmd": "ping"}).to_string()]));
    let b = std::thread::spawn(move || client(port, vec![json!({"id": "b", "cmd": "ping"}).to_string()]));
    let start = Instant::now();
    while !(a.is_finished() && b.is_finished()) {
        assert!(start.elapsed() < Duration::from_secs(30));
        control.poll(&ctx, &mut app);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(a.join().unwrap()[0]["id"], "a");
    assert_eq!(b.join().unwrap()[0]["id"], "b");
    drop(control);
    // A default (not listening) control polls harmlessly.
    let mut idle = Control::default();
    assert_eq!(idle.port(), 0);
    idle.poll(&ctx, &mut app);
}

#[test]
fn png_encoder_writes_a_valid_header() {
    // 2×1 RGBA: filter byte + 8 bytes.
    let png = encode_png(2, 1, &[0, 255, 0, 0, 255, 0, 0, 255, 255]);
    assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    assert_eq!(&png[12..16], b"IHDR");
    assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
}
