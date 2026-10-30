# Mail accounts

MailCraft talks to mail servers over open protocols and published APIs: IMAP and POP3 for incoming mail, SMTP
for sending, and JMAP for both; Microsoft 365 and Outlook.com mailboxes can also be reached through Microsoft
Graph's public REST API, and Exchange accounts through Exchange Web Services (EWS). Accounts are added with **File › Add Account** (or
`accounts.add`); try them against the local test servers (`mailcraft-testmail`) rather than a real mailbox.

## Finding the server settings

**Connect** in the Add Account wizard first looks the address up (`accounts.discover`), and only guesses
`imap.<domain>` / `smtp.<domain>` when that finds nothing:

0. **Big consumer domains** (`gmail.com`, `googlemail.com`, `outlook.com`, `hotmail.com`, `live.com`,
   `msn.com` and their country variants): the provider's published settings, without any lookup.
1. **Autoconfig.** The provider's `clientConfig` document over HTTPS:
   `https://autoconfig.<domain>/mail/config-v1.1.xml?emailaddress=<address>`, then
   `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`. It gives the IMAP, POP3 and SMTP servers,
   ports, encryption and the user name format. Plain-HTTP locations are never read.
2. **DNS SRV records** (RFC 6186, with the implicit-TLS records of RFC 8314): `_imaps._tcp`, `_imap._tcp`,
   `_pop3s._tcp`, `_pop3._tcp`, `_submissions._tcp`, `_submission._tcp` and `_jmap._tcp` (RFC 8620) under the
   domain, asked of the first name server in `/etc/resolv.conf`. Where there is no resolv.conf (Windows),
   no SRV or MX lookup is made. A target of `.` means the service is not offered.
3. **DNS MX records** of the domain (only the domain is asked about; the address is not sent anywhere):
   mail exchangers under `mail.protection.outlook.com` or `mx.microsoft` are **Microsoft 365**
   (`outlook.office365.com` IMAP 993 / POP 995 with TLS, `smtp.office365.com` 587 with STARTTLS), under
   `olc.protection.outlook.com` **Outlook.com**, and under `google.com` / `googlemail.com` (`aspmx.l.google.com`,
   `smtp.google.com`) **Google Workspace** (`imap.gmail.com` 993, `pop.gmail.com` 995, `smtp.gmail.com` 465).
   The most preferred exchangers decide.
4. **Microsoft 365 tenant.** The tenant's public OpenID configuration,
   `https://login.microsoftonline.com/<domain>/v2.0/.well-known/openid-configuration` (the domain only, never
   the address). **This sends the domain to Microsoft**, and only in two cases: when Microsoft 365 was
   already recognised (its MX records or servers are Microsoft's; the lookup reads the tenant id), and when
   nothing else found anything and the domain's MX records name a mail exchanger MailCraft does not know (an
   organization whose mail passes through a filtering service first). The domain is not sent when the MX
   lookup gave no answer (no resolver, as on Windows; no MX records) or found a null MX, nor for consumer
   domains or domains whose autoconfig, SRV or MX records found their servers. Every domain verified in a
   Microsoft Entra tenant has a tenant, wherever its mail is (an on-premises Exchange server, another host),
   so in the second case Microsoft 365 is only a probable match: its servers are marked unverified and the
   settings page asks you to confirm them with your administrator.

A well-known provider's settings page says how it was recognised and what may need switching on: Microsoft
365 refuses passwords for IMAP, POP and SMTP (sign in with OAuth), the organization may have to turn on IMAP
(or POP) and **Authenticated SMTP** for the mailbox, and an administrator may have to consent to the app's
permissions. The OAuth section opens with the right provider chosen; for Microsoft 365 the sign-in goes to the
organization's tenant, which single-tenant app registrations need. For Microsoft 365 and Outlook.com
addresses (the lookup reports `provider.graph`) the wizard starts with the **Microsoft 365** account type
(Microsoft Graph, see below); with **Let me set up my account manually** the Advanced setup page offers IMAP
and POP with the provider's settings instead.

