use serde::{Deserialize, Serialize};

use crate::AccountId;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountKind {
    /// Local data file only (like a PST-only profile): no sending.
    #[default]
    Local,
    Imap,
    Jmap,
    Exchange,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Security {
    None,
    StartTls,
    #[default]
    Tls,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
    /// Auth mechanism name ("PLAIN", "LOGIN", "XOAUTH2", …); empty = auto.
    pub auth: String,
    /// Extra trusted root certificate (PEM) for this server, e.g. a self-signed certificate the user accepted
    /// or a local test server's. Advanced setting; the system roots are still used.
    #[serde(default)]
    pub extra_root_pem: Option<String>,
    /// Allow credentials over an unencrypted connection (only for a server the user explicitly trusts).
    #[serde(default)]
    pub allow_plaintext_auth: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub kind: AccountKind,
    /// Shown as the mailbox root label (the address, as observed).
    pub display_name: String,
    pub address: String,
    /// Sender name for outgoing mail.
    pub sender_name: String,
    pub incoming: Option<ServerSettings>,
    pub outgoing: Option<ServerSettings>,
    /// Default account for new messages.
    pub is_default: bool,
    /// How the account logs in. The secret itself is never stored in the mailbox (see the engine's
    /// `SecretStore`).
    #[serde(default)]
    pub credential: CredentialKind,
    /// The server files a copy of every sent message itself, so the client must not upload one to Sent.
    #[serde(default)]
    pub server_saves_sent: bool,
    /// Synchronisation bookkeeping (server folder states, queued server changes).
    #[serde(default)]
    pub sync: AccountSync,
    /// Contacts and tasks server (CardDAV / CalDAV) and calendar publishing location, if set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dav: Option<DavSettings>,
    /// This store is another user's or a shared mailbox opened through `owner`'s IMAP login (RFC 2342
    /// namespaces): it signs in as the owner and lists only the folders below its root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<SharedStore>,
    /// A data file kept in a file of its own (New Data File at a chosen path, or opened from one): where it
    /// is written when it is closed or saved. Its password, if any, is in the engine's secret store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_file: Option<DataFileLink>,
}

/// Where a standalone data file lives on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DataFileLink {
    /// The file's path.
    pub path: String,
    /// The file is encrypted under a password.
    pub protected: bool,
}

/// Where an account's contacts, tasks and published calendars live on a WebDAV server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DavSettings {
    /// Server, principal or collection URL discovery starts from (`https://dav.example.com/`).
    pub url: String,
    /// Synchronise the Contacts folder with the server's address book (CardDAV, RFC 6352).
    pub contacts: bool,
    /// Synchronise the Tasks folder with the server's task list (CalDAV VTODO, RFC 4791).
    pub tasks: bool,
    /// Synchronise the Calendar with the server's calendar of events (CalDAV VEVENT, RFC 4791).
    pub calendar: bool,
    /// WebDAV collection calendars are published into (empty: publishing is not set up).
    pub publish_url: String,
    /// User name for the DAV server (empty: the incoming server's).
    pub username: String,
    /// Extra trusted root certificate(s) for the DAV and publishing servers, PEM (a self-signed server). The
    /// incoming mail server's certificate is not trusted here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_root_pem: Option<String>,
    /// Allow the DAV login over plain `http://` (a server on this computer or a trusted LAN). Separate from the
    /// incoming mail server's setting.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub allow_plaintext_auth: bool,
}

/// A URL path (without its query) in one normal form, for comparing addresses and for sending them
/// (RFC 3986 section 6.2.2): escapes of unreserved characters and of `:` and `@` are decoded, any other escape is
/// kept (in upper-case hex), and a byte a path cannot hold as it is (a space, a non-ASCII byte, a lone `%`) is
/// escaped. `/` separators stay as written. An escaped `/`, `%` or `;` therefore keeps its escape, so the
/// normal form names exactly the resource the original did, and normalising twice changes nothing.
pub fn normalize_url_path(path: &str) -> String {
    let decoded_ok =
        |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b':' | b'@');
    let raw_ok = |b: u8| decoded_ok(b) || b == b'/' || b"!$&'()*+,;=".contains(&b);
    let hex = |h: Option<&[u8]>| -> Option<u8> {
        let h = std::str::from_utf8(h?).ok()?;
        if h.bytes().all(|c| c.is_ascii_hexdigit()) { u8::from_str_radix(h, 16).ok() } else { None }
    };
    let b = path.as_bytes();
    let mut out = String::with_capacity(b.len());
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        if c == b'%'
            && let Some(v) = hex(b.get(i + 1..i + 3))
        {
            if decoded_ok(v) {
                out.push(char::from(v));
            } else {
                out.push_str(&format!("%{v:02X}"));
            }
            i += 3;
            continue;
        }
        if raw_ok(c) {
            out.push(char::from(c));
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
        i += 1;
    }
    out
}

