# MailCraft roadmap

MailCraft is a clean-room, open-source desktop mail client in Rust, modelled on the classic Outlook
workflow. This file tracks what is done, what is next, and an honest estimate of how close each area is to
Outlook. It is updated with every commit.

Status: **pre-alpha** (2026-10-29). Repository: https://github.com/MonkeySaint/mailcraft. Tags: v0.1.0 (commit 23,
unpublished: its release run failed), v0.1.1 (commit 26, first GitHub release), v0.2.0 (after round 5), v0.3.0 (after round 7), v0.4.0 (storage redesign S0-S6), v0.5.0 (Microsoft Graph, Exchange EWS, PST import), v0.5.1 (Windows account setup and sign-in fixes).

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
| — | Gap closing: profile settings store and Options; People/Tasks/Notes views and actions; calendar depth (colours, overlay, drag/resize, named time zones, subscriptions); compose (rich paste, themes, list formats, voting, receipts, Save Sent Item To); Junk Email; accounts (IDLE push, POP3, JMAP, OAuth sign-in, OS keyring) | ✅ round 1 done (commit 14), re-audited at 87.1 % |
| — | Gap closing round 2: task requests, contact reminders, calendar settings and meeting form, Options wiring, saved views and columns, compose panes and find in received mail, autodiscovery/OAuth/JMAP push, keyboard focus, New Items | ✅ done (commits 15–22) |
| — | Round 3: new-mail pipeline (junk, Focused overrides, rules, meeting auto-respond, task and receipt processing), contact reminders, saved views in Change View, theme colours, picture paste, vCard photos, compose auto-save; move to GitHub Actions, app id `saint.dev.MailCraft`, new app icon | ✅ done (commit 23, tag v0.1.0) |
| — | Release fixes (desktop entry, web wasm-opt, Flatpak on trixie, CI cache); first published release | ✅ done (commits 24–26, tag v0.1.1) |
| — | Round 4: AutoArchive, S/MIME, compose and reading options take effect, note positions | ✅ done |
| — | Round 5: data safety (data files kept on Remove, Close All Items prompts), second main window, Focused Inbox on arrival, Ignore, Font and Paragraph dialogs, building blocks, screenshots, equations, thesaurus, People/Tasks/Notes/Calendar table views, Help | ✅ done, re-audited at 95.7 % |
| — | Round 6: compose drawing objects, paragraph borders, Find and Replace options, proofing and the Editor pane, To-Do List actions, in-place Edit Message, recoverable items, quota, Send/Receive groups, Mail Merge documents, CardDAV/CalDAV contact and task sync, calendar publishing, shared mailboxes | ✅ done, re-audited at 97.1 % |
| — | Round 7: installed fonts and system dictionaries, Apply Styles pane, Office themes, multi-object drawing (group, align, distribute, text wrap, Edit Points, linked text boxes), Format Shape, appointment window depth, CalDAV event sync, View Settings depth in every module, standalone data files, quick step options, menu access keys, server search, opt-in translation | ✅ done, re-audited at 99.1 % |
| — | Round 8: release check (second-opinion audit, app walk-through, security review of the network code), accessibility pane, building blocks with pictures, list and paragraph dialog depth, linked styles | next |
| v0.1.0 / v0.2.0 / v0.3.0 | Local release tags | commit 23 / after round 5 / after round 7 |
| v1.0.0 | Definition of done: full catalog coverage, ≥ 95 % honest parity in every area, end-to-end against a local IMAP/SMTP server | goal |

## Parity

Two numbers, kept separate on purpose:

- **Catalog coverage** (mechanical, `cargo xtask parity` → [docs/parity.md](docs/parity.md)): every observed
  Outlook command row maps to a MailCraft command id that exists in the registry. Today 1,565 of 1,565 applicable
  rows (100 %); 133 rows are not applicable (cloud-only Microsoft services). This only proves an id exists.
- **Honest parity** (re-audited 2026-10-21 after round 7): each of the 1,565 audited rows classified by what its
  command really does: REAL (works and is reachable), DIALOG-OK (opens a dialog that works), DIALOG-STUB (opens a
  dialog that doesn't do the thing), PARTIAL (works, but clearly less than observed), STUB (no-op, echo or
  error). Honest % = (REAL + DIALOG-OK + ½ PARTIAL) / rows. **Overall: 99.1 %** (98.3 % counting only REAL +
  DIALOG-OK), up from 97.1 % (94.8 %) after round 6 with the same method. Every area is above 95 % on both
  numbers; the 27 PARTIAL rows left are listed under Biggest gaps.

| Catalog area | Rows | REAL | DIALOG-OK | DIALOG-STUB | PARTIAL | STUB | Honest |
|---|---:|---:|---:|---:|---:|---:|---:|
| Mail (main window) | 262 | 200 | 60 | 0 | 2 | 0 | 99.6 % |
| Context menus | 204 | 162 | 40 | 0 | 2 | 0 | 99.5 % |
| Read window | 84 | 59 | 23 | 0 | 2 | 0 | 98.8 % |
| Search | 40 | 36 | 4 | 0 | 0 | 0 | 100 % |
| Compose | 503 | 403 | 80 | 0 | 20 | 0 | 98.0 % |
| Calendar | 122 | 91 | 30 | 0 | 1 | 0 | 99.6 % |
| People | 142 | 104 | 38 | 0 | 0 | 0 | 100 % |
| Notes | 64 | 50 | 14 | 0 | 0 | 0 | 100 % |
| Tasks | 144 | 119 | 25 | 0 | 0 | 0 | 100 % |
| **Total** | **1,565** | **1,224** | **314** | **0** | **27** | **0** | **99.1 %** |

