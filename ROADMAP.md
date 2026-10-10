# MailCraft roadmap

MailCraft is a clean-room, open-source desktop mail client in Rust, modelled on the classic Outlook
workflow. This file tracks what is done, what is next, and an honest estimate of how close each area is to
Outlook. It is updated with every commit.

Status: **pre-alpha** (2026-10-09). Repository: git@github.com:MonkeySaint/mailcraft.git. Nothing is released yet; the first local tag (v0.1.0) is planned at
commit 23.

## Milestones

| Milestone | What | Status |
|---|---|---|
| M0.1–M0.6 | Workspace and foundation crates: geom, mime (RFC 5322/MIME), model, fonts, store, html, layout, render | ✅ done (commit 2) |
| M0.7 | Engine: session, undo/redo, command registry (370 commands), Outlook feature catalog (1,643 rows), sample mailbox, fuzzing | ✅ done (commit 3) |
| M0.8 | Outlook-style egui UI: title bar + search, module rail, ribbon (Simplified + Classic), folder pane, message list, reading pane, compose window, backstage, options, dialogs, calendar/people/tasks/notes views, original icon set; desktop app | ✅ done (commit 4) |
| M0.9–M0.12 | CLI, MCP server, `cargo xtask` (ci, layers, assets, parity, stats, version, licences, ico), app icon, AGENTS/ATTRIBUTION/NOTICE, docs/mcp.md | ✅ done (commit 5) |
| M4.1 | Compose depth: rich-text editing, spelling, signatures, attachments, address auto-complete; web app | ✅ done (commit 6) |
| M5.1 | Conversation view in the list (header + child rows) | ✅ done (commit 7) |
| M1 | Protocols: IMAP sync, SMTP send, Outbox/Sent flow, OAuth2, account setup; MSG/EML/MBOX | planned (commit 8) |
| M3/M6/M12 | Calendar (iCalendar), contacts (vCard), tasks, notes, rules engine + Rules Wizard, quick steps, search folders, PDF/print, import/export | planned (commit 9) |
| — | Clippy/fmt clean, honest parity table, CI green | planned (commit 10) |
| — | README with screenshots | planned (commit 11) |
| — | Release pipeline on GitHub Actions (Linux AppImage/deb/rpm/tar/Flatpak, Windows MSI, macOS DMG, FreeBSD, web) | planned (commits 12–13) |
| — | Fidelity: responsive single-pane, reading pane Bottom columns, row icons/hover actions, read-on-selection-change + inline reply | planned (commits 14–17) |
| M7/M14 | Rules UI depth, alpha/parity estimates, small fixes | planned (commits 18–26) |
| v0.1.0 / v0.2.0 / v0.3.0 | Local release tags | commits 23 / 27 / 30 |
| v1.0.0 | Definition of done: full catalog coverage, ≥ 95 % honest parity in every area, end-to-end against a local IMAP/SMTP server | goal |

## Parity

Two numbers, kept separate on purpose:

- **Catalog coverage** (mechanical): every observed Outlook command row is mapped to a MailCraft command id
  that exists in the registry. Today: **1,518 of 1,518 applicable rows (100 %)**; 125 rows are not applicable
  (cloud-only Microsoft services such as Teams, Copilot, OneNote, Loop, Viva, Microsoft 365 Groups).
- **Honest parity** (judgement): how much of the real behaviour works for a user. A command that only opens a
  placeholder dialog, or works in the engine but has no UI yet, does not count. Estimated per area:

| Area | Honest parity | Notes |
|---|---|---|
| Message parsing / building (MIME) | ~85 % | RFC 5322/2045–2049/2047/2231, charsets, multipart, builder; no S/MIME, no TNEF |
| Local storage | ~70 % | snapshot + journal, blobs, Maildir/MBOX import, MBOX export; no PST |
| Mail commands (engine) | ~60 % | reply/forward format as observed, delete/move/flag/categorize/read state, drafts; no sending yet |
| Message list and folders (UI) | ~55 % | folder pane with badges/favourites, compact rows, date groups, unread/flag styling, Focused/Other, arrange menu, Bottom-pane columns; drag-and-drop and conversations later |
| Reading pane / HTML display | ~50 % | header, persona, info bars, attachment chips, body rendering, read windows; remote-image prompt basic |
| Compose | ~60 % | rich-text editing, spelling with suggestions, signatures, address auto-complete, Check Names, attachments; web build |
| Ribbon / menus / shortcuts | ~50 % | Simplified + Classic ribbons for every tab with width degradation, context menus, shortcuts, keytips; many dialogs still basic |
| Rules and quick steps | ~35 % | model + engine (conditions, actions, run now); dialogs in commit 4, wizard depth later |
| Search | ~20 % | simple query prefixes; indexed search in commit 9 |
| Calendar / People / Tasks / Notes | ~30 % | day/week/month/schedule views, contact cards, task list, notes grid; iCalendar/vCard in commit 9 |
| Accounts / sync (IMAP, SMTP, JMAP) | ~25 % | protocol clients + local test servers done; account wiring, sync and sending in commit 8 |
| Packaging / releases | 0 % | commits 12–13 |

## Needs the owner

These need you and do not block other work:

- **Code-signing secrets** for Windows (MSI) and macOS (DMG), when releases should be signed.
- **GitHub runners** (not enabled yet on github.com): CI and release workflows are committed under
  `.github/workflows/` and stay dormant until runners exist; labels are documented in docs/releasing.md
  (Linux docker, plus self-hosted Windows/macOS/FreeBSD if you want those artifacts).

Not needed from you: icon art (all icons are original SVGs made for this project).
