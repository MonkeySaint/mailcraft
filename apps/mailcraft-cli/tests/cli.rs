//! Runs the built `mailcraft-cli` binary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mailcraft-cli")).args(args).output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn ok(args: &[&str]) -> String {
    let o = cli(args);
    assert!(o.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    stdout(&o)
}

#[test]
fn version_and_help() {
    assert!(ok(&["--version"]).starts_with("mailcraft-cli "));
    assert!(ok(&["--help"]).contains("Usage: mailcraft-cli"));
    assert!(ok(&[]).contains("Commands:"));
    let bad = cli(&["frobnicate"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown command"));
}

#[test]
fn commands_table_and_json() {
    let table = ok(&["commands"]);
    assert!(table.lines().next().unwrap().starts_with("ID"));
    assert!(table.contains("mail.reply"));
    let list: Value = serde_json::from_str(&ok(&["commands", "--json", "--filter", "reply"])).unwrap();
    let list = list.as_array().unwrap();
    assert!(list.iter().any(|c| c["id"] == "mail.reply" && c["shortcut"] == "Mod+R"));
    assert!(list.iter().all(|c| c["enabled"].is_boolean()));
}

#[test]
fn parity_summary_and_json() {
    assert!(ok(&["parity"]).contains("Outlook parity:"));
    let p: Value = serde_json::from_str(&ok(&["parity", "--json"])).unwrap();
    assert!(p["total_rows"].as_u64().unwrap() > 1000);
    assert!(p["percent"].as_f64().is_some());
}

#[test]
fn run_and_query() {
    let r: Value = serde_json::from_str(&ok(&[
        "run",
        "--sample",
        "--cmd",
        "nav.selectAll",
        "--cmd",
        "search.query={\"text\": \"budget\"}",
        "--query",
        "status",
        "--query",
        "list",
    ]))
    .unwrap();
    assert_eq!(r["results"][0]["ok"], true);
    assert_eq!(r["results"][1]["result"]["query"], "budget");
    assert_eq!(r["query"]["status"]["folderName"], "Inbox");
    assert!(r["query"]["list"].is_array());

    let failed = cli(&["run", "--cmd", "no.such", "--cmd", "nav.selectAll"]);
    assert_eq!(failed.status.code(), Some(1));
    let r: Value = serde_json::from_str(&stdout(&failed)).unwrap();
    assert_eq!(r["results"].as_array().unwrap().len(), 1, "stops at the first failure");
    assert_eq!(r["results"][0]["ok"], false);
}

#[test]
fn show_folder() {
    let text = ok(&["show", "--sample", "--folder", "Inbox", "--limit", "5"]);
    assert!(text.starts_with("Inbox: "), "{text}");
    assert!(text.lines().count() >= 2);
    let bad = cli(&["show", "--folder", "Nowhere"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("no folder named"));
}

const EML: &str = "From: Alice Rivera <alice@example.com>\r\nTo: Jordan Lee <jordan.lee@example.net>\r\n\
Subject: Imported from the CLI test\r\nDate: Thu, 08 Oct 2026 09:10:00 +0000\r\nMessage-ID: <cli-test@example.com>\r\n\
MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello from an .eml file.\r\n";

#[test]
fn import_show_export_on_a_profile() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let eml = dir.path().join("one.eml");
    std::fs::write(&eml, EML).unwrap();
    let profile_s = profile.to_str().unwrap();
    let missing = cli(&["import-eml", eml.to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(2), "imports need an explicit --profile");
    let msg = ok(&["import-eml", "--profile", profile_s, eml.to_str().unwrap()]);
    assert!(msg.starts_with("Imported 1 message(s) into "), "{msg}");
    // A second process sees the imported message (it was saved to the profile).
    let text = ok(&["show", "--profile", profile_s]);
    assert!(text.contains("Imported from the CLI test"), "{text}");
    let mbox = dir.path().join("out.mbox");
    let inbox = text.split(':').next().unwrap().to_owned();
    let msg = ok(&["export-mbox", "--profile", profile_s, "--folder", &inbox, mbox.to_str().unwrap()]);
    assert!(msg.starts_with("Exported 1 message(s)"), "{msg}");
    let data = std::fs::read_to_string(&mbox).unwrap();
    assert!(data.starts_with("From "));
    let msg = ok(&["import-mbox", "--sample", mbox.to_str().unwrap()]);
    assert!(msg.starts_with("Imported 1 message(s)"), "{msg}");
    let bad = cli(&["import-mbox", "--sample", dir.path().join("nope.mbox").to_str().unwrap()]);
    assert_eq!(bad.status.code(), Some(1));
}

#[test]
fn mcp_over_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mailcraft-cli"))
        .args(["mcp", "--sample"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for msg in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "status", "arguments": {}}}),
        ] {
            writeln!(stdin, "{msg}").unwrap();
        }
    }
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success());
    let lines: Vec<Value> = stdout(&o).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 9);
    assert_eq!(lines[2]["result"]["isError"], false);
    assert!(String::from_utf8_lossy(&o.stderr).contains("serving headless"));
}

#[test]
fn mcp_connect_failure_is_an_error() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let o = cli(&["mcp", "--connect", &format!("127.0.0.1:{port}")]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("cannot connect"));
}
