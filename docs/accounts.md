# Mail accounts

MailCraft talks to mail servers over open protocols only: IMAP and POP3 for incoming mail, SMTP for sending,
and JMAP for both. Exchange (EWS) is not supported. Accounts are added with **File › Add Account** (or
`accounts.add`); try them against the local test servers (`mailcraft-testmail`) rather than a real mailbox.

## Finding the server settings

**Connect** in the Add Account wizard first looks the address up (`accounts.discover`), and only guesses
`imap.<domain>` / `smtp.<domain>` when that finds nothing:

1. **Autoconfig.** The provider's `clientConfig` document over HTTPS:
   `https://autoconfig.<domain>/mail/config-v1.1.xml?emailaddress=<address>`, then
   `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`. It gives the IMAP, POP3 and SMTP servers,
   ports, encryption and the user name format. Plain-HTTP locations are never read.
2. **DNS SRV records** (RFC 6186, with the implicit-TLS records of RFC 8314): `_imaps._tcp`, `_imap._tcp`,
   `_pop3s._tcp`, `_pop3._tcp`, `_submissions._tcp`, `_submission._tcp` and `_jmap._tcp` (RFC 8620) under the
   domain, asked of the first name server in `/etc/resolv.conf`. Where there is no resolv.conf (Windows),
   no SRV lookup is made. A target of `.` means the service is not offered.

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

## Contacts, tasks and published calendars

An IMAP or JMAP account can also keep its **Contacts** and **Tasks** folders on a server: in Account
Settings › Change › **Contacts, Tasks and Calendar Server**, enter the server's address (a CardDAV / CalDAV
server, principal or collection URL; the account's password signs in, or a separate user name). Send/Receive
All Folders and **Update Folder** on Contacts or Tasks then synchronise both ways (CardDAV, RFC 6352;
CalDAV tasks, RFC 4791):

- the Contacts folder takes the server's first address book and the Tasks folder its first task list; more
  address books and task lists get folders of their own below them;
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
  address a server answers with that points elsewhere is not followed.

The same section takes a **publishing location**, a WebDAV collection. **Share Calendar** (one entry per
calendar) can then publish a calendar there and write a sharing invitation with its `webcal://` address;
every send/receive uploads it again when it changed, and subscribers refresh it (`REFRESH-INTERVAL`). The
commands are `calendar.publish` and `calendar.share {"publish": true}`. A calendar is only published on the
account's publishing server; publishing it again at another address removes the earlier file with the next
send/receive.

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
