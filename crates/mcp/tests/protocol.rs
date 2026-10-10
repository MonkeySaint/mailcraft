//! Drives the MCP server with in-memory readers/writers (headless backend) and a fake control channel
//! (bridged backend).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::TcpListener;

use mailcraft_mcp::{
    Backend, BridgedBackend, HeadlessBackend, MAX_REQUEST_BYTES, Server, code, serve, session_query,
};
use serde_json::{Value, json};

/// Runs `lines` through a fresh sample server; returns the response lines.
fn exchange(lines: &[String]) -> Vec<Value> {
    let mut server = Server::new(HeadlessBackend::sample());
    exchange_with(&mut server, lines)
}

fn exchange_with<B: Backend>(server: &mut Server<B>, lines: &[String]) -> Vec<Value> {
    let input = lines.iter().map(|l| format!("{l}\n")).collect::<String>();
    let mut out = Vec::new();
    serve(server, Cursor::new(input.into_bytes()), &mut out).unwrap();
    String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn req(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn call(id: u64, tool: &str, args: Value) -> String {
    req(id, "tools/call", json!({"name": tool, "arguments": args}))
}

/// The JSON a successful tool call returned.
fn tool_json(resp: &Value) -> Value {
    assert_eq!(resp["result"]["isError"], false, "tool failed: {resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

fn tool_error(resp: &Value) -> String {
    assert_eq!(resp["result"]["isError"], true, "tool should fail: {resp}");
    resp["result"]["content"][0]["text"].as_str().unwrap().to_owned()
}

#[test]
fn handshake_and_tool_list() {
    let out = exchange(&[
        req(
            1,
            "initialize",
            json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
        ),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        req(2, "tools/list", json!({})),
        req(3, "ping", Value::Null),
    ]);
    assert_eq!(out.len(), 3, "notifications get no response: {out:?}");
    assert_eq!(out[0]["id"], 1);
    assert_eq!(out[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(out[0]["result"]["serverInfo"]["name"], "mailcraft");
    assert!(out[0]["result"]["capabilities"]["tools"].is_object());
    let tools = out[1]["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for want in [
        "list_commands",
        "run_command",
        "list_folders",
        "list_messages",
        "read_message",
        "select",
        "search",
        "status",
        "parity",
    ] {
        assert!(names.contains(&want), "missing tool {want}");
    }
    assert_eq!(out[2]["result"], json!({}));
}

#[test]
fn unknown_protocol_version_gets_ours() {
    let out = exchange(&[req(1, "initialize", json!({"protocolVersion": "1999-01-01"}))]);
    assert_eq!(out[0]["result"]["protocolVersion"], mailcraft_mcp::PROTOCOL_VERSION);
}

#[test]
fn protocol_errors() {
    let out = exchange(&[
        "{not json".into(),
        "[]".into(),
        "42".into(),
        json!({"jsonrpc": "2.0", "id": 1}).to_string(),
        req(2, "no/such/method", json!({})),
        json!({"jsonrpc": "2.0", "id": {"x": 1}, "method": "ping"}).to_string(),
        json!({"jsonrpc": "1.0", "id": 3, "method": "ping"}).to_string(),
        json!({"jsonrpc": "2.0", "id": 4, "method": "ping", "params": [1]}).to_string(),
        req(5, "tools/call", json!({"name": "no_such_tool"})),
        req(6, "tools/call", json!({})),
        json!({"jsonrpc": "2.0", "id": 99, "result": {}}).to_string(),
        String::new(),
        req(7, "ping", json!({})),
    ]);
    let codes: Vec<i64> = out.iter().filter_map(|r| r["error"]["code"].as_i64()).collect();
    assert_eq!(
        codes,
        vec![
            code::PARSE_ERROR,
            code::INVALID_REQUEST,
            code::INVALID_REQUEST,
            code::INVALID_REQUEST,
            code::METHOD_NOT_FOUND,
            code::INVALID_REQUEST,
            code::INVALID_REQUEST,
            code::INVALID_PARAMS,
            code::INVALID_PARAMS,
            code::INVALID_PARAMS,
        ]
    );
    assert_eq!(out.last().unwrap()["id"], 7, "the server keeps going after errors");
    assert_eq!(out.last().unwrap()["result"], json!({}));
}

#[test]
fn oversized_and_non_utf8_lines_are_rejected_and_skipped() {
    let big = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"pad\":\"{}\"}}",
        "x".repeat(MAX_REQUEST_BYTES)
    );
    let mut input = big.into_bytes();
    input.push(b'\n');
    input.extend_from_slice(&[0xff, 0xfe, b'\n']);
    input.extend_from_slice(req(2, "ping", json!({})).as_bytes());
    input.push(b'\n');
    let mut server = Server::new(HeadlessBackend::sample());
    let mut out = Vec::new();
    serve(&mut server, Cursor::new(input), &mut out).unwrap();
    let lines: Vec<Value> =
        String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["error"]["code"], code::INVALID_REQUEST);
    assert!(lines[0]["error"]["message"].as_str().unwrap().contains("larger than"));
    assert_eq!(lines[1]["error"]["code"], code::PARSE_ERROR);
    assert_eq!(lines[2]["id"], 2);
}

#[test]
fn batches() {
    let batch = json!([
        {"jsonrpc": "2.0", "id": 1, "method": "ping"},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "nope"}
    ]);
    let out = exchange(&[batch.to_string()]);
    let arr = out[0].as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["id"], 1);
    assert_eq!(arr[1]["error"]["code"], code::METHOD_NOT_FOUND);
}

#[test]
fn reading_mail() {
    let out = exchange(&[
        call(1, "list_folders", json!({})),
        call(2, "list_folders", json!({"all": true})),
        call(3, "list_messages", json!({"folder": "Inbox", "limit": 5})),
        call(4, "status", json!({})),
    ]);
    let rows = tool_json(&out[0]);
    let rows = rows["rows"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["name"] == "Inbox"));
    let all = tool_json(&out[1]);
    assert!(all["count"].as_u64().unwrap() >= rows.len() as u64 / 2);
    assert!(all["folders"].as_array().unwrap().iter().any(|f| f["kind"] == "Calendar"));
    let list = tool_json(&out[2]);
    assert_eq!(list["folderName"], "Inbox");
    assert!(list["rows"].as_array().unwrap().len() <= 5);
    assert!(list["items"].as_u64().unwrap() > 0);
    let status = tool_json(&out[3]);
    assert_eq!(status["folderName"], "Inbox");
    assert!(status["items"].as_u64().unwrap() > 0);
}

/// First item id in the Inbox.
fn first_inbox_item() -> u64 {
    let out = exchange(&[call(1, "list_messages", json!({"folder": "inbox", "limit": 1000}))]);
    let list = tool_json(&out[0]);
    list["rows"].as_array().unwrap().iter().find(|r| r["type"] == "item").unwrap()["id"].as_u64().unwrap()
}

#[test]
fn read_select_and_run() {
    let id = first_inbox_item();
    let out = exchange(&[
        call(1, "read_message", json!({"id": id})),
        call(2, "read_message", json!({"id": id, "max_chars": 3})),
        call(3, "select", json!({"folder": "Inbox", "item": id})),
        call(4, "run_command", json!({"id": "mail.reply"})),
        call(5, "run_command", json!({"id": "app.undo", "params": {}})),
        call(6, "read_message", json!({"id": 999_999_999})),
        call(7, "run_command", json!({"id": "no.such"})),
        call(8, "select", json!({})),
    ]);
    let msg = tool_json(&out[0]);
    assert_eq!(msg["id"], id);
    assert!(!msg["header"]["subject"].as_str().unwrap().is_empty());
    assert!(msg["body"].as_str().is_some());
    let short = tool_json(&out[1]);
    assert!(short["body"].as_str().unwrap().chars().count() <= 3);
    let sel = tool_json(&out[2]);
    assert_eq!(sel["items"], json!([id]));
    let reply = tool_json(&out[3]);
    assert_eq!(reply["command"], "mail.reply");
    assert!(reply["notices"].is_array(), "reply opens a compose window: {reply}");
    assert!(out[4]["result"]["isError"].is_boolean());
    assert!(tool_error(&out[5]).contains("no message"));
    assert!(tool_error(&out[6]).contains("unknown command"));
    assert!(tool_error(&out[7]).contains("folder"));
}

#[test]
fn search_and_commands_and_parity() {
    let out = exchange(&[
        call(1, "search", json!({"query": "budget"})),
        call(2, "search", json!({"query": ""})),
        call(3, "list_commands", json!({"filter": "reply"})),
        call(4, "list_commands", json!({})),
        call(5, "parity", json!({})),
        call(6, "list_messages", json!({"folder": "No Such Folder"})),
    ]);
    let s = tool_json(&out[0]);
    assert_eq!(s["search"]["query"], "budget");
    assert!(s["rows"].is_array());
    let cleared = tool_json(&out[1]);
    assert_eq!(cleared["search"]["query"], "");
    let reply = tool_json(&out[2]);
    let ids: Vec<&str> =
        reply["commands"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"mail.reply"));
    let all = tool_json(&out[3]);
    assert!(all["count"].as_u64().unwrap() > 300);
    let parity = tool_json(&out[4]);
    assert!(parity["total_rows"].as_u64().unwrap() > 1000);
    assert!(tool_error(&out[5]).contains("no folder named"));
}