The lookup runs in the background while the wizard says so (**Skip** stops waiting). The settings page then
says where the settings came from; servers found in DNS outside the address's domain are pointed out and
should only be used when the provider says so (RFC 6186 section 6). A provider whose document lists only
OAuth 2.0 opens the OAuth section. Nothing is sent to the servers until you connect or test.
When nothing else found settings, the lookup asks the domain's Exchange Autodiscover service without
signing in (see Exchange below); a server that answers makes the account an Exchange account.
Domains reserved for examples and tests (`example.com/.net/.org`, `.example`, `.test`, `.invalid`,
`.localhost`) are never looked up.

## IMAP

- Folders and messages are synchronised both ways (CONDSTORE / QRESYNC when the server offers them).
- A message from the server moved into a default folder the server has no mailbox for (Archive, Deleted
  Items, Junk Email, Sent Items or Drafts; most often Archive), or into a folder below one (`Archive/2024`),
  makes that mailbox on the server first, with its special use (RFC 6154 CREATE-SPECIAL-USE, when offered),
  then the folders below it, so the message stays on the server. A JMAP account does the same with the
  mailbox role; a JMAP server that refuses the role gets the mailbox without one, and a server that refuses
  the mailbox shows the error in the account's status (the message stays where it was on the server). A
  server mailbox without a special use or role is recognised by its name ("Archive", "Sent Items", …),
  unless the server marks another mailbox with that role.
- **Push.** After the first successful send/receive, the account keeps its connection open and waits on the
  Inbox with IMAP IDLE (RFC 2177). New mail starts an Inbox update without a Send/Receive. IDLE is renewed
  every 25 minutes and interrupted at once when another job comes in. A lost connection is retried with
  backoff from 5 seconds up to 5 minutes; a failed sign-in stops push instead of retrying.
- A server without IDLE shows as **Polling**: the Inbox is checked every `pollMinutes` (default 5).
- Push and polling stop while working offline. Turn push off per account with
  `accounts.options {"account": …, "push": false}` or `sendReceive.preferences`.

## How much mail the first download takes

**More Settings › Download mail from the past** (`downloadMonths`, default 12 months; `null` = all mail)
limits the *first* download of each folder to mail that arrived in that period: IMAP asks
`UID SEARCH SINCE`, JMAP filters Email/query by `after`. Older mail stays on the server; every later
send/receive downloads all new mail. Changing the setting does not fetch older mail into folders already
downloaded. POP3 has no dates before downloading and always takes everything.

## Offline mail (how long bodies stay on this computer)

An IMAP account keeps the bodies of its messages on this computer for a period chosen on its settings page
(**Offline mail**): a check box, **Keep mail on this computer to read when offline**, and a slider,
**Keep mail from the last**, with 11 stops (3 days, 1 week, 2 weeks, 1 month, 3 months, 6 months, 1 year,
2 years, 3 years, 5 years, All). The default is 1 year. The slider works with the arrow keys (one stop),
Page Up / Page Down (three stops) and Home / End, and screen readers get its value as text ("1 year").

- Message headers, flags, categories and the preview line are always kept. A message received before the
  window loses its source and body from the profile's body store; opening it while online downloads it
  again (the reading pane offers **Download it now** if that did not work), and while working offline the
  reading pane says why only the header is there.
- A message whose body is not here (taken off, or downloaded as headers only) is not moved out of its
  account (to a local folder, a data file or another account; its server copy would be removed), copied,
  forwarded as an attachment, saved or exported: the command says so and starts its download, and works
  once it has arrived. Moves within the account work as usual.
- With the check box off, no bodies are kept: each one is downloaded when it is opened and taken off again
  by the next pass.
- Never taken off: drafts, mail in folders that are not on the server, mail without a server id, a body
  changed here (Edit Message, View › Encoding), the messages selected or open (in the reading pane or a
  read window), and content the undo history still needs. A source is removed only once the profile has
  saved the change that took it off (on a full disk it stays until a later pass).
