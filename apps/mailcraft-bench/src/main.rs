//! `mailcraft-bench`: storage benchmarks on synthetic profiles (see the library docs and `cargo xtask bench`).

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use mailcraft_bench::{Backend, Options, Result, parse_size};

const USAGE: &str = "\
Usage: mailcraft-bench <command> [options]

Commands:
  run        generate a profile, measure it in a fresh process, write the JSON report
  generate   only write the profile (--profile DIR)
  measure    only measure an existing profile (--profile DIR); prints the measurements as JSON
  maildir    write the synthetic mailbox as a Maildir tree (--out DIR), for importer tests
  mbox       write the synthetic mailbox as one MBOX file (--out FILE), for importer tests

Options:
  --size N            messages: 2000, 10k, 40k, 100k, 1m… (default 10k)
  --backend B         legacy (default) or sqlite
  --small-bodies      bodies of a few KB (same structure; lets 1M-message runs fit on disk)
  --seed N            generator seed
  --sync N            messages applied as synchronised mail (default 10000)
  --bulk-move N       messages in the bulk move / undo / redo (default 10000)
  --generate-batch N  sources stored per fsync and commit while generating (default 1000; 1 = one per
                      source, as the app's import and sync store them)
  --work DIR          run: where the profile is built (default target/scratch/bench/work)
  --out PATH          run: report file (default target/scratch/bench/<date>-<backend>-<size>.json);
                      maildir / mbox: where to write
  --profile DIR       generate / measure: the profile folder
  --keep              run: keep the profile afterwards
";

#[derive(Debug, Default)]
struct Args {
    command: String,
    size: Option<usize>,
    backend: Option<Backend>,
    small: bool,
    seed: Option<u64>,
    sync: Option<usize>,
    bulk: Option<usize>,
    generate_batch: Option<usize>,
    work: Option<PathBuf>,
    out: Option<PathBuf>,
    profile: Option<PathBuf>,
    keep: bool,
}

fn parse(args: &[String]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    a.command = it.next().cloned().unwrap_or_default();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("`{name}` needs a value"));
        match arg.as_str() {
            "--size" => a.size = Some(parse_size(&value("--size")?)?),
            "--backend" => a.backend = Some(Backend::parse(&value("--backend")?)?),
            "--small-bodies" => a.small = true,
            "--seed" => a.seed = Some(value("--seed")?.parse().map_err(|_| "`--seed` needs a number")?),
            "--sync" => a.sync = Some(value("--sync")?.parse().map_err(|_| "`--sync` needs a number")?),
            "--bulk-move" => {
                a.bulk = Some(value("--bulk-move")?.parse().map_err(|_| "`--bulk-move` needs a number")?)
            }
            "--generate-batch" => {
                a.generate_batch = Some(
                    value("--generate-batch")?.parse().map_err(|_| "`--generate-batch` needs a number")?,
                )
            }
            "--work" => a.work = Some(PathBuf::from(value("--work")?)),
            "--out" => a.out = Some(PathBuf::from(value("--out")?)),
            "--profile" => a.profile = Some(PathBuf::from(value("--profile")?)),
            "--keep" => a.keep = true,
            other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
        }
    }
    Ok(a)
}

fn options(a: &Args) -> Options {
    let mut o = Options::new(a.size.unwrap_or(10_000));
    o.small_bodies = a.small;
    if let Some(b) = a.backend {
        o.backend = b;
    }
    if let Some(s) = a.seed {
        o.seed = s;
    }
    if let Some(n) = a.sync {
        o.sync_messages = n;
    }
    if let Some(n) = a.bulk {
        o.bulk_move = n;
    }
    if let Some(n) = a.generate_batch {
        o.generate_batch = n;
    }
    o
}

/// The flags that make `measure` in the child process see the same options.
fn child_args(o: &Options) -> Vec<String> {
    let mut v = vec![
        "--size".to_owned(),
        o.messages.to_string(),
        "--backend".to_owned(),
        o.backend.name().to_owned(),
        "--seed".to_owned(),
        o.seed.to_string(),
        "--sync".to_owned(),
        o.sync_messages.to_string(),
        "--bulk-move".to_owned(),
        o.bulk_move.to_string(),
    ];
    if o.small_bodies {
        v.push("--small-bodies".to_owned());
    }
    v
}

