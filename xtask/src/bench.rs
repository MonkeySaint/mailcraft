//! `cargo xtask bench`: builds `apps/mailcraft-bench` in release mode and runs it on a synthetic profile,
//! writing the JSON report under `target/scratch/bench/` (storage ADR, section 6.1).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{TaskResult, cargo};

/// Options of one run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Bench {
    pub size: String,
    pub backend: String,
    pub small_bodies: bool,
    pub out: Option<PathBuf>,
    /// Options passed through unchanged (`--seed`, `--sync`, `--bulk-move`, `--keep`, `--force-large`).
    pub extra: Vec<String>,
}

/// Parses `[--size 10k|50k | --size 100k|1m|2m --small-bodies] [--backend legacy|sqlite] [--out FILE]
/// [--seed N] [--sync N] [--bulk-move N] [--keep] [--force-large]`. `mailcraft-bench` checks the size
/// against the body kind and the machine before it writes anything.
pub fn parse(args: &[String]) -> Result<Bench, String> {
    let mut b = Bench { size: "10k".into(), backend: "legacy".into(), ..Default::default() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("`{name}` needs a value"));
        match a.as_str() {
            "--size" => b.size = value("--size")?,
            "--backend" => {
                b.backend = value("--backend")?;
                if !matches!(b.backend.as_str(), "legacy" | "sqlite") {
                    return Err(format!("`--backend` is legacy or sqlite, not `{}`", b.backend));
                }
            }
            "--small-bodies" => b.small_bodies = true,
            "--out" => b.out = Some(PathBuf::from(value("--out")?)),
            "--keep" | "--force-large" => b.extra.push(a.clone()),
            "--seed" | "--sync" | "--bulk-move" => {
                let v = value(a)?;
                b.extra.extend([a.clone(), v]);
            }
            other => return Err(format!("`bench` does not take `{other}`")),
        }
    }
    Ok(b)
}

/// The `mailcraft-bench run` arguments for `b` in repository `root`.
pub fn bench_args(root: &Path, b: &Bench, date: &str) -> Vec<String> {
    let dir = root.join("target").join("scratch").join("bench");
    let small = if b.small_bodies { "-small" } else { "" };
    let out =
        b.out.clone().unwrap_or_else(|| dir.join(format!("{date}-{}-{}{small}.json", b.backend, b.size)));
    let work = dir.join(format!("work-{}-{}{small}", b.backend, b.size));
    let mut v = vec![
        "run".to_owned(),
        "--size".to_owned(),
        b.size.clone(),
        "--backend".to_owned(),
        b.backend.clone(),
        "--work".to_owned(),
        work.to_string_lossy().into_owned(),
        "--out".to_owned(),
        out.to_string_lossy().into_owned(),
    ];
    if b.small_bodies {
        v.push("--small-bodies".to_owned());
    }
    v.extend(b.extra.iter().cloned());
    v
}

fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
        .unwrap_or(0);
    let (y, m, d) = mailcraft_engine::model::dates::civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn run(root: &Path, args: &[String]) -> TaskResult {
    let b = parse(args)?;
    let mut cmd = Command::new(cargo());
    cmd.current_dir(root).args(["run", "--release", "-p", "mailcraft-bench", "--"]).args(bench_args(
        root,
        &b,
        &today(),
    ));
    println!(
        "==> bench: {} messages, {} backend{}",
        b.size,
        b.backend,
        if b.small_bodies { ", small bodies" } else { "" }
    );
    let status = cmd.status().map_err(|e| format!("bench: cannot run cargo: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("bench: mailcraft-bench failed ({status})")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn arguments_reach_the_runner() {
        let b = parse(&s(&["--size", "40k", "--small-bodies", "--seed", "3", "--keep", "--force-large"]))
            .unwrap();
        assert_eq!(b.size, "40k");
        assert_eq!(b.backend, "legacy");
        let args = bench_args(Path::new("/repo"), &b, "2026-10-28");
        let joined = args.join(" ");
        assert!(joined.starts_with("run --size 40k --backend legacy --work "), "{joined}");
        assert!(joined.contains("2026-10-28-legacy-40k-small.json"), "{joined}");
        assert!(joined.ends_with("--small-bodies --seed 3 --keep --force-large"), "{joined}");
        assert!(parse(&s(&["--backend", "pst"])).is_err());
        assert!(parse(&s(&["--size"])).is_err());
        assert!(parse(&s(&["--fast"])).is_err());
        let out = parse(&s(&["--out", "x.json"])).unwrap();
        assert!(bench_args(Path::new("/repo"), &out, "d").contains(&"x.json".to_owned()));
    }
}
