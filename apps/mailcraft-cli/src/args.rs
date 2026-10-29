//! Hand-rolled argument parsing (no dependencies): `mailcraft-cli <command> [options]`.

use std::path::PathBuf;

use mailcraft_engine::StoreKind;
use serde_json::{Value, json};

pub const USAGE: &str = "\
MailCraft command-line tool

Usage: mailcraft-cli <command> [options]

Commands:
  commands [--json] [--filter TEXT]
        Every command (id, shortcut, location, label); --json adds params and enabled state.
  parity [--json]
        Feature parity: implemented share of the observed feature catalog.
  run [--sample | --profile DIR] --cmd 'ID[={json params}]'... [--query list|folders|status|selection]...
        Runs commands in order (stops at the first failure) and prints the results as JSON.
  show [--sample | --profile DIR] [--folder NAME] [--limit N]
        Prints a folder's message list as text (default: the Inbox).
  import-eml (--profile DIR | --sample) [--folder NAME] FILE...
  import-mbox (--profile DIR | --sample) [--folder NAME] FILE
  export-mbox (--profile DIR | --sample) --folder NAME FILE
        Imports .eml / MBOX files into a folder (default: the Inbox), or exports a folder to MBOX.
  mcp [--sample | --profile DIR | --connect HOST:PORT]
        Runs the MCP server on stdin/stdout: headless over the sample mailbox (default) or a profile, or
        bridged to a running app started with --control-port PORT.

Options:
  --sample         Use the built-in sample mailbox (in memory; nothing is saved). Default for run/show/mcp.
  --profile DIR    Use (or create) the MailCraft data folder DIR; changes are saved there.
  --store KIND     Storage of a new --profile folder: legacy (default) or sqlite. An existing profile keeps
                   its own. MAILCRAFT_STORE=sqlite does the same.
  -h, --help       Show this help.
  -V, --version    Show the version.
";

/// Which mailbox a command works on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Sample,
    /// A profile folder, and the storage a new one gets (`--store`; `None`: `MAILCRAFT_STORE`).
    Profile(PathBuf, Option<StoreKind>),
}

/// What `--query` reads after `run`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    List,
    Folders,
    Status,
    Selection,
}

impl Query {
    /// The control channel / `session_query` name.
    pub fn name(self) -> &'static str {
        match self {
            Query::List => "list_rows",
            Query::Folders => "folder_rows",
            Query::Status => "status",
            Query::Selection => "selection",
        }
    }
    /// The `--query` spelling.
    pub fn key(self) -> &'static str {
        match self {
            Query::List => "list",
            Query::Folders => "folders",
            Query::Status => "status",
            Query::Selection => "selection",
        }
    }
    fn parse(s: &str) -> Result<Query, String> {
        match s {
            "list" | "list_rows" => Ok(Query::List),
            "folders" | "folder_rows" => Ok(Query::Folders),
            "status" => Ok(Query::Status),
            "selection" => Ok(Query::Selection),
            other => Err(format!("unknown query `{other}` (list, folders, status, selection)")),
        }
    }
}

/// Where `mcp` sends tool calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpTarget {
    Headless(Source),
    Connect(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cli {
    Help,
    Version,
    Commands { json: bool, filter: Option<String> },
    Parity { json: bool },
    Run { source: Source, cmds: Vec<(String, Value)>, queries: Vec<Query> },
    Show { source: Source, folder: Option<String>, limit: usize },
    ImportEml { source: Source, folder: Option<String>, files: Vec<PathBuf> },
    ImportMbox { source: Source, folder: Option<String>, file: PathBuf },
    ExportMbox { source: Source, folder: String, file: PathBuf },
    Mcp { target: McpTarget },
}

/// Splits `--name=value` into (`--name`, Some(value)).
fn split_flag(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((k, v)) if k.starts_with("--") => (k, Some(v)),
        _ => (arg, None),
    }
}

/// Parses `ID` or `ID={json}` (`ID=` alone means `{}`).
pub fn parse_cmd(spec: &str) -> Result<(String, Value), String> {
    let (id, params) = match spec.split_once('=') {
        Some((id, p)) => (id.trim(), p.trim()),
        None => (spec.trim(), ""),
    };
    if id.is_empty() || id.contains(char::is_whitespace) {
        return Err(format!("`{spec}`: expected a command id like mail.reply or mail.reply={{\"id\": 3}}"));
    }
    let params = if params.is_empty() {
        json!({})
    } else {
        let v: Value =
            serde_json::from_str(params).map_err(|e| format!("`{id}`: params are not valid JSON: {e}"))?;
        if !v.is_object() {
            return Err(format!("`{id}`: params must be a JSON object"));
        }
        v
    };
    Ok((id.to_owned(), params))
}

struct Args {
    items: std::vec::IntoIter<String>,
}

impl Args {
    fn value(&mut self, flag: &str, inline: Option<&str>) -> Result<String, String> {
        match inline {
            Some(v) => Ok(v.to_owned()),
            None => self.items.next().ok_or_else(|| format!("`{flag}` needs a value")),
        }
    }
}

