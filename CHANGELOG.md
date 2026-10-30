# Changelog

What changed in each MailCraft release. Help › What's New shows the newest entries from this file.

## Unreleased

- Microsoft 365 and Outlook.com accounts through Microsoft Graph: sign in with your Microsoft account; the
  folder tree, mail (kept up to date with delta queries), read state, flags, categories, importance, moves,
  deletions and folder changes both ways; sending with large attachments; throttling handled. Account setup
  offers it for Microsoft 365 and Outlook.com addresses.

## 0.4.0

- Large mailboxes: message bodies and sources are read from disk when shown instead of being kept in memory,
  folder counts are kept up to date instead of being recounted every frame, and commands and undo record only
  what they change. A profile can no longer grow past the size it can be opened at again.
- Offline mail: server accounts keep message bodies on this computer for a chosen period (1 year by default,
  from 3 days to All) set with a slider in the account settings; headers, flags and previews always stay.
- New profiles can keep their mail in SQLite with compressed pack files (`--store sqlite`, opt-in for now).
- A profile opened in 0.4.0 no longer opens in older versions, and a version that finds a profile saved by a
  newer one refuses it instead of opening it with changes missing. A crash can no longer save half of a
  change such as removing an account.
- Account setup recognises Microsoft 365 and Google Workspace domains from their mail servers, and the big
  consumer domains, and fills in the servers and OAuth sign-in.
- JMAP: folder changes are written back to the server, and Contacts and Calendar sync over JMAP when the server
  offers them. Archiving on a server without an Archive folder creates one.
- CardDAV and CalDAV: a calendar holding both events and tasks fills Calendar and Tasks; uploads never replace
  or delete what another device changed; calendar publishing only writes its own files.
- IMAP IDLE picks up mail that arrived just before it started listening.
- Bookmark dialog; text boxes and shapes saved to the Text Box gallery; Edit Points keeps curves.
- Release-check fixes: layout at 1024x768 and in the dark theme, View in Browser follows Junk Email
  protection, the translation key is only sent to its own server, safer data files and calendar attachments.
- The Linux AppImage starts on a base Ubuntu 22.04.

## 0.3.0

- Compose drawing: shapes, text boxes, WordArt and canvases are objects you can select together, group, align,
  distribute, wrap text around and format with Shape Format and Format Shape; Edit Points and linked text boxes.
- Icons, charts, SmartArt and 3D models are sent as PNG and can be edited after insert; equations reopen for
  editing.
- Compose text: Borders and Shading, Find and Replace options, links to places and bookmarks in the message,
  Sort Text, nine-level lists, Apply Styles pane, Office theme files, installed fonts in the editor and PDF.
- Proofing: more grammar rules, the Editor pane, and Hunspell dictionaries found on the system for other
  languages.
- Appointments and meetings: time zones, formatted notes, attachments, Importance and Resources; a new meeting
  is sent from its own window. Add Holidays.
- Contacts, tasks and calendars sync with CardDAV and CalDAV servers; calendars can be published to WebDAV;
  shared IMAP mailboxes open in the folder pane.
- Recover Deleted Items keeps permanently deleted items across restarts; the status bar shows the mailbox quota;
  Send/Receive groups.
- Tasks: the Tasks ribbon acts on flagged messages and contacts in the To-Do List; Edit Message edits a received
  message in place.
- View Settings: group by up to four fields, sort by up to four keys, fonts and grid lines in every module; New
  Data File can write a standalone file, optionally with a password.
- Mail Merge to letters, labels and envelopes; quick steps carry message options; context menus have access
  keys.
- Search: Include Older Results searches mail that is only on the server. Translate Message works with a
  translation server you set up (nothing is sent until you do).

## 0.2.0

- Data files: Remove keeps the data file so File › Open Data File can open it again; Data File Properties.
- Close All Items asks before closing edited appointment, contact and task windows.
- Open in New Window opens a second main window; read windows close or move on when their item is deleted
  or moved.
- Focused Inbox sorts new mail by itself; Ignore and Always Move Conversation also catch later replies.
- Compose: Cut, Copy and Paste on the ribbon use the system clipboard; Format Painter, full Font and
  Paragraph dialogs, More Colors, style sets, building blocks, screenshots, equations, a thesaurus and
  Read Aloud.
- People, Tasks, Notes and Calendar have table views with column sets, sorting, filters, conditional
  formatting and saved views.
- Update Folder and Send/Receive All Folders download internet calendars again; Create New Folder makes
  calendar, contact, task, note and journal folders.
- Help: topics for every module with search, a training tour and What's New.
- AutoArchive, S/MIME signing and encryption, and many more File › Options settings take effect.

## 0.1.1

- First release on GitHub, with packages for Linux, Windows, macOS and FreeBSD and a web build.
- Fixes for the web build, the Flatpak and the desktop entry.

## 0.1.0

- Mail with a ribbon, folder pane, message list and reading pane; IMAP, POP3, SMTP and JMAP accounts.
- Calendar, People, Tasks and Notes modules.
- Rules, Quick Steps, search folders, conversations, saved views and conditional formatting.
- Import and export of EML, MBOX, iCalendar and vCard files; printing to PDF.
- A command line tool and an MCP server that run every command.