#[test]
fn junk_arguments_never_break_the_server() {
    let junk = [
        Value::Null,
        json!(1),
        json!("x"),
        json!([1, 2]),
        json!({}),
        json!({"id": -1, "folder": -3, "item": "x", "limit": -1, "offset": "a", "query": 5, "all": "yes"}),
        json!({"id": "", "params": [1], "filter": {}, "max_chars": 1e300}),
        json!({"id": "mail.delete", "params": {"ids": [-1, "a", null], "folder": {}}}),
        json!({"folder": "", "item": u64::MAX, "query": "\u{0}\u{ffff}"}),
        json!({"folder": "\\\\//", "limit": u64::MAX, "offset": u64::MAX}),
    ];
    let tools = mailcraft_mcp::tools::TOOL_NAMES;
    let mut lines = Vec::new();
    let mut n = 0;
    for tool in tools {
        for args in &junk {
            n += 1;
            lines.push(req(n, "tools/call", json!({"name": tool, "arguments": args})));
        }
    }
    let out = exchange(&lines);
    assert_eq!(out.len(), lines.len());
    for r in &out {
        assert!(r["result"]["content"].is_array() || r["error"].is_object(), "{r}");
    }
}

#[test]
fn profile_backend_persists() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut server = Server::new(HeadlessBackend::open(dir.path()).unwrap());
        let out = exchange_with(
            &mut server,
            &[call(1, "run_command", json!({"id": "folder.new", "params": {"name": "Receipts 2026"}}))],
        );
        tool_json(&out[0]);
    }
    // A new server over the same folder sees the change.
    let mut server = Server::new(HeadlessBackend::open(dir.path()).unwrap());
    let out = exchange_with(&mut server, &[call(1, "list_folders", json!({"all": true}))]);
    let all = tool_json(&out[0]);
    assert!(all["folders"].as_array().unwrap().iter().any(|f| f["name"] == "Receipts 2026"), "{all}");
}