fn set_source(s: Source, cur: &mut Option<Source>) -> Result<(), String> {
    if cur.as_ref().is_some_and(|c| *c != s) {
        return Err("give only one of --sample and --profile".into());
    }
    *cur = Some(s);
    Ok(())
}

/// Parses the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut items = args.into_iter().collect::<Vec<String>>().into_iter();
    let Some(first) = items.next() else { return Ok(Cli::Help) };
    let mut a = Args { items };
    let sub = first.as_str();
    match sub {
        "-h" | "--help" | "help" => return Ok(Cli::Help),
        "-V" | "--version" | "version" => return Ok(Cli::Version),
        _ => {}
    }
    let mut json_out = false;
    let mut filter = None;
    let mut source: Option<Source> = None;
    let mut store: Option<StoreKind> = None;
    let mut connect: Option<String> = None;
    let mut cmds = Vec::new();
    let mut queries = Vec::new();
    let mut folder = None;
    let mut limit: usize = 200;
    let mut positional: Vec<PathBuf> = Vec::new();
    while let Some(arg) = a.items.next() {
        let (flag, inline) = split_flag(&arg);
        let allowed: &[&str] = match sub {
            "commands" => &["--json", "--filter"],
            "parity" => &["--json"],
            "run" => &["--sample", "--profile", "--store", "--cmd", "--query"],
            "show" => &["--sample", "--profile", "--store", "--folder", "--limit"],
            "import-eml" | "import-mbox" | "export-mbox" => &["--sample", "--profile", "--store", "--folder"],
            "mcp" => &["--sample", "--profile", "--store", "--connect"],
            other => return Err(format!("unknown command `{other}` (try --help)")),
        };
        if matches!(flag, "-h" | "--help") {
            return Ok(Cli::Help);
        }
        if flag.starts_with('-') && flag.len() > 1 && !allowed.contains(&flag) {
            return Err(format!("`{sub}` does not take `{flag}` (try --help)"));
        }
        match flag {
            "--json" | "--sample" if inline.is_some() => return Err(format!("`{flag}` takes no value")),
            "--json" => json_out = true,
            "--filter" => filter = Some(a.value(flag, inline)?),
            "--sample" => set_source(Source::Sample, &mut source)?,
            "--profile" => {
                let dir = a.value(flag, inline)?;
                if dir.trim().is_empty() {
                    return Err("`--profile` needs a folder".into());
                }
                set_source(Source::Profile(PathBuf::from(dir), None), &mut source)?;
            }
            "--store" => {
                let v = a.value(flag, inline)?;
                store = Some(
                    StoreKind::parse(&v)
                        .ok_or_else(|| format!("`--store` is legacy or sqlite, not `{v}`"))?,
                );
            }
            "--connect" => connect = Some(a.value(flag, inline)?),
            "--cmd" => cmds.push(parse_cmd(&a.value(flag, inline)?)?),
            "--query" => queries.push(Query::parse(&a.value(flag, inline)?)?),
            "--folder" => folder = Some(a.value(flag, inline)?),
            "--limit" => {
                let v = a.value(flag, inline)?;
                limit = v.parse().map_err(|_| format!("`--limit` must be a number, not `{v}`"))?;
            }
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    if let Some(kind) = store {
        match source.as_mut() {
            Some(Source::Profile(_, k)) => *k = Some(kind),
            _ => return Err("`--store` needs --profile DIR".into()),
        }
    }
    let no_positional = |positional: &[PathBuf]| match positional.first() {
        Some(p) => Err(format!("unexpected argument `{}` (try --help)", p.display())),
        None => Ok(()),
    };
    let explicit = |source: Option<Source>| {
        source.ok_or_else(|| {
            format!("`{sub}` needs --profile DIR (or --sample to try it on the sample mailbox)")
        })
    };
    match sub {
        "commands" => {
            no_positional(&positional)?;
            Ok(Cli::Commands { json: json_out, filter })
        }
        "parity" => {
            no_positional(&positional)?;
            Ok(Cli::Parity { json: json_out })
        }
        "run" => {
            no_positional(&positional)?;
            if cmds.is_empty() && queries.is_empty() {
                return Err("`run` needs at least one --cmd or --query".into());
            }
            Ok(Cli::Run { source: source.unwrap_or(Source::Sample), cmds, queries })
        }
        "show" => {
            no_positional(&positional)?;
            Ok(Cli::Show { source: source.unwrap_or(Source::Sample), folder, limit })
        }
        "import-eml" => {
            if positional.is_empty() {
                return Err("`import-eml` needs at least one .eml file".into());
            }
            Ok(Cli::ImportEml { source: explicit(source)?, folder, files: positional })
        }
        "import-mbox" | "export-mbox" => {
            let mut files = positional.into_iter();
            let file = files.next().ok_or_else(|| format!("`{sub}` needs one MBOX file"))?;
            no_positional(&files.collect::<Vec<_>>())?;
            let source = explicit(source)?;
            if sub == "import-mbox" {
                Ok(Cli::ImportMbox { source, folder, file })
            } else {
                let folder = folder.ok_or("`export-mbox` needs --folder NAME")?;
                Ok(Cli::ExportMbox { source, folder, file })
            }
        }
        "mcp" => {
            no_positional(&positional)?;
            let target = match (connect, source) {
                (Some(_), Some(_)) => return Err("give either --connect or --sample/--profile".into()),
                (Some(addr), None) => McpTarget::Connect(addr),
                (None, s) => McpTarget::Headless(s.unwrap_or(Source::Sample)),
            };
            Ok(Cli::Mcp { target })
        }
        _ => Err(format!("unknown command `{sub}` (try --help)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Cli, String> {
        parse(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn basics() {
        assert_eq!(p(&[]), Ok(Cli::Help));
        assert_eq!(p(&["--version"]), Ok(Cli::Version));
        assert_eq!(p(&["-V"]), Ok(Cli::Version));
        assert_eq!(p(&["commands"]), Ok(Cli::Commands { json: false, filter: None }));
        assert_eq!(
            p(&["commands", "--json", "--filter=reply"]),
            Ok(Cli::Commands { json: true, filter: Some("reply".into()) })
        );
        assert_eq!(p(&["parity", "--json"]), Ok(Cli::Parity { json: true }));
        assert_eq!(p(&["commands", "--help"]), Ok(Cli::Help));
    }

    #[test]
    fn run_commands() {
        let cli = p(&[
            "run",
            "--profile",
            "/tmp/x",
            "--cmd",
            "nav.selectAll",
            "--cmd",
            "mail.flag={\"ids\":[1]}",
            "--query",
            "status",
        ]);
        assert_eq!(
            cli,
            Ok(Cli::Run {
                source: Source::Profile("/tmp/x".into(), None),
                cmds: vec![("nav.selectAll".into(), json!({})), ("mail.flag".into(), json!({"ids": [1]}))],
                queries: vec![Query::Status],
            })
        );
        assert!(matches!(p(&["run", "--query", "list"]), Ok(Cli::Run { source: Source::Sample, .. })));
        assert!(p(&["run"]).is_err());
        assert!(p(&["run", "--cmd", "x={"]).unwrap_err().contains("not valid JSON"));
        assert!(p(&["run", "--cmd", "x=[1]"]).unwrap_err().contains("object"));
        assert!(p(&["run", "--cmd", "=1"]).is_err());
        assert!(p(&["run", "--cmd"]).unwrap_err().contains("needs a value"));
        assert!(p(&["run", "--query", "bogus"]).is_err());
        assert!(
            p(&["run", "--sample", "--profile", "d", "--query", "list"]).unwrap_err().contains("only one")
        );
    }

    #[test]
    fn files_and_mcp() {
        assert!(p(&["import-eml", "a.eml"]).unwrap_err().contains("--profile"));
        assert_eq!(
            p(&["import-eml", "--sample", "a.eml", "b.eml"]),
            Ok(Cli::ImportEml {
                source: Source::Sample,
                folder: None,
                files: vec!["a.eml".into(), "b.eml".into()]
            })
        );
        assert!(p(&["import-mbox", "--sample", "a", "b"]).is_err());
        assert!(p(&["export-mbox", "--sample", "out.mbox"]).unwrap_err().contains("--folder"));
        assert_eq!(p(&["mcp"]), Ok(Cli::Mcp { target: McpTarget::Headless(Source::Sample) }));
        assert_eq!(
            p(&["mcp", "--connect", "127.0.0.1:7982"]),
            Ok(Cli::Mcp { target: McpTarget::Connect("127.0.0.1:7982".into()) })
        );
        assert!(p(&["mcp", "--connect", "x:1", "--sample"]).is_err());
        assert!(p(&["mcp", "--folder", "x"]).unwrap_err().contains("does not take"));
        assert!(p(&["frobnicate"]).unwrap_err().contains("unknown command"));
        assert!(p(&["show", "--limit", "many"]).is_err());
        assert!(p(&["show", "extra"]).unwrap_err().contains("unexpected"));
    }

    #[test]
    fn store_kind_for_a_new_profile() {
        assert_eq!(
            p(&["show", "--store", "sqlite", "--profile", "d"]),
            Ok(Cli::Show {
                source: Source::Profile("d".into(), Some(StoreKind::Sqlite)),
                folder: None,
                limit: 200
            })
        );
        assert_eq!(
            p(&["mcp", "--profile=d", "--store=legacy"]),
            Ok(Cli::Mcp {
                target: McpTarget::Headless(Source::Profile("d".into(), Some(StoreKind::Legacy)))
            })
        );
        assert!(p(&["show", "--store", "sqlite"]).unwrap_err().contains("--profile"));
        assert!(p(&["show", "--sample", "--store", "sqlite"]).unwrap_err().contains("--profile"));
        assert!(p(&["show", "--profile", "d", "--store", "lmdb"]).unwrap_err().contains("legacy or sqlite"));
        assert!(p(&["commands", "--store", "sqlite"]).unwrap_err().contains("does not take"));
    }
}
