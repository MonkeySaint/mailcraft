# MailCraft roadmap

MailCraft is a clean-room, open-source desktop mail client in Rust, modelled on the classic Outlook
workflow. This file tracks what is done, what is next, and an honest estimate of how close each area is to
Outlook. It is updated with every commit.

Status: **pre-alpha** (2026-10-10). Repository: git@github.com:MonkeySaint/mailcraft.git. Nothing is released yet; the first local tag (v0.1.0) is planned at
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
| — | Gap closing: profile settings store and Options; People/Tasks/Notes views and actions; calendar depth (colours, overlay, drag/resize, named time zones, subscriptions); compose (rich paste, themes, list formats, voting, receipts, Save Sent Item To); Junk Email; accounts (IDLE push, POP3, JMAP, OAuth sign-in, OS keyring) | ✅ round 1 done (commit 14), re-audited at 87.1 %; round 2 in progress |
| — | Gap closing round 2: Options wiring, calendar settings, saved views and columns, task requests, compose panes, autodiscovery and JMAP push, keyboard focus, new items | in progress (commits 15–22) |
| M7/M14 | Rules UI depth, alpha/parity estimates, small fixes | planned (commits 23–26) |
| v0.1.0 / v0.2.0 / v0.3.0 | Local release tags | commits 23 / 27 / 30 |
| v1.0.0 | Definition of done: full catalog coverage, ≥ 95 % honest parity in every area, end-to-end against a local IMAP/SMTP server | goal |

## Parity

Two numbers, kept separate on purpose:

- **Catalog coverage** (mechanical, `cargo xtask parity` → [docs/parity.md](docs/parity.md)): every observed
  Outlook command row maps to a MailCraft command id that exists in the registry. Today 1,512 of 1,512 applicable
  rows (100 %); 131 rows are not applicable (cloud-only Microsoft services). This only proves an id exists.
- **Honest parity** (re-audited 2026-10-10 at commit 14): each of the 1,512 rows classified by what its command
  really does: REAL (works and is reachable), DIALOG-OK (opens a dialog that works), DIALOG-STUB (opens a dialog
  that doesn't do the thing), PARTIAL (works, but clearly less than observed), STUB (no-op, echo or error).
  Honest % = (REAL + DIALOG-OK + ½ PARTIAL) / rows. **Overall: 87.1 %** (80.2 % counting only REAL + DIALOG-OK),
  up from 61.9 % at commit 10. Six Exchange-only calendar rows moved to not applicable and 76 rows were re-mapped
  to the command that now implements them.

| Catalog area | Rows | REAL | DIALOG-OK | DIALOG-STUB | PARTIAL | STUB | Honest |
|---|---:|---:|---:|---:|---:|---:|---:|
| Mail (main window) | 262 | 200 | 23 | 7 | 28 | 4 | 90.5 % |
| Context menus | 205 | 156 | 23 | 7 | 7 | 12 | 89.0 % |
| Read window | 85 | 65 | 7 | 5 | 2 | 6 | 85.9 % |
| Search | 40 | 29 | 3 | 0 | 2 | 6 | 82.5 % |
| Compose | 448 | 286 | 40 | 9 | 88 | 25 | 82.6 % |
| Calendar | 122 | 78 | 13 | 3 | 25 | 3 | 84.8 % |
| People | 142 | 99 | 14 | 3 | 25 | 1 | 88.4 % |
| Notes | 64 | 44 | 8 | 0 | 12 | 0 | 90.6 % |
| Tasks | 144 | 116 | 9 | 1 | 18 | 0 | 93.1 % |
| **Total** | **1,512** | **1,073** | **140** | **35** | **207** | **57** | **87.1 %** |

### By functional area (judgement, with evidence)

| Area | Honest | Evidence |
|---|---|---|
| Message list and folders | ~80 % | grouping, Focused/Other, arrange/sort/filter, folder CRUD, favourites, Bottom-pane columns, hover actions; saved views, Add Columns, conditional formatting, Quick Actions settings, AutoArchive missing |
| Reading pane | ~85 % | header, info bars, inline reply, mark-as-read timing, Space-bar reading; no find in a received message |
| Compose | ~80 % | editing, styles, themes, list formats, rich paste, voting, receipts, Save Sent Item To; Clipboard/Styles/Selection panes, ¶ marks, S/MIME, bitmap paste missing |
| Ribbon, menus, shortcuts | ~88 % | data-driven ribbons with width rules and keytips; a few Calendar buttons route through Mail commands |
| Rules and quick steps | ~80 % | 31 conditions, 25 actions, wizard and manage dialogs, Quick Steps |
| Search | ~82 % | indexed query language, refiners, Advanced Find, search folders; scopes and Expiring Soon partial |
| Accounts and sync | ~70 % | IMAP (IDLE, CONDSTORE/QRESYNC), POP3, JMAP, SMTP, OAuth sign-in, OS keyring; no autodiscovery, JMAP push, OAuth end-to-end test |
| Calendar | ~80 % | views, colours, overlay, drag/resize, named time zones, subscriptions, iTIP; calendar settings inert, New Meeting saves before the form |
| People | ~75 % | editor, groups, views, vCard/CSV, Mail Merge (e-mail); contact reminders, Details/All Fields pages missing |
| Tasks | ~70 % | editor, recurrence, views, module actions; incoming task requests not processed |
| Notes | ~85 % | create/edit/colour/views; positions not persisted |
| Import/export and print | ~65 % | EML/MSG/MBOX/Maildir/ICS/VCF/CSV, PDF everywhere, CUPS; no PST, no Windows printing |
| Options dialog | ~22 % | all 179 observed settings persist in the profile; 40 take effect |
| Responsive behaviour | ~97 % | 29 of 30 observed behaviours (Classic Respond group at 1024 px) |
| Keyboard | ~99 % | Ctrl+< / Ctrl+> missing in compose; Space on a focused button |

### Biggest gaps (ranked by user impact)

1. Options: Mail and Calendar settings that persist but don't take effect yet.
2. New Meeting saves the item before its form opens (an untitled meeting is left on cancel).
3. Saved views, Add Columns, conditional formatting, status bar customisation, Quick Actions settings.
4. Incoming task requests.
5. Accounts: autodiscovery, JMAP push, configurable first-sync window, OAuth end-to-end test.
6. Compose panes (Clipboard, Styles, Selection), formatting marks, S/MIME, bitmap paste.
7. Find in a received message; F6 focus cycling; Space on a focused button.
8. New Items depth, Reply with Meeting, Post in This Folder, folder properties, AutoArchive.

The full audit (per-row classification) is regenerated before each release; the commits after 14 and the
milestones after v0.3.0 work down this list until every area is ≥ 95 %.

## Needs the owner

These need you and do not block other work:

- **Code-signing secrets** for Windows (MSI) and macOS (DMG), when releases should be signed.
- **GitHub runners** (not enabled yet on github.com): CI and release workflows are committed under
  `.github/workflows/` and stay dormant until runners exist; labels are documented in docs/releasing.md
  (Linux docker, plus self-hosted Windows/macOS/FreeBSD if you want those artifacts).

Not needed from you: icon art (all icons are original SVGs made for this project).