- A pass runs after each successful send/receive (at most once an hour per account) and at once when the
  setting changes. It takes at most 5,000 messages; the next pass continues. Its changes are not an undo
  step and change nothing on the server.
- Commands: `accounts.options {"account": …, "keepOffline": true, "offlineWindow": "1y"}` (`offlineWindow`
  takes a key `3d` `1w` `2w` `1m` `3m` `6m` `1y` `2y` `3y` `5y` `all`, a label such as `"6 months"`, or
  the slider stop 0–10) and `accounts.applyOfflineWindow {"account": …}` (run a pass now; every IMAP
  account without `account`).
- POP3 and JMAP accounts keep all their mail: a POP3 server may delete what was downloaded, and a single
  JMAP message cannot be fetched again yet.

## POP3

- Add with `kind: "pop3"` (the wizard's **POP** choice). Default ports: 995 (TLS) and 110 (STARTTLS).
- Options (More Settings): **Leave a copy of messages on the server**, **Remove from server after N days**,
  **Remove from server when deleted from Deleted Items**.
- A server copy is deleted only on the run *after* the message was stored here, and only when the session
  ends with a clean QUIT. Mail is sent over SMTP; local folder changes never reach a POP3 server.

## JMAP

- Folders come from Mailbox/get and Mailbox/changes, mail from Email/query + Email/get on the first run (the
  download window above, newest first, page by page) and Email/changes after that. Flags, moves and
  deletions go back with Email/set; the Outbox is sent with EmailSubmission/set and the server files the
  Sent copy.
- Folders made, renamed, moved or deleted here go back with Mailbox/set (a new folder is created before
  mail is moved into it; a deleted folder goes into Deleted Items first, and its mailbox is removed when it
  is deleted from there; a mailbox still holding mail this computer never downloaded is kept, and its
  folder comes back). Folders below the Outbox, or below a default folder the server has no mailbox for,
  stay on this computer until mail from the server is moved into them (see IMAP above). Undo and redo keep
  what synchronisation brought meanwhile: a folder another client made is not removed from the server by
  undoing an earlier command, and a folder brought back whose mailbox is gone is made there again.
- **Contacts and calendars over JMAP.** When the server offers JMAP for Contacts (RFC 9610) and JMAP for
  Calendars, the account's **Contacts** and **Calendar** folders synchronise with its address books and
  calendars over JMAP, without any DAV settings; the rules are the ones below for CardDAV / CalDAV. Cards
  are JSContact (RFC 9553) and events JSCalendar: a recurring series with its changed and removed
  occurrences is one event (`recurrenceRule`, `recurrenceOverrides`). An edit writes only the properties
  it changed: a property it did not touch stays on the server as it is, with what MailCraft does not show
  inside it (further organizations, a photo stored as a blob, further alerts, extra locations), and so does
  a property MailCraft does not map. A property the edit changed (the company, the reminder, the location)
  is replaced by MailCraft's version of it, which holds what MailCraft shows. Events are written with
  `sendSchedulingMessages: false` (the server never mails attendees for MailCraft). An account whose
  settings name a CardDAV / CalDAV server keeps using it for all three folders; clear the DAV server to
  move Contacts and Calendar to JMAP (items keep their UIDs, so the server's copies are matched rather than
  uploaded again). JMAP has no task lists MailCraft uses: **Tasks** synchronise with a CalDAV server if one
  is set in the account's settings.
- **Push.** After the first successful send/receive the account keeps the server's event source open
  (RFC 8620 section 7.3, `text/event-stream`, a ping every 60 seconds) and starts an update when a
  `StateChange` names a new Email or Mailbox state. A lost stream is reopened with backoff (the Inbox is
  polled meanwhile); a server without an `eventSourceUrl` shows as **Polling** and is checked every
  `pollMinutes`.
- Limits today: no header-only mode, and a message in several mailboxes shows in one folder.

## Microsoft 365 and Outlook.com (Microsoft Graph)

A **Microsoft 365** account (`accounts.add {"kind": "graph", "address": …}`) reads and sends a Microsoft 365 or
Outlook.com mailbox through the Microsoft Graph REST API v1.0 (`https://graph.microsoft.com/v1.0`, the
`incoming.host` of the account; another endpoint, such as a national cloud's, can be set there). It has no
SMTP server and takes no password: sign in with **Sign in with OAuth 2.0**, provider **Microsoft**. For a
Graph account the sign-in asks for Graph's delegated permissions instead of the IMAP/POP/SMTP ones (one token
serves one service).

**Permissions to grant** in the Microsoft Entra app registration (API permissions › Microsoft Graph ›
Delegated): `Mail.ReadWrite` (read and change mail and folders), `Mail.Send` (send as the user) and
`offline_access` (a refresh token). Calendars, contacts and tasks over Graph add their own permissions. An
organization that does not let users consent to apps needs an administrator's consent once. IMAP, POP and
Authenticated SMTP do not have to be turned on for the mailbox.

- **Folders.** The whole folder tree with child folders. Graph's well-known folders (Inbox, Drafts, Sent
  Items, Deleted Items, Junk Email, Archive) are the default folders; Graph's own Outbox stays out (MailCraft
  keeps its own). Folders made, renamed, moved or deleted here are made, renamed, moved or deleted there; a
  new folder is created before mail is moved into it. A folder made below a default folder the service does
  not have (Conversation History, RSS Feeds) stays on this computer; mail moved into it keeps its copy on the
  service where it was. A folder disappears here only when the service's whole folder list was read: a
  cancelled update, or a list cut short at 5,000 folders or 32 levels, removes nothing.