// ---------------------------------------------------------------------------------------------------------
// Bridged

/// A stand-in for the app's control channel (`crates/ui-egui/src/control.rs`) backed by a headless session.
fn fake_app() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let mut backend = HeadlessBackend::sample();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let mut writer = stream.try_clone().unwrap();
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                let id = req.get("id").cloned().unwrap_or(Value::Null);
                let r = match req["cmd"].as_str() {
                    Some("ping") => Ok(json!({"pong": true})),
                    Some("run") => backend.run(req["command"].as_str().unwrap_or(""), &req["params"]),
                    Some("query") => {
                        session_query(backend.session(), req["what"].as_str().unwrap_or(""), &req)
                    }
                    _ => Err("unknown cmd".to_owned()),
                };
                let resp = match r {
                    Ok(result) => json!({"id": id, "ok": true, "result": result}),
                    Err(error) => json!({"id": id, "ok": false, "error": error}),
                };
                if writeln!(writer, "{resp}").is_err() {
                    break;
                }
            }
        }
    });
    addr
}

#[test]
fn bridged_backend_drives_the_app() {
    let addr = fake_app();
    let backend = BridgedBackend::connect(&addr.to_string()).unwrap();
    let mut server = Server::new(backend);
    let out = exchange_with(
        &mut server,
        &[
            req(1, "initialize", json!({})),
            call(2, "list_messages", json!({"folder": "Inbox", "limit": 3})),
            call(3, "status", json!({})),
            call(4, "run_command", json!({"id": "nav.selectAll"})),
            call(5, "run_command", json!({"id": "no.such"})),
            call(6, "list_commands", json!({"enabled_only": true})),
            call(7, "list_folders", json!({"all": true})),
        ],
    );
    assert!(out[0]["result"]["instructions"].as_str().unwrap().contains("bridged"));
    let list = tool_json(&out[1]);
    assert_eq!(list["folderName"], "Inbox");
    let first = list["rows"].as_array().unwrap().iter().find(|r| r["type"] == "item").unwrap()["id"]
        .as_u64()
        .unwrap();
    assert_eq!(tool_json(&out[2])["folderName"], "Inbox");
    tool_json(&out[3]);
    assert!(tool_error(&out[4]).contains("unknown command"));
    assert!(tool_json(&out[5])["count"].as_u64().unwrap() > 0);
    assert!(tool_json(&out[6])["folders"].as_array().unwrap().iter().any(|f| f["path"] == "Inbox"));
    // The fake app has no `body` query (like older apps): the header still comes back.
    let out = exchange_with(&mut server, &[call(8, "read_message", json!({"id": first}))]);
    let msg = tool_json(&out[0]);
    assert!(msg["body"].is_null());
    assert!(!msg["header"]["subject"].as_str().unwrap().is_empty());
}

#[test]
fn bridged_backend_reports_a_missing_app() {
    // Bind then drop a listener to get a port nothing listens on.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let err = BridgedBackend::connect(&format!("127.0.0.1:{port}")).unwrap_err();
    assert!(err.contains("cannot connect"), "{err}");
    assert!(BridgedBackend::connect("not an address").is_err());
}