### By functional area (judgement, with evidence)

| Area | Honest | Evidence |
|---|---|---|
| Message list and folders | ~80 % | grouping, Focused/Other, arrange/sort/filter, folder CRUD, favourites, Bottom-pane columns, hover actions; saved views, Add Columns, conditional formatting, Quick Actions settings missing; AutoArchive (scheduled and manual, per-folder settings) |
| Reading pane | ~90 % | header, info bars, inline reply, mark-as-read timing, Space-bar reading, find in a received message (Find…, F4), preview and portrait reading options, read-window Next / Previous links |
| Compose | ~97 % | editing, styles, themes and Office theme files, list formats, rich paste, voting, receipts, S/MIME, accessibility checker, @mentions, system clipboard, Format Painter, Font and Paragraph dialogs, building blocks, screenshots, equations, thesaurus, Read Aloud, drawing objects (multi-select, groups, wrapping, Format Shape), Borders and Shading, Find and Replace options, bookmarks, Editor pane, installed fonts, system dictionaries; styles are direct formatting, drop caps inline |
| Ribbon, menus, shortcuts | ~88 % | data-driven ribbons with width rules and keytips; a few Calendar buttons route through Mail commands |
| Rules and quick steps | ~80 % | 31 conditions, 25 actions, wizard and manage dialogs, Quick Steps |
| Search | ~92 % | indexed query language, refiners, Advanced Find with more criteria, search folders, Include Older Results on the server (IMAP, JMAP), Deleted Items scope |
| Accounts and sync | ~85 % | IMAP (IDLE, CONDSTORE/QRESYNC, QUOTA, NAMESPACE and shared mailboxes), POP3, JMAP, SMTP, CardDAV/CalDAV for contacts and tasks, Send/Receive groups, OAuth sign-in, OS keyring; no OAuth end-to-end test |
| Calendar | ~96 % | views, colours, overlay, drag/resize, named time zones, subscriptions, iTIP, Calendar Properties, WebDAV publishing, CalDAV event sync, per-window calendars, appointment window with time zones, formatted notes, attachments and Resources; saved views missing from Change View |
| People | ~97 % | editor, groups, table and card views with sort, filter and conditional formatting, saved views, vCard/CSV, Mail Merge (e-mail, letters, labels, envelopes), CardDAV sync |
| Tasks | ~98 % | editor, recurrence, task requests, table views with column sets and saved views, Tasks ribbon on flagged messages and contacts, CalDAV task sync, flagged mail previews in the To-Do List |
| Notes | ~100 % | create/edit/colour, icon and table views, saved views, positions kept on the note |
| Import/export and print | ~75 % | EML/MSG/MBOX/Maildir/ICS/VCF/CSV, PST import and Open Data File (read only; export goes to MailCraft data files, EML, MBOX, ICS and VCF; attached messages inside PST files not yet), PDF everywhere, CUPS; no Windows printing |
| Options dialog | ~90 % | every setting persists in the profile; 178 of 228 take effect, 29 are not applicable, 21 are shown or stored only (`settings::effect_counts`) |
| Responsive behaviour | ~97 % | 29 of 30 observed behaviours (Classic Respond group at 1024 px) |
| Keyboard | ~99 % | Space on a focused button; F6 cycles compose and read windows |

### Biggest gaps (ranked by user impact, from the 2026-10-21 audit after round 7)

1. Compose: Check Accessibility is a one-shot list, not a pane that keeps checking with fix actions; building
   blocks lose their pictures in other messages and after a restart.
2. Compose structure: built-in styles are direct formatting rather than linked styles; Paragraph has no Tabs
   dialog; list definitions have no position settings; drop caps stay inline.
3. Compose drawing and pictures: 10 SmartArt layouts, 67 icons, single-colour 3D models, no arrowheads or
   picture/gradient text fills.
4. Proofing and reading aids: one proofing language per message; Read Aloud and the Thesaurus are dialogs, not
   panes; Translate needs a server the user sets up.
5. Mail: Post in This Folder is plain text; Quick Print opens the Print page on Windows; voting buttons only
   work between MailCraft users.

The full audit (per-row classification) is regenerated before each release. Every area is now at 95 % or more;
the rounds after v0.3.0 work down this list and check the audit with a second, skeptical pass before v1.0.0.

## Needs the owner

These need you and do not block other work:

- **Code-signing secrets** (optional) for Windows (MSI) and macOS (DMG), when releases should be signed:
  `WINDOWS_CERT_PFX`/`WINDOWS_CERT_PASSWORD` and the `APPLE_*` secrets (docs/releasing.md).

Not needed from you: icon art (all icons are original SVGs made for this project).
