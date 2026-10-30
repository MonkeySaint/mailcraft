# PST files

MailCraft can import a PST data file (`.pst`) and open one as a data file of its own. It reads PST files
only: it never writes or changes them.

## Importing

**File › Open & Export › Import/Export › Import a PST file (.pst)**, then pick the file and a folder:

- **Put the file's Inbox, Calendar and other default folders into this folder's account** (the default):
  the PST file's Inbox goes into the account's Inbox, its Sent Items into Sent Items, its Calendar into the
  Calendar and so on; its other folders are created at the top of the account, or merged into a folder of
  the same name and kind that is already there. Subfolders keep their place.
- **Put all of the file's folders beneath this folder**: the whole folder tree of the file goes below the
  chosen folder.

Items already in the target folder are left out, so an import that was stopped can be run again: messages
by their Message-ID, meetings by their meeting UID, and drafts, other messages without a Message-ID,
calendar items, contacts, tasks and notes by what they hold (subject, times, names, text). Tick **Import
items that are already there again** to bring them in anyway.

Calendar items keep the UID of their meeting (from the meeting's object id), so later updates and
cancellations still match them; an item without one gets a new UID that no other file or run shares.

When the target account synchronises with a server, what the import adds is sent to the server like any
other change: new folders are created there and the messages uploaded (IMAP), or pushed (JMAP).

The import runs in the background. A window shows how far it is and has a **Stop** button; what came in
before the stop stays. **Hide** closes the window while the import goes on: the status bar then shows
**Importing PST file**, and clicking it (or **PST Import** on File > Open & Export) opens the window again.
When it ends, the window says what was imported and lists what was not, and why; it opens again by itself
when something was left out.

## Opening a PST file as a data file

**File › Open & Export › Open Data File...**, then pick a `.pst` file. MailCraft makes a new data file named
after the file and imports the PST file into it, with the same folder tree. **Remove** on the data file's
context menu closes it again, like any other data file (it is kept with the profile's closed data files and
can be opened again). The PST file itself is not linked: changes made in MailCraft stay in MailCraft.

## What comes across

| Item | What is imported |
|---|---|
| Folders | the tree below the top of the file; the kind of each folder (mail, calendar, contacts, tasks, notes) from its container class |
| Mail | sender, recipients, subject, dates, Message-ID and references, plain text, HTML and compressed RTF bodies (HTML wrapped in RTF is unwrapped), attachments, read state, importance, sensitivity, categories, follow-up flags (with due dates and completion), drafts; an RFC 5322 source is built for each message |
| Calendar | subject, location, start and end, all-day, show as, reminder, categories, private, notes, attachments, attendees of meetings; recurring series with their pattern (daily, weekly, monthly, yearly, with an end date or a count) and deleted occurrences |
| Contacts | names, file as, company and job, e-mail addresses, phone numbers, business, home and other addresses, web page, IM address, birthday, anniversary, notes, categories |
| Tasks | subject, start and due dates, status, % complete, priority, reminder, completed date, notes, categories |
| Notes | text, colour, created and modified dates, categories |

Not imported (each is listed in the report): contact groups, journal entries, attachments stored as links
to files, attachments over 40 MB per message, and attached messages (a message forwarded as an attachment;
the PST reader MailCraft uses cannot open them yet). Changed occurrences of a recurring series come across
as the series' pattern. Search folders are not imported.

Files MailCraft refuses with a message: files that are not PST files, offline cache files, PST files of a
format version it does not know, and password-protected PST files (remove the password in the program that
created the file, then import it again).

## Exporting

MailCraft does not write PST files. To take mail elsewhere, export folders to MBOX, calendars to iCalendar,
contacts to vCard or CSV, save messages as `.eml` or `.msg` files (**File › Save As**), or keep whole folders
in a MailCraft data file (`.mcdata`, **File › Open & Export › New Data File**).

## Damaged and hostile files

A PST file is untrusted input. In the desktop app and the CLI the file is read in a separate process, so a
damaged file that makes the reader fail can only end that process: the import stops, says so, and keeps what
it had read. Inside the reader every folder and item is read under a panic guard, reads stop at a byte
budget (a damaged index that loops is cut off), and folders, items, attachments and nesting are capped. The
files are read with Microsoft's MIT-licensed `outlook-pst` crate, the reference reader for the public
[MS-PST] specification; the mapping to MailCraft items follows [MS-OXCMSG], [MS-OXOCAL], [MS-OXOCNTC],
[MS-OXOTASK] and [MS-OXONOTE].

## Commands

| Command | Parameters |
|---|---|
| `file.openExport` | `{"action": "importPst" (or "import" with a .pst path), "path", "folder"?, "into"?: "account" or "beneath", "duplicates"?: "skip" or "allow", "wait"?: true, "timeoutSecs"?}` |
| `file.openDataFile` | `{"path": "….pst", "wait"?: true}` |
| `file.importProgress` | `{"cancel"?: true, "show"?: true}`: the running import's progress, or the last one's report; `show` opens the progress window |

With `"wait": true` (the CLI, MCP and scripts) the command returns when the import is done, with its report.
