# Live server tests

The normal test suite runs against the local test servers only (`mailcraft-testmail`, 127.0.0.1, synthetic
users). `crates/engine/tests/live_servers.rs` holds a second, opt-in suite that drives the engine like a user
against a real mail server, to find what the local servers do not show. It never runs in CI: every test is
`#[ignore]`, and returns early unless the settings below are present.

## What it covers

| Test | What it does |
|---|---|
| `live_discovery_finds_the_servers` | `accounts.discover` on the test address (autoconfig, DNS SRV): IMAP, SMTP and JMAP settings |
| `live_archive_keeps_mail_on_the_server` | Archive on a server without an archive mailbox (IMAP and JMAP): the mailbox is made there with its special use / role, and the message stays on the server |
| `live_imap_and_smtp` | adds the account from discovered settings; Test Account Settings; folder mapping (Sent, Deleted Items, Drafts, Junk); receive; read and flag; a new folder; move, delete, permanent delete; folder delete; IDLE push; SMTP send to itself (implicit TLS) and the Sent copy; quota and shared mailboxes (reported) |
| `live_include_older_results` | an old message outside the download window, found by Search › Include Older Results over IMAP and JMAP |
| `live_pop3_leaves_mail_on_the_server` | POP3 over implicit TLS, leave a copy on the server, no duplicates |
| `live_jmap_mail_push_and_submission` | a JMAP account: receive, keywords, folders (create, move into, delete), push over the event source, sending with EmailSubmission to itself, quota |
| `live_carddav_and_caldav` | an IMAP account with a CardDAV / CalDAV server: contacts, a recurring series with an exception, and tasks, created, edited and deleted on both sides |
| `live_ews_mail` (`live_ews.rs`) | an Exchange (EWS) account: sign-in, folders, receive, read state, flags and categories, moves, a new folder, uploads, delete, sending to itself with the Sent copy |
| `live_ews_contacts_calendar_tasks` (`live_ews.rs`) | contacts, a recurring series with a changed occurrence, and tasks over EWS, both ways |
| `live_dav_against_the_same_server` (`live_ews.rs`) | the same mailbox's CardDAV / CalDAV: contacts, a recurring series and a task over DAV, checked through EWS |
| `mailcraft-net` `live_ews.rs` | the EWS client itself: Autodiscover, sign-in, Out of Office settings, free/busy |
| `live_jmap_contacts_and_calendars` | a JMAP account with no DAV settings whose server offers JMAP for Contacts and Calendars: contacts and a recurring series with an exception, both ways |

## Rules the tests keep

- They use one mailbox kept for testing, and send mail only to that mailbox itself.
- Everything a test creates carries a run tag (`MCLIVE…` in subjects, names and UIDs) and is removed when the
  test ends, also when it fails (a drop guard); nothing else in the mailbox is touched.
- Server checks retry for up to 90 seconds (servers index and deliver asynchronously).

## Running

Set the server and the test mailbox in the environment (never in a file under version control):

```sh
export MC_LIVE_STALWART_HOST=mail.example.com      # IMAPS 993, POP3S 995, SMTPS 465, HTTPS 443
export MC_LIVE_STALWART_USER=test-mailbox@example.com
export MC_LIVE_STALWART_PASS='…'
cargo test -p mailcraft-engine --test live_servers -- --ignored --test-threads=1
```

The Exchange tests (`crates/engine/tests/live_ews.rs`, `crates/net/tests/live_ews.rs`) read their own
settings. A server with a certificate of its own is trusted through the account's extra root certificate
(`MC_LIVE_EWS_CA`, a PEM file), never by switching TLS checks off; `MC_LIVE_EWS_ADDR` connects to an address
other than the one the server name resolves to, for a test server without DNS:

```sh
export MC_LIVE_EWS_URL=https://mail.example.com/EWS/Exchange.asmx
export MC_LIVE_EWS_USER=test-mailbox@example.com
export MC_LIVE_EWS_PASS='…'
export MC_LIVE_EWS_CA=path/to/server.pem          # optional
export MC_LIVE_EWS_ADDR=192.0.2.10                # optional
export MC_LIVE_DAV_URL=https://mail.example.com/dav/   # optional, the DAV test
cargo test -p mailcraft-engine --test live_ews -- --ignored --test-threads=1
cargo test -p mailcraft-net --test live_ews -- --ignored --test-threads=1
```

Run them one after another (`--test-threads=1`): they share the mailbox, and a push test should not see
another test's mail. The suite was written against a Stalwart server (IMAP, POP3, SMTP submission, JMAP
with contacts and calendars, CardDAV and CalDAV on one host); a server without one of these fails the test
for it, and a server without JMAP contacts skips that test.