fn size_label(n: usize) -> String {
    if n >= 1_000_000 && n.is_multiple_of(1_000_000) {
        format!("{}m", n / 1_000_000)
    } else if n >= 1_000 && n.is_multiple_of(1_000) {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

fn write_json(path: &std::path::Path, v: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    std::fs::write(path, text + "\n").map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn run_all(a: &Args) -> Result<()> {
    let o = options(a);
    let base = PathBuf::from("target").join("scratch").join("bench");
    let work = a.work.clone().unwrap_or_else(|| base.join("work"));
    let small = if o.small_bodies { "-small" } else { "" };
    let out = a.out.clone().unwrap_or_else(|| {
        base.join(format!(
            "{}-{}-{}{small}.json",
            mailcraft_bench::today(),
            o.backend.name(),
            size_label(o.messages)
        ))
    });
    let profile = mailcraft_bench::profile_dir(&work);
    eprintln!("generating {} messages into {}", o.messages, profile.display());
    let generated = mailcraft_bench::generate(&profile, &o)?;
    eprintln!("measuring in a fresh process");
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let child = Command::new(exe)
        .arg("measure")
        .arg("--profile")
        .arg(&profile)
        .args(child_args(&o))
        .output()
        .map_err(|e| format!("cannot start the measuring process: {e}"))?;
    eprint!("{}", String::from_utf8_lossy(&child.stderr));
    if !child.status.success() {
        return Err(format!("the measuring process failed ({})", child.status));
    }
    let measured: serde_json::Value = serde_json::from_slice(&child.stdout)
        .map_err(|e| format!("the measuring process wrote bad JSON: {e}"))?;
    let report = mailcraft_bench::report(&o, generated, measured);
    write_json(&out, &report)?;
    if !a.keep {
        let _ = std::fs::remove_dir_all(&profile);
    }
    println!("{}", summary(&report));
    println!("report: {}", out.display());
    Ok(())
}

/// A few headline numbers for the terminal.
fn summary(r: &serde_json::Value) -> String {
    let g = |p: &str| r.pointer(p).map(|v| v.to_string()).unwrap_or_else(|| "-".into());
    format!(
        "{} messages ({}): open {} ms, first window {} ms, RSS after open {} kB, folder switch {} ms, \
         index build {} ms, sync full {}/s, headers {}/s, mark read {} ms, move 1 {} ms, bulk move {} ms, \
         undo {} ms, disk {} bytes",
        g("/messages"),
        g("/backend"),
        g("/measure/open/ms"),
        g("/measure/open/first_window_ms"),
        g("/measure/open/memory_after/rss_kb"),
        g("/measure/folder_switch/date_sort_ms"),
        g("/measure/search/index_build_ms"),
        g("/measure/sync/full_per_sec"),
        g("/measure/sync/headers_per_sec"),
        g("/measure/commands/mark_read_1_ms"),
        g("/measure/commands/move_1_ms"),
        g("/measure/commands/bulk_move_ms"),
        g("/measure/commands/undo_bulk_move_ms"),
        g("/measure/disk_end/total"),
    )
}

fn main_inner(args: &[String]) -> Result<()> {
    let a = parse(args)?;
    match a.command.as_str() {
        "" | "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        "run" => run_all(&a),
        "generate" => {
            let dir = a.profile.clone().ok_or("`generate` needs --profile DIR")?;
            let v = mailcraft_bench::generate(&dir, &options(&a))?;
            println!("{}", serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?);
            Ok(())
        }
        "measure" => {
            let dir = a.profile.clone().ok_or("`measure` needs --profile DIR")?;
            let v = mailcraft_bench::measure(&dir, &options(&a))?;
            println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
            Ok(())
        }
        "maildir" => {
            let dir = a.out.clone().ok_or("`maildir` needs --out DIR")?;
            let n = mailcraft_testmail::synth::write_maildir(&dir, &options(&a).config())
                .map_err(|e| format!("cannot write {}: {e}", dir.display()))?;
            println!("{n} messages written to {}", dir.display());
            Ok(())
        }
        "mbox" => {
            let file = a.out.clone().ok_or("`mbox` needs --out FILE")?;
            let n = mailcraft_testmail::synth::write_mbox(&file, &options(&a).config())
                .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
            println!("{n} messages written to {}", file.display());
            Ok(())
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match main_inner(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mailcraft-bench: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_round_trip_to_the_child() {
        let args: Vec<String> =
            ["run", "--size", "40k", "--small-bodies", "--seed", "9", "--sync", "100", "--bulk-move", "50"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
        let a = parse(&args).unwrap();
        let o = options(&a);
        assert_eq!(
            (o.messages, o.small_bodies, o.seed, o.sync_messages, o.bulk_move),
            (40_000, true, 9, 100, 50)
        );
        let mut child = vec!["measure".to_owned()];
        child.extend(child_args(&o));
        assert_eq!(options(&parse(&child).unwrap()), o);
        assert_eq!(o.generate_batch, mailcraft_bench::GENERATE_BATCH);
        let one: Vec<String> =
            ["generate", "--generate-batch", "1"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(options(&parse(&one).unwrap()).generate_batch, 1);
        assert!(parse(&["run".to_owned(), "--generate-batch".to_owned(), "x".to_owned()]).is_err());
        assert!(parse(&["run".to_owned(), "--size".to_owned()]).is_err());
        assert!(parse(&["run".to_owned(), "--backend".to_owned(), "pst".to_owned()]).is_err());
        assert_eq!(size_label(40_000), "40k");
        assert_eq!(size_label(1_000_000), "1m");
        assert_eq!(size_label(2_500), "2500");
    }
}
