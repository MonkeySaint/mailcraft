//! Exchange-only services the commands use directly (one request each, on the calling thread, with short
//! timeouts): automatic replies ([MS-OXWSOOF]), other people's free/busy time for the Scheduling Assistant
//! ([MS-OXWAVLS]) and meeting answers recorded and sent by the server ([MS-OXWSMTGS]).

use std::time::Duration;

use mailcraft_model::{AccountId, AccountKind};
use mailcraft_net::ews::{BusyBlock, EwsClient, MeetingReply, OofSettings};

use super::native::{Incoming, conn_settings};
use crate::Session;

/// The Exchange account to use: `account` when it is one, else the current or first Exchange account.
pub(crate) fn exchange_account(s: &Session, account: Option<AccountId>) -> Option<AccountId> {
    let is_ex = |id: AccountId| {
        s.mailbox.account(id).is_some_and(|a| a.kind == AccountKind::Exchange && a.incoming.is_some())
    };
    match account {
        Some(a) => is_ex(a).then_some(a),
        None => s
            .current_account()
            .filter(|a| is_ex(*a))
            .or_else(|| s.mailbox.accounts.iter().find(|a| is_ex(a.id)).map(|a| a.id)),
    }
}

/// How to reach `account` (an Exchange account), with timeouts fit for a command: built on the session's
/// thread, usable on any.
pub(crate) fn conn(s: &Session, account: AccountId) -> Result<super::ews::EwsConn, String> {
    let a = s.mailbox.account(account).ok_or("no such account")?;
    let mut conn = conn_settings(s, a)?;
    let Incoming::Ews(e) = &mut conn.incoming else { return Err("not an Exchange account".into()) };
    let mut c = e.clone();
    c.limit_timeouts(Duration::from_secs(15), Duration::from_secs(30));
    Ok(c)
}

/// A client for `account` (an Exchange account), with timeouts fit for a command.
fn client(s: &Session, account: AccountId) -> Result<(EwsClient, String), String> {
    let c = conn(s, account)?;
    let address = c.address().to_owned();
    Ok((c.client().map_err(|e| e.to_string())?, address))
}

pub(crate) fn oof(s: &Session, account: AccountId) -> Result<OofSettings, String> {
    let (c, address) = client(s, account)?;
    c.oof(&address).map_err(|e| e.to_string())
}

pub(crate) fn set_oof(s: &Session, account: AccountId, o: &OofSettings) -> Result<(), String> {
    let (c, address) = client(s, account)?;
    c.set_oof(&address, o).map_err(|e| e.to_string())
}

/// The busy times of `addresses` between `start` and `end` (UTC `YYYY-MM-DDThh:mm:ss`).
pub(crate) fn availability(
    conn: &super::ews::EwsConn,
    addresses: &[String],
    start: &str,
    end: &str,
) -> Result<Vec<Result<Vec<BusyBlock>, String>>, String> {
    let c = conn.client().map_err(|e| e.to_string())?;
    let r = c.availability(addresses, start, end).map_err(|e| e.to_string())?;
    Ok(r.into_iter().map(|x| x.map_err(|e| e.to_string())).collect())
}

/// Answers meeting request `item` (its EWS item id) on the server; `send` mails the organizer.
pub(crate) fn reply_to_meeting(
    s: &Session,
    account: AccountId,
    item: &str,
    reply: MeetingReply,
    send: bool,
    comment: &str,
) -> Result<(), String> {
    let (c, _) = client(s, account)?;
    c.reply_to_meeting(reply, item, "", send, comment).map_err(|e| e.to_string())
}
