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
    /// Shown as the mailbox root label (Outlook uses the address).
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
}

impl Account {
    /// True when the account can send (has an outgoing server or is a JMAP/Exchange account).
    pub fn can_send(&self) -> bool {
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
}
