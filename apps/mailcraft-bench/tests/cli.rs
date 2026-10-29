//! The `mailcraft-bench` program end to end: `run` generates, measures in a child process (`measure`,
//! whose stdout is the JSON), writes the report and deletes the profile; `generate` and `measure` refuse a
//! folder that is not a benchmark profile.

use std::process::Command;

use serde_json::Value;

const EXE: &str = env!("CARGO_BIN_EXE_mailcraft-bench");

fn work_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mailcraft-bench-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    assert!(std::fs::create_dir_all(&d).is_ok(), "{}", d.display());
    d
}

#[test]
fn run_measures_in_a_child_and_writes_the_report() {
    let work = work_dir("run");
    let out = work.join("r.json");
    let o = Command::new(EXE)
        .args(["run", "--size", "200", "--small-bodies", "--sync", "50", "--bulk-move", "20", "--work"])
        .arg(work.join("w"))
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let r: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(r["messages"], 200);
    assert_eq!(r["generate"]["messages"], 200);
    assert_eq!(r["measure"]["open"]["mail_items"], 200);
    assert_eq!(r["measure"]["commands"]["mail_items"], 200);
    assert_eq!(r["measure"]["sync"]["inbox_items"], 50);
    assert_eq!(r["measure"]["commands"]["bulk_move_items"], 20);
    assert!(String::from_utf8_lossy(&o.stdout).contains("report:"));
    assert!(!mailcraft_bench::profile_dir(&work.join("w")).exists(), "the profile is deleted");
    std::fs::remove_dir_all(&work).unwrap();
}

#[test]
fn generate_and_measure_refuse_other_folders() {
    let work = work_dir("refuse");
    std::fs::write(work.join("mailbox.json"), "{}").unwrap();
    for cmd in ["generate", "measure"] {
        let o = Command::new(EXE).args([cmd, "--size", "20", "--profile"]).arg(&work).output().unwrap();
        assert!(!o.status.success(), "{cmd}");
        assert!(String::from_utf8_lossy(&o.stderr).contains("not a benchmark profile"), "{cmd}");
        assert_eq!(std::fs::read_to_string(work.join("mailbox.json")).unwrap(), "{}");
    }
    let p = work.join("p");
    let o = Command::new(EXE).args(["generate", "--size", "100k", "--profile"]).arg(&p).output().unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--small-bodies"));
    assert!(!p.exists());
    std::fs::remove_dir_all(&work).unwrap();
}
