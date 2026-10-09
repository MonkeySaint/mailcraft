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
