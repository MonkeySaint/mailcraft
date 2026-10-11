# MailCraft: instructions for agents

MailCraft is a clean-room, open-source, Rust-native desktop mail client modelled on the classic Outlook
workflow: module rail, ribbon, folder pane, message list, reading pane, compose window, rules, quick steps,
calendar, people, tasks and notes. It runs natively on Linux, Windows, macOS and FreeBSD, and on the web via
WASM.

## Start every session here
1. Read `plan/STATUS.md` (current commit, what's next), then `plan/execution-plan.md` and
   `plan/architecture.md`. `plan/` is gitignored (local only); if it's missing, start from `ROADMAP.md` and
   `docs/parity.md`.
2. Work autonomously. Stop to ask only for decisions that are genuinely the owner's (licensing, publishing,
   pushing to remotes, secrets, real mail accounts).

## Never crash
People trust a mail client with their mail; a crash can lose a draft. This outranks feature work.
- No panics in non-test code: no `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`,
  `unimplemented!`; no `unsafe` (`unsafe_code = "forbid"`). Workspace lints deny them; tests may unwrap
  (`clippy.toml`).
- Errors are `Result` through each crate's error type. Unfinished features return an error or are disabled.
- Input-derived data is hostile (messages, mailboxes, server responses, command params): `get()` instead of
  `[i]`, char-boundary slicing, checked/saturating arithmetic, size caps, bounded recursion.
- `Session::run` turns an escaped panic into an error and restores the mailbox. It is a safety net only.
- Every crash fix lands with a regression test. `crates/engine/tests/fuzz.rs` runs every command with junk
  params; keep it green.

## Assets: no Microsoft artwork, ever
- Never copy, trace, crop or recreate icons, images, templates, sounds or fonts from Outlook/Office or any
  Microsoft product, not even temporarily.
- Icons are original SVGs drawn for this project (`assets/icons/`, `assets/app-icon/`). Fonts are OFL
  (Inter, Liberation, Carlito). Every asset file has a row in `ATTRIBUTION.md`; `cargo xtask assets` checks.
- Proprietary fonts named in mail (Calibri, Aptos, Segoe UI, Arial…) map to bundled open substitutes
  (`crates/fonts`).

## Clean room
- Outlook was observed black-box only; the notes live in `plan/reference/` (local). Behaviour and feature names
  come from there. Never read or copy anything from an Outlook install, never copy Microsoft wording beyond
  feature names.
- Formats and protocols come from public specs: RFC 5322/2045–2049/2047/2231 (mail), RFC 3501/9051 (IMAP),
  RFC 5321/6409 (SMTP), RFC 8620/8621 (JMAP), RFC 5545 (iCalendar), RFC 6350 (vCard), Microsoft Open
  Specifications ([MS-OXMSG], [MS-OXWS]…). Never copy GPL/LGPL/AGPL/MPL code (Thunderbird, Evolution,
  Geary, mutt…).
- Never test with real mail accounts. Use the local test servers and synthetic mail
  (`mailcraft-engine::sample`, example.com/.org/.net addresses).

## Architecture (`plan/architecture.md`)
| Layer | Crate | Job |
|---|---|---|
| L0 | `geom`, `mime` | units/colours; RFC 5322 + MIME parse/build |
| L1 | `model`, `fonts` | mailbox model, rich text, dates; bundled fonts |
| L2 | `store`, `html`, `layout` | local storage; HTML sanitise ↔ rich text; body layout |
| L3 | `render`, `net`, `formats`, `proof` (+ `search` later) | offscreen raster; protocols; file formats; spelling; search |
| L4 | `engine` | Session, undo, **command registry**, Outlook feature catalog, sample mailbox |
| L5 | `mcp` | MCP server |
| L6 | `ui-egui` | Outlook-style egui front end |
| apps | `apps/mailcraft`, `apps/mailcraft-cli`, `apps/mailcraft-web` | binaries |
| test support | `testmail` | local IMAP/SMTP test servers (127.0.0.1); dev-dependencies and apps only |

- **Everything is a command.** User-visible behaviour is a `CommandSpec` in `crates/engine/src/cmd/*.rs`.
  Ribbon, shortcuts, menus, CLI, control channel and MCP all dispatch by id.
- **Parity is measured:** `crates/engine/catalog/*.txt` maps every observed Outlook command to an id;
  `cargo xtask parity` writes `docs/parity.md`.
- **Layering** is enforced by `cargo xtask layers`: nothing below L6 depends on egui/eframe.

## Quality gates
Before every commit: `cargo xtask ci` (fmt, clippy -D warnings, tests, layers, assets, licences, parity).
CI runs on GitHub Actions (`.github/workflows/`).

## Running and looking at the app
- `cargo run --release -p mailcraft -- --sample --control-port 7982` (sample mailbox + control channel; see
  `docs/control-protocol.md`).
- Headless screenshots: `cargo run -p mailcraft-ui-egui --example ui_shot -- …` (see the example header).
- CLI and MCP: `mailcraft-cli commands`, `mailcraft-cli parity`, `mailcraft-cli mcp` (see `docs/mcp.md`).
- **Never use /tmp** on the dev box: it is a RAM tmpfs and filling it crashed the machine. Scratch files go
  under `target/scratch/`; run cargo with `TMPDIR=$PWD/target/tmp` (create it first) so test temp files land
  on disk.
- Parallel agents: give each agent its own `CARGO_TARGET_DIR`, cap `CARGO_BUILD_JOBS` (the dev box ran out
  of memory with many parallel builds), edit only the paths you own, delete your target dir when done.
