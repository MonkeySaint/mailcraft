# Changelog

What changed in each MailCraft release. Help › What's New shows the newest entries from this file.

## Unreleased

- A message window no longer slows the main window down while it is open: the list order its Previous /
  Next Item use is built once per change of the list instead of several times per frame (a folder with
  thousands of messages made every frame wait).
- Troubleshooting logging records how long a new message or compose window takes to open (its content, and
  creating and showing the window), how long a message body takes to read, and any frame that held up the
  main window for 100 ms or more, part by part.
- Troubleshooting logging also records start-up times (profile opened, window and renderer ready, first
  frame). `MAILCRAFT_RENDERER=wgpu` draws with wgpu (Direct3D 12 on Windows, Vulkan or Metal elsewhere)
  instead of OpenGL, to compare on machines where windows open slowly.

## 0.5.1

- Add Account: "(Not you?)", or another address after an account was added, now sets up a new account and
  leaves the one already added as it is (it used to change that account, and showed its sign-in state).
- Add Account: < Back and another account type for the same address replaces the account the wizard added
  (saving used to fail).
- Add Account: the "Sign in with OAuth 2.0" section can be closed again after the wizard opened it.
- Add Account: the "Contacts, Tasks and Calendar Server" section fits the window instead of widening it.
- Add Account: a greyed-out Sign In says what is still missing (email address, client id or server address);
  an Exchange account needs its EWS address to sign in with OAuth, and Microsoft 365 mailboxes are pointed
  at the Microsoft 365 account type.
- Windows: account setup looks up SRV and MX records again, using the DNS servers of the active network
  adapters, so addresses on their own domain are recognised (for example Microsoft 365 by its MX record).
- Windows: Microsoft sign-ins are kept. Secrets longer than Windows Credential Manager allows (such as
  Microsoft access tokens) are split over several credentials and checked when read back.
- A sign-in whose token cannot be saved now shows as failed, with the reason, instead of "Signed in".
- Opening a message in its own window no longer shows an empty white window first: message and compose
  windows appear once their content is drawn, and a message opened from the list reuses the body the
  reading pane already prepared instead of reading and parsing the whole message again, so the main window
  waits much less.

## 0.5.0

- Microsoft 365 and Outlook.com accounts through Microsoft Graph: sign in with your Microsoft account; the
  folder tree, mail (kept up to date with delta queries), read state, flags, categories, importance, moves,
  deletions and folder changes both ways; sending with large attachments; throttling handled. Account setup
  offers it for Microsoft 365 and Outlook.com addresses.
- Microsoft 365 contacts, calendars and To Do tasks: contact folders, calendars (recurring series with their
  exceptions, time zones, reminders, attendees) and task lists synchronise both ways; meetings are scheduled
  by the service, and Accept / Tentative / Decline answer through it. Calendars other people share appear
  under Shared Calendars, and Add Calendar › Open Shared Calendar and From Address Book open them. A meeting
  someone else organized is never re-created on the service in your name; answers and cancellations to edit
  first open a note before the service sends them; answers that fail for a passing reason are tried again;
  all-day events keep their days east of UTC; fax numbers and anniversaries stay when a contact changes on
  the service.
- Exchange accounts over EWS for on-premises Exchange servers: mail, folders, calendar, contacts and tasks
  both ways, with Basic, NTLM or OAuth sign-in. Autodiscover finds the server from the address and only sends
  the password to hosts in the address's own domain unless you confirm another. Meeting answers go through
  the server, Automatic Replies (Out of Office) is in File › Info, and the Scheduling Assistant shows
  attendees' busy times. Large messages and messages the server refuses no longer stop a sync.
- PST files: File › Open & Export › Import/Export imports a PST file's mail, calendar, contacts, tasks and
  notes into an account or a folder, and Open Data File opens a PST file as a data file. The import runs in
  the background with a progress window you can hide and reopen, Stop keeps what came in, running it again
  adds nothing twice, and a report lists what was left out. Damaged files are read in a separate process so
  they cannot take the app down. MailCraft reads PST files but does not write them.
- Microsoft 365 and Exchange accounts work side by side in one profile.
- Folder names in Calendar, People, Tasks and Notes stay on one line and end in "…" when they are too long,
  instead of being cut off at the top and bottom.

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
