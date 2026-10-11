# Mail accounts

MailCraft talks to mail servers over open protocols only: IMAP and POP3 for incoming mail, SMTP for sending,
and JMAP for both. Exchange (EWS) is not supported. Accounts are added with **File › Add Account** (or
`accounts.add`); try them against the local test servers (`mailcraft-testmail`) rather than a real mailbox.

## IMAP

- Folders and messages are synchronised both ways (CONDSTORE / QRESYNC when the server offers them).
- **Push.** After the first successful send/receive, the account keeps its connection open and waits on the
  Inbox with IMAP IDLE (RFC 2177). New mail starts an Inbox update without a Send/Receive. IDLE is renewed
  every 25 minutes and interrupted at once when another job comes in. A lost connection is retried with
  backoff from 5 seconds up to 5 minutes; a failed sign-in stops push instead of retrying.
- A server without IDLE shows as **Polling**: the Inbox is checked every `pollMinutes` (default 5).
- Push and polling stop while working offline. Turn push off per account with
  `accounts.options {"account": …, "push": false}` or `sendReceive.preferences`.

## POP3

- Add with `kind: "pop3"` (the wizard's **POP** choice). Default ports: 995 (TLS) and 110 (STARTTLS).
- Options (More Settings): **Leave a copy of messages on the server**, **Remove from server after N days**,
  **Remove from server when deleted from Deleted Items**.
- A server copy is deleted only on the run *after* the message was stored here, and only when the session
  ends with a clean QUIT. Mail is sent over SMTP; local folder changes never reach a POP3 server.

## JMAP

- Folders come from Mailbox/get and Mailbox/changes, mail from Email/query + Email/get on the first run and
  Email/changes after that. Flags, moves and deletions go back with Email/set; the Outbox is sent with
  EmailSubmission/set and the server files the Sent copy.
- Limits today: polling only (no EventSource push), no header-only mode, at most 2,000 messages on the first
  run, and a message in several mailboxes shows in one folder.

## Download headers only

**Send / Receive › Download Headers** fetches only the headers of new mail for that run. Header-only
messages can be marked with **Mark to Download** (or **Mark to Download Message Copy**, which leaves a POP3
server copy) and fetched in full with **Process Marked Headers**. The download preference (full items,
headers then full items, headers only) is stored per account in **Download Preferences**.

## Send/Receive groups

Every account belongs to **All Accounts** unless excluded; F9 and scheduled runs skip excluded accounts.
Each account can have its own send/receive interval. These options and the sync bookkeeping are kept in
`account-options.json` next to the data file (it holds no secrets).

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
before they expire. Step-by-step instructions are in
[crates/net/data/README.md](../crates/net/data/README.md).
