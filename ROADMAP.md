# MailCraft roadmap

MailCraft is a clean-room, open-source desktop mail client in Rust, modelled on the classic Outlook
workflow. This file tracks what is done, what is next, and an honest estimate of how close each area is to
Outlook. It is updated with every commit.

Status: **pre-alpha** (2026-10-21). Repository: https://github.com/MonkeySaint/mailcraft. Tags: v0.1.0 (commit 23,
unpublished: its release run failed), v0.1.1 (commit 26, first GitHub release), v0.2.0 (after round 5).

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
| — | Round 7: multi-object drawing (group, align, distribute, text wrap), appointment window depth, CalDAV calendar sync, standalone data files, View Settings depth | next |
| v0.1.0 / v0.2.0 / v0.3.0 | Local release tags | commits 23 / 27 / 30 |
| v1.0.0 | Definition of done: full catalog coverage, ≥ 95 % honest parity in every area, end-to-end against a local IMAP/SMTP server | goal |

## Parity

Two numbers, kept separate on purpose:

- **Catalog coverage** (mechanical, `cargo xtask parity` → [docs/parity.md](docs/parity.md)): every observed
  Outlook command row maps to a MailCraft command id that exists in the registry. Today 1,559 of 1,565 applicable
  rows (99.6 %); 133 rows are not applicable (cloud-only Microsoft services). The 6 missing rows are drawing
  features not built yet (Edit Points, Create Text Box Link, Group Objects, Distribute, Text Wrap). This only
  proves an id exists.
- **Honest parity** (re-audited 2026-10-21 after round 6): each of the 1,565 audited rows classified by what its
  command really does: REAL (works and is reachable), DIALOG-OK (opens a dialog that works), DIALOG-STUB (opens a
  dialog that doesn't do the thing), PARTIAL (works, but clearly less than observed), STUB (no-op, echo or
  error). Honest % = (REAL + DIALOG-OK + ½ PARTIAL) / rows. **Overall: 97.1 %** (94.8 % counting only REAL +
  DIALOG-OK), up from 95.7 % (92.0 %) after round 5 with the same method. Every area is now at 95 % or more;
  Compose is exactly at 95 %, and the strict number is still under 95 % for Compose, Read, Calendar and People.

| Catalog area | Rows | REAL | DIALOG-OK | DIALOG-STUB | PARTIAL | STUB | Honest |
|---|---:|---:|---:|---:|---:|---:|---:|
| Mail (main window) | 262 | 199 | 56 | 0 | 7 | 0 | 98.7 % |
| Context menus | 204 | 162 | 39 | 0 | 3 | 0 | 99.3 % |
| Read window | 84 | 56 | 23 | 2 | 3 | 0 | 95.8 % |
| Search | 40 | 34 | 4 | 0 | 1 | 1 | 96.2 % |
| Compose | 503 | 370 | 89 | 0 | 38 | 6 | 95.0 % |
| Calendar | 122 | 86 | 29 | 0 | 7 | 0 | 97.1 % |
| People | 142 | 102 | 31 | 0 | 9 | 0 | 96.8 % |
| Notes | 64 | 51 | 11 | 0 | 2 | 0 | 98.4 % |
| Tasks | 144 | 123 | 19 | 0 | 2 | 0 | 99.3 % |
| **Total** | **1,565** | **1,183** | **301** | **2** | **72** | **7** | **97.1 %** |

### By functional area (judgement, with evidence)