/// Which namespace a shared store comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SharedKind {
    /// Another user's mailbox (the IMAP "other users" namespace).
    #[default]
    OtherUser,
    /// A mailbox shared by several people (the IMAP "shared" namespace).
    Shared,
}

/// A store opened from another user's or a shared IMAP mailbox.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SharedStore {
    /// The account whose login opens it.
    pub owner: AccountId,
    /// Server name of the mailbox root ("Shared/support").
    pub root: String,
    pub delimiter: Option<char>,
    pub kind: SharedKind,
}

/// A shared mailbox the server offers to an account (found by send/receive).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SharedRootInfo {
    pub root: String,
    /// Name below the namespace prefix ("support", "pat@example.com").
    pub name: String,
    pub delimiter: Option<char>,
    pub kind: SharedKind,
}

/// Where a contact or task lives on a CardDAV / CalDAV server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RemoteRef {
    /// Absolute URL of the resource.
    pub url: String,
    /// Entity tag of the server copy last seen (quotes included).
    pub etag: String,
    /// vCard / iCalendar UID.
    pub uid: String,
    /// Fingerprint of the item as last synchronised: a different one means it changed here since.
    pub synced: u64,
}

/// A server collection mapped to a local Contacts or Tasks folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DavFolderState {
    /// Absolute collection URL.
    pub collection: String,
    /// Resource URL → ETag as of the last synchronisation (tells deletions here from additions there).
    pub known: std::collections::BTreeMap<String, String>,
}

impl Account {
    /// True when the account can send (has an outgoing server or is a JMAP/Exchange account).
    pub fn can_send(&self) -> bool {
        if self.shared.is_some() {
            return false;
        }
        match self.kind {
            AccountKind::Local => false,
            AccountKind::Imap => self.outgoing.is_some(),
            AccountKind::Jmap | AccountKind::Exchange => true,
        }
    }
}

/// What kind of secret an account logs in with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialKind {
    /// User name and password.
    #[default]
    Password,
    /// An OAuth 2.0 access token (XOAUTH2 / OAUTHBEARER).
    OAuthToken,
}

/// Per-folder server state remembered between syncs (IMAP: RFC 9051 UIDVALIDITY/UIDNEXT, RFC 7162 MODSEQ).
/// The UIDs held locally are the `remote_id`s of the folder's items.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteFolderState {
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub highest_modseq: u64,
}

/// A change made locally that still has to reach the server. Folder names are server (IMAP) names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteOp {
    /// Add and remove flags (`\Seen`, `\Flagged`, `\Answered`, `$Forwarded`) on one message.
    Flags {
        folder: String,
        uid: u32,
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// Move one message; the local item gets the new UID when the server reports it.
    Move {
        folder: String,
        uid: u32,
        dest: String,
        item: crate::ItemId,
    },
    /// Upload a local item (its source) into a server folder.
    Append {
        folder: String,
        item: crate::ItemId,
        flags: Vec<String>,
    },
    /// Remove one message permanently.
    Expunge {
        folder: String,
        uid: u32,
    },
    CreateFolder {
        name: String,
    },
    RenameFolder {
        from: String,
        to: String,
    },
    DeleteFolder {
        name: String,
    },
}

/// A queued [`RemoteOp`] with its retry state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOp {
    pub id: u64,
    pub op: RemoteOp,
    /// Failed attempts so far.
    #[serde(default)]
    pub attempts: u32,
    /// Unix time before which automatic retries wait (backoff); user-started Send/Receive ignores it.
    #[serde(default)]
    pub not_before: i64,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// Synchronisation bookkeeping of a server account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountSync {
    /// Server hierarchy delimiter (IMAP), learned from the folder list.
    pub delimiter: Option<char>,
    /// Server state per local folder.
    pub folders: std::collections::BTreeMap<crate::FolderId, RemoteFolderState>,
    /// Local changes waiting for the server, oldest first.
    pub pending: Vec<PendingOp>,
    /// Last id handed to a [`PendingOp`].
    pub next_op: u64,
    /// CardDAV / CalDAV collections mapped to local Contacts and Tasks folders.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub dav: std::collections::BTreeMap<crate::FolderId, DavFolderState>,
    /// Other users' and shared mailboxes the server offers (RFC 2342), as last listed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shared_roots: Vec<SharedRootInfo>,
}
