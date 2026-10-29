# Mail accounts

MailCraft talks to mail servers over open protocols only: IMAP and POP3 for incoming mail, SMTP for sending,
and JMAP for both. Exchange (EWS) is not supported. Accounts are added with **File › Add Account** (or
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
organization's tenant, which single-tenant app registrations need. (A Microsoft Graph account kind, when there
is one, is the alternative for Microsoft 365 and Outlook.com addresses: the lookup reports `provider.graph`.)

The lookup runs in the background while the wizard says so (**Skip** stops waiting). The settings page then
says where the settings came from; servers found in DNS outside the address's domain are pointed out and
should only be used when the provider says so (RFC 6186 section 6). A provider whose document lists only
OAuth 2.0 opens the OAuth section. Nothing is sent to the servers until you connect or test.
Domains reserved for examples and tests (`example.com/.net/.org`, `.example`, `.test`, `.invalid`,
`.localhost`) are never looked up.

## IMAP

- Folders and messages are synchronised both ways (CONDSTORE / QRESYNC when the server offers them).
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
- **Push.** After the first successful send/receive the account keeps the server's event source open
  (RFC 8620 section 7.3, `text/event-stream`, a ping every 60 seconds) and starts an update when a
  `StateChange` names a new Email or Mailbox state. A lost stream is reopened with backoff (the Inbox is
  polled meanwhile); a server without an `eventSourceUrl` shows as **Polling** and is checked every
  `pollMinutes`.
- Limits today: no header-only mode, and a message in several mailboxes shows in one folder.

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
OAUTHBEARER, JMAP with a Bearer token). Step-by-step instructions for registering a client id are in
[crates/net/data/README.md](../crates/net/data/README.md).

The local test servers include an OAuth authorization server (device grant, authorization code with PKCE,
rotating refresh tokens) whose tokens sign in to their IMAP, SMTP and JMAP servers; the end-to-end tests
(`crates/engine/tests/e2e_setup.rs`) sign in with the device flow, receive and send over XOAUTH2, and
recover from expired tokens with the refresh token.
