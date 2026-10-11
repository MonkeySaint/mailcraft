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
| M1 | Protocols: IMAP sync, SMTP send, Outbox/Sent flow, OAuth2, account setup; MSG/EML/MBOX | ✅ done (commit 8) |
| M3/M6/M12 | Calendar (iCalendar), contacts (vCard), tasks, notes, rules engine + Rules Wizard, quick steps, search folders, PDF/print, import/export | ✅ done (commit 9) |
| — | Clippy/fmt clean, honest parity table, CI green | ✅ done (commit 10) |
| — | README with screenshots | ✅ done (commit 11) |
| — | Release pipeline on GitHub Actions (Linux AppImage/deb/rpm/tar/Flatpak, Windows MSI, macOS DMG, FreeBSD, web) | ✅ done (commits 12–13) |
| — | Gap closing: profile settings store and Options; People/Tasks/Notes views and actions; calendar depth (colours, overlay, drag/resize, named time zones, subscriptions); compose (rich paste, themes, list formats, voting, receipts, Save Sent Item To); Junk Email; accounts (IDLE push, POP3, JMAP, OAuth sign-in, OS keyring) | ✅ round 1 done (commit 14); re-audit next |
| — | Fidelity: responsive single-pane, reading pane Bottom columns, row icons/hover actions, read-on-selection-change + inline reply | planned (commits 14–17) |
| M7/M14 | Rules UI depth, alpha/parity estimates, small fixes | planned (commits 18–26) |
| v0.1.0 / v0.2.0 / v0.3.0 | Local release tags | commits 23 / 27 / 30 |
| v1.0.0 | Definition of done: full catalog coverage, ≥ 95 % honest parity in every area, end-to-end against a local IMAP/SMTP server | goal |

## Parity

Two numbers, kept separate on purpose:

- **Catalog coverage** (mechanical, `cargo xtask parity` → [docs/parity.md](docs/parity.md)): every observed
  Outlook command row maps to a MailCraft command id that exists in the registry. Today 1,518 of 1,518 applicable
  rows (100 %); 125 rows are not applicable (cloud-only Microsoft services). This only proves an id exists.
- **Honest parity** (audited 2026-10-10, commit 10): each of the 1,518 rows classified by what its command really
  does: REAL (works and is reachable), DIALOG-OK (opens a dialog that works), DIALOG-STUB (opens a dialog that
  doesn't do the thing), PARTIAL (works, but clearly less than observed), STUB (no-op, echo or error).
  Honest % = (REAL + DIALOG-OK + ½ PARTIAL) / rows. **Overall: 61.9 %** (55.0 % counting only REAL + DIALOG-OK).

| Catalog area | Rows | REAL | DIALOG-OK | DIALOG-STUB | PARTIAL | STUB | Honest |
|---|---:|---:|---:|---:|---:|---:|---:|
| Mail (main window) | 262 | 189 | 14 | 16 | 25 | 18 | 82 % |
| Context menus | 205 | 149 | 20 | 10 | 14 | 12 | 86 % |
| Read window | 85 | 63 | 7 | 5 | 4 | 6 | 85 % |
| Search | 40 | 29 | 3 | 0 | 2 | 6 | 83 % |
| Compose | 448 | 223 | 28 | 21 | 90 | 86 | 66 % |
| Calendar | 128 | 41 | 8 | 9 | 21 | 49 | 47 % |
| People | 142 | 34 | 3 | 12 | 25 | 68 | 35 % |
| Notes | 64 | 8 | 2 | 10 | 17 | 27 | 29 % |
| Tasks | 144 | 12 | 2 | 9 | 12 | 109 | 14 % |
| **Total** | **1,518** | **748** | **87** | **92** | **210** | **381** | **61.9 %** |

### By functional area (judgement, with evidence)

| Area | Honest | Evidence |
|---|---|---|
| Message list and folders | ~75 % | grouping, Focused/Other, drafts, preview lines, arrange/sort/filter, folder CRUD and favourites work; saved views, columns, conditional formatting, Quick Actions, AutoArchive, Open in New Window don't |
| Reading pane | ~75 % | header, info bars, blocked pictures, inline reply, read-on-selection-change; Reading Pane options inert, no Space-bar reading |
| Compose | ~65 % | editing, styles, tables, attachments, signatures, receipts, delay delivery; themes, style sets, list formats, encryption, voting, Save Sent Item To, rich paste missing |
| Ribbon, menus, shortcuts | ~80 % | data-driven ribbons with width rules and keytips; they also surface the stubs below |
| Rules and quick steps | ~80 % | 31 conditions, 25 actions, wizard and manage dialogs, rules on new mail, Quick Steps all real |
| Search | ~80 % | indexed query language, refiners, scopes, Advanced Find, search folders; "Locations to Search" and Expiring Soon inert |
| Accounts and sync | ~45 % | IMAP (CONDSTORE/QRESYNC), SMTP, Outbox, retries, offline work end-to-end tested; no JMAP send/receive, Exchange, POP3, IDLE push, OAuth sign-in UI; secrets file unencrypted |
| Calendar | ~50 % | five views, recurrence, reminders, iTIP round trip, ICS; no colours, overlay, subscriptions, drag/resize, named time zones |
| People | ~40 % | editor, groups, vCard/CSV; module views other than Card, arrange, Mail Merge missing |
| Tasks | ~25 % | editor and To-Do list; no recurrence, task requests, views, module actions |
| Notes | ~40 % | create/edit/colour; ribbon views dropped |
| Import/export and print | ~65 % | EML/MSG/MBOX/Maildir/ICS/VCF/CSV, PDF everywhere, CUPS; no PST, no Windows printing |
| Options dialog | ~4 % | ~123 of 207 observed settings present but only 9 take effect and none persist |
| Responsive behaviour | ~90 % | 26 of 30 observed behaviours, exact break points |
| Keyboard | ~93 % | 38 of 41 tested keys |

### Biggest gaps (ranked by user impact)

1. Options: settings don't take effect or persist (needs a profile settings store; one key mismatch for auto-complete).
2. New Appointment/Contact/Task from the Mail module fail (`mail.newItem` passes a mail folder).
3. View commands outside Mail (`view.changeView`, arrange, reading pane, To-Do Bar, view settings) error or do nothing.
4. Accounts: JMAP send/receive, OAuth sign-in, IDLE push, POP3, Exchange (EWS), encrypted secrets.
5. Calendar subscriptions and CalDAV/CardDAV; calendar colours, overlay, drag and resize; named time zones with DST.
6. Compose: rich paste, S/MIME encryption, voting buttons, Save Sent Item To, themes and style sets, list formats.
7. Junk: blocked/safe sender lists and junk levels.
8. People/Tasks/Notes module actions routed to mail-only commands; task recurrence and requests.
9. Reading Pane options (mark as read after N seconds), Space-bar reading.
10. Saved views and columns; Quick Actions; status bar customisation; AutoArchive.

The full audit (per-row classification) is regenerated before each release; commits 14–26 and the milestones
after v0.3.0 work down this list until every area is ≥ 95 %.

## Needs the owner

These need you and do not block other work:

- **Code-signing secrets** for Windows (MSI) and macOS (DMG), when releases should be signed.
- **GitHub runners** (not enabled yet on github.com): CI and release workflows are committed under
  `.github/workflows/` and stay dormant until runners exist; labels are documented in docs/releasing.md
  (Linux docker, plus self-hosted Windows/macOS/FreeBSD if you want those artifacts).

Not needed from you: icon art (all icons are original SVGs made for this project).