| Area | Honest | Evidence |
|---|---|---|
| Message list and folders | ~80 % | grouping, Focused/Other, arrange/sort/filter, folder CRUD, favourites, Bottom-pane columns, hover actions; saved views, Add Columns, conditional formatting, Quick Actions settings missing; AutoArchive (scheduled and manual, per-folder settings) |
| Reading pane | ~90 % | header, info bars, inline reply, mark-as-read timing, Space-bar reading, find in a received message (Find…, F4), preview and portrait reading options, read-window Next / Previous links |
| Compose | ~95 % | editing, styles, themes, list formats, rich paste, voting, receipts, S/MIME, accessibility checker, @mentions, system clipboard, Format Painter, Font and Paragraph dialogs, building blocks, screenshots, equations, thesaurus, Read Aloud, floating drawing objects with Shape Format, Borders and Shading, Find and Replace options, bookmarks, Editor pane; no multi-object selection, text wrapping or installed-font rendering |
| Ribbon, menus, shortcuts | ~88 % | data-driven ribbons with width rules and keytips; a few Calendar buttons route through Mail commands |
| Rules and quick steps | ~80 % | 31 conditions, 25 actions, wizard and manage dialogs, Quick Steps |
| Search | ~82 % | indexed query language, refiners, Advanced Find, search folders; scopes and Expiring Soon partial |
| Accounts and sync | ~80 % | IMAP (IDLE, CONDSTORE/QRESYNC, QUOTA, NAMESPACE and shared mailboxes), POP3, JMAP, SMTP, CardDAV/CalDAV for contacts and tasks, Send/Receive groups, OAuth sign-in, OS keyring; no CalDAV event sync, no OAuth end-to-end test |
| Calendar | ~90 % | views, colours, overlay, drag/resize, named time zones, subscriptions, iTIP, Calendar Properties, WebDAV publishing, per-window calendars; appointment window lacks time zones, rich notes and Resources, New Meeting sends through a second compose |
| People | ~96 % | editor, groups, table views with column sets and saved views, vCard/CSV, Mail Merge (e-mail, letters, labels, envelopes), CardDAV sync; card views lack sort/filter settings |
| Tasks | ~97 % | editor, recurrence, task requests, table views with column sets and saved views, Tasks ribbon on flagged messages and contacts, CalDAV task sync; flagged mail has no preview in the To-Do List |
| Notes | ~100 % | create/edit/colour, icon and table views, saved views, positions kept on the note |
| Import/export and print | ~65 % | EML/MSG/MBOX/Maildir/ICS/VCF/CSV, PDF everywhere, CUPS; no PST, no Windows printing |
| Options dialog | ~90 % | every setting persists in the profile; 178 of 228 take effect, 29 are not applicable, 21 are shown or stored only (`settings::effect_counts`) |
| Responsive behaviour | ~97 % | 29 of 30 observed behaviours (Classic Respond group at 1024 px) |
| Keyboard | ~99 % | Space on a focused button; F6 cycles compose and read windows |

### Biggest gaps (ranked by user impact, from the 2026-10-21 audit after round 6)

1. Compose drawing: one drawing is selected at a time, so Group, Align Selected Objects and Distribute are
   missing; text never wraps around drawings; Edit Points and linked text boxes are not built.
2. Calendar: the appointment window has no time-zone picker, rich notes, attachments, Importance or Resources,
   and a new meeting is sent through a second compose window; event calendars are not synced with CalDAV.
3. Data files: New Data File adds a store inside the profile instead of writing a standalone file the user picks.
4. Views: View Settings grouping is on/off with one sort level; card views and calendar grids ignore filters and
   conditional formatting; Reset View in a tasks folder resets the whole module.
5. Compose fonts and proofing: installed fonts are drawn with the closest bundled face; English is the only
   bundled dictionary.
6. Search: no search of older mail that is only on the server; Translate needs a translator the user installs.

The full audit (per-row classification) is regenerated before each release; the commits after 14 and the
milestones after v0.3.0 work down this list until every area is ≥ 95 %.

## Needs the owner

These need you and do not block other work:

- **Code-signing secrets** (optional) for Windows (MSI) and macOS (DMG), when releases should be signed:
  `WINDOWS_CERT_PFX`/`WINDOWS_CERT_PASSWORD` and the `APPLE_*` secrets (docs/releasing.md).

Not needed from you: icon art (all icons are original SVGs made for this project).