- **Mail.** Each folder is read with a delta query: the first one takes the mail of the download window
  (`downloadMonths`), page by page, and the `deltaLink` it ends with is kept, so later updates only bring what
  changed (new mail, read state, flags, categories, importance, moves, deletions). A delta state the service
  no longer knows starts that folder over. New messages arrive with their MIME source (`$value`); with a
  headers-first download preference only their header (built from the message's properties) arrives, and
  the rest is downloaded when the message is opened, marked for download, or by **Download Headers and Then
  Full Items** in the same run. A message moved on the service keeps its item here (immutable ids), also when
  only the Inbox is updated (the poll, or one folder's Send/Receive): the service is asked where a message
  that left the folder went, and only one it no longer has is removed here. A message that could not be
  stored here (a full disk) is listed again by the next update.
- **Changes made here** go back with the next run, or right after the command: read and unread, the
  follow-up flag (with its start and due date, also when only the dates change) and completion, categories,
  importance, moves between folders, and permanent deletion. Mail moved or copied in from elsewhere is
  uploaded with its state; the service files uploads as drafts, but they stay ordinary mail here. When a
  large file of an upload (or of a large message being sent) cannot go up, the message is deleted from the
  service again and tried once more on the next run.
- **Sending.** Outbox messages are sent with `sendMail` as MIME content (Bcc recipients included), and the
  service files the copy in Sent Items, which the next update brings back. A message too large for one request
  is made as a draft without its large attachments, which then go up in pieces through an upload session,
  and the draft is sent.
- **Throttling.** When the service answers 429 (or 503/504 for requests that can be repeated) the run waits
  as its `Retry-After` asks (at most two minutes at a time, five times per request) and carries on.
- **Focused Inbox.** New mail keeps the service's Focused / Other classification; Always Move to Focused /
  Other still wins.
- **Push.** Graph notifies only public web endpoints, so a Graph account with push on has its Inbox polled
  every `pollMinutes` instead (**Polling**).
- Limits today: no server search (Include Older Results), no offline window, uploaded messages are drafts on
  the service, and Save Sent Item To has no effect (the service files the copy in Sent Items).

The local test servers include a fake Graph mail service (`TestServers::graph_url`) on the same mail as their
IMAP, POP3 and JMAP servers, with paging, delta links, `$value`, `sendMail`, upload sessions, flag dates, and
throttling or refused upload sessions on demand; `crates/engine/tests/e2e_graph.rs` runs a Graph account against it end to end, and never against a
Microsoft service.

## Exchange (EWS)

Exchange accounts use Exchange Web Services (SOAP over HTTPS, written from Microsoft's Open Specifications
[MS-OXWS…]). They work with Exchange servers that still offer EWS and with other servers that implement it
(a grommunio server, for example). Microsoft 365 is retiring EWS; use a Microsoft 365 (Graph) account
there.

- **Setting up.** Choose **Exchange** as the account type (Advanced setup), or let the lookup choose it: when
  an address has no IMAP, POP or JMAP settings, the lookup asks the domain's Autodiscover service
  ([MS-OXDSCLI] POX: `https://<domain>/autodiscover/autodiscover.xml`, then
  `https://autodiscover.<domain>/autodiscover/autodiscover.xml`, then the plain-HTTP redirect of
  `http://autodiscover.<domain>/autodiscover/autodiscover.xml`, which is only followed to an HTTPS address,
  then the `_autodiscover._tcp` SRV record). Leave **EWS address** empty and the address is found with the
  password when the account is added (`accounts.add` with `"autodiscover": true`); or enter the server's EWS
  address, such as `https://mail.example.com/EWS/Exchange.asmx`. A plain-HTTP EWS address is refused.
- **Where the password goes during Autodiscover.** Only to hosts in the address's own domain (the domain or a
  subdomain of it). The plain-HTTP redirect, the SRV record and redirects are not authenticated, so a location
  they name in another domain is asked without credentials, and an EWS address outside the domain is not
  used either: the account is not added, and the wizard shows the host with a **Sign in at <host>** button
  (`accounts.add` answers `{"needsConfirm": true, "confirmHost"}`; add the host to `"autodiscoverHosts"` to
  agree). Use it only when your provider told you to.
- **Sign-in.** Basic, NTLM (NTLMv2, [MS-NLMP]; the user name may be `DOMAIN\user`) or an OAuth bearer token.
  With no method set, MailCraft tries Basic and changes to NTLM when the server offers only NTLM or
  Negotiate. NTLM signs in one connection, which is kept open for the requests that follow.
- **A server with its own certificate.** Paste it into the account's extra root certificate setting
  (`incoming.extraRootPem`). A certificate given there is trusted for that server only, and the server's name
  is still checked against it; TLS checks are never switched off.
- **Mail.** Folders come from FindFolder and SyncFolderHierarchy, mail from SyncFolderItems (each folder
  keeps the server's sync state; the first run takes the download window, newest first) with the message
  source from GetItem (MimeContent). Read state, flags and categories go back with UpdateItem, moves with
  MoveItem, deletions with DeleteItem, new folders, renames and folder moves with CreateFolder, UpdateFolder
  and MoveFolder, drafts and copies with CreateItem. The Outbox is sent with CreateItem (SendAndSaveCopy):
  the server files the Sent copy. The server's Outbox and non-mail folders are not shown as mail folders.
- **Contacts, calendar and tasks** synchronise with the mailbox's Contacts, Calendar and Tasks folders over
  EWS, with the rules for CardDAV / CalDAV below: an edit sends only the fields it changed (UpdateItem with
  the item's change key, so an edit made elsewhere meanwhile is not overwritten). A recurring series keeps
  its recurrence, its changed occurrences and its removed ones as they come from the server; a single
  occurrence changed or removed here is not written back yet (the update is refused with an error, so change
  the whole series instead). An account whose settings name a DAV server uses that instead.
- **Meeting requests** received on an Exchange account are answered on the server (AcceptItem,
  TentativelyAcceptItem, DeclineItem): the server updates its calendar and, unless you chose not to send a
  response, tells the organizer (an answer not sent leaves no draft behind). The answer is made here first,
  so a failure here reaches nobody. If the server cannot answer, a reply mail goes as for other accounts.
- **Automatic Replies** (File › Info, for the Exchange account chosen there; `file.automaticReplies`): on,
  off or for a time range, one text for your
  organization and one for others (nobody, your contacts or everyone), read and saved with
  GetUserOofSettings / SetUserOofSettings.
- **Other people's free/busy time.** `calendar.freeBusy` with `attendees` asks the server
  (GetUserAvailability) for their busy times. The Scheduling Assistant shows them in the attendees' rows,
  asked once per day shown, without waiting on the server in the window.
- **Large messages and refusals.** New mail is downloaded in batches of at most ten messages and 16 MB; a
  message too large to download on its own stays on the server and is reported, and a message the server
  refuses to store (too large, over quota) stays here only and is reported: neither stops sending or
  receiving. Mail moved into a folder the server does not have stays where it was on the server; a folder is
  removed there only when it holds no mail and no subfolders.
- **New mail** is checked every `pollMinutes` (the account shows as **Polling**); EWS push and streaming
  notifications are not used.
- Limits today: no header-only mode, no public folders, no delegate access.

## Download headers only

**Send / Receive › Download Headers** fetches only the headers of new mail for that run.

**On Slow Connections Download Only Headers** (`slowHeadersOnly`, on by default; `"toggle"` flips it) is
honoured for IMAP and POP3: each run measures the transfer rate of the mail it downloads (only runs that
download at least 32 KiB count), and while the last measurement is below 128 kbit/s (16,000 bytes per
second) new mail arrives as headers only, like Download Headers. A measurement counts for an hour; after
that (or when a run measures a faster link) the account's download preference applies again. The status shows `slowConnection` per account. Header-only
messages can be marked with **Mark to Download** (or **Mark to Download Message Copy**, which leaves a POP3
server copy) and fetched in full with **Process Marked Headers**. The download preference (full items,
headers then full items, headers only) is stored per account in **Download Preferences**.

## Send/Receive groups

Every account belongs to **All Accounts** unless excluded; F9 and scheduled runs skip excluded accounts.
Each account can have its own send/receive interval. These options and the sync bookkeeping are kept in
`account-options.json` next to the data file (it holds no secrets).

## Contacts, tasks, calendars and published calendars

An IMAP or JMAP account can also keep its **Contacts**, **Tasks** and **Calendar** folders on a server: in
Account Settings › Change › **Contacts, Tasks and Calendar Server**, enter the server's address (a CardDAV /
CalDAV server, principal or collection URL; the account's password signs in, or a separate user name), and
tick what to keep there. Send/Receive All Folders and **Update Folder** on Contacts, Tasks or Calendar then
synchronise both ways (CardDAV, RFC 6352; CalDAV tasks and events, RFC 4791):

- the Contacts folder takes the server's first address book, the Tasks folder its first task list and the
  Calendar its first calendar of events; more address books, task lists and calendars get folders of their
  own below them;
- a recurring series and its changed occurrences are one resource on the server (RECURRENCE-ID overrides);
  editing one occurrence here uploads the series again, and a server change to one occurrence arrives as
  that occurrence; a calendar collection that holds tasks too is shared by the Calendar and Tasks folders,
  each taking its own kind of item;
- an item changed here is uploaded with `If-Match`, a new one with `If-None-Match: *`, one deleted here is
  deleted there; changes and deletions made on the server arrive here; when both sides changed an item the
  copy changed here wins (a change made on the server during the run is never overwritten unseen);
- details only MailCraft keeps (a contact's follow-up flag, a task's link to its mail) stay local;
- **View › Change View › Server Tasks** lists the tasks that live on the server;
- the links between items and their server copies survive settings changes that keep the same contacts
  server and login (a new mail server, the address written with or without its trailing `/`); when they are
  lost, the next run recognises items already on the server (same UID, or the same card) instead of
  uploading them again;
- the account's password or token is only sent to the contacts server and the publishing location; an
  address a server answers with that points elsewhere is not followed, and a listed item outside its
  collection is left out (that run then removes nothing here: the listing may be incomplete);
- a collection the server moved (its listing is redirected) is followed to its new address, links included;
  links saved by an earlier MailCraft version are recognised as they are and take the server's form;
- the server has connection settings of its own: a **Password (if different)**, the consent to sign in over
  plain `http://`, and (`accounts.edit {"dav": {"extraRootPem": …}}`) a trusted certificate for a self-signed
  server. Nothing is taken over from the mail server's settings. A saved password of its own can be removed
  again (**Use the account's password instead**, or `"password": ""`). An account signed in through its mail
  provider's page (OAuth) sends that token only to the provider's own servers; any other contacts server
  needs its own password. A token the contacts server refuses is renewed once and the server asked again,
  as long as nothing of that run was applied yet;
- every request has a time limit (five minutes) and stops when Send/Receive is cancelled.

Update Folder on a calendar kept on this computer only says so, and how to keep it on a server.

The same section takes a **publishing location**, a WebDAV collection. **Share Calendar** (one entry per
calendar) can then publish a calendar there and write a sharing invitation with its `webcal://` address;
every send/receive uploads it again when it changed, and subscribers refresh it (`REFRESH-INTERVAL`). The
commands are `calendar.publish` and `calendar.share {"publish": true}`. A calendar is only published as a
file inside the account's publishing location (not a folder, not anywhere else on that server). MailCraft
only ever replaces or removes the file it wrote itself: the first upload creates the file and fails when
something is already at that address, later uploads and the removal need the file to be unchanged since
the last upload (`If-Match`), and a file changed or replaced by someone else is left as it is. Publishing
again at another address removes the earlier file with the next send/receive. When a file that stops being
published cannot be removed that way (it changed since, or an earlier version published it outside the
current publishing location), Send/Receive reports the address that is still public and keeps asking until
the file is gone (remove it on the server).

## Shared and other users' mailboxes

A full send/receive of an IMAP account lists the mailboxes its login may open besides its own (the RFC 2342
"other users" and "shared" namespaces). Account Settings › Change › **Shared Mailboxes** opens one as a
mailbox of its own (`accounts.openSharedMailbox`): it signs in with the account's login, lists only that
mailbox's folders, cannot send, and appears in the folder pane and in Search › Locations to Search.

## Passwords and tokens

- The desktop app keeps account passwords and OAuth tokens in the operating system's credential store: the
  macOS Keychain, the Windows Credential Manager, or the Secret Service keyring (GNOME Keyring, KWallet) on
  Linux and the BSDs. Secrets already in the profile are moved there when the app starts.
- Where no credential store is available (no keyring daemon or no D-Bus session), secrets stay in
  `secrets.json` in the profile folder. That file is **not encrypted**; Account Settings says so, and its
  **Use System Keyring** button retries the move (`accounts.useKeyring {"confirm": true}`).
- `mailcraft-cli` reads secrets from the same place when the profile has mail accounts.

## OAuth 2.0 sign-in

Some providers (for example Google and Microsoft) need OAuth 2.0 instead of a password. MailCraft ships the
providers' public endpoints and scopes, but **no client id**: register a desktop / public-client
application with the provider and enter its client id in **Sign in with OAuth 2.0**. Both the device-code
flow and the browser flow (PKCE with a one-time 127.0.0.1 redirect) are supported, and tokens are refreshed
before they expire, and again when a server turns a token down (IMAP, POP3 and SMTP sign-in with XOAUTH2 or
OAUTHBEARER, JMAP and Microsoft Graph with a Bearer token). Step-by-step instructions for registering a client id are in
[crates/net/data/README.md](../crates/net/data/README.md).

The local test servers include an OAuth authorization server (device grant, authorization code with PKCE,
rotating refresh tokens) whose tokens sign in to their IMAP, SMTP and JMAP servers; the end-to-end tests
(`crates/engine/tests/e2e_setup.rs`) sign in with the device flow, receive and send over XOAUTH2, and
recover from expired tokens with the refresh token.
