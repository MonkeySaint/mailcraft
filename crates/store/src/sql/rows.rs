//! Mail rows of `store.db`: encoding an item, and the writes that keep `folder_counts` and the body
//! reference counts in step with the `mail` table (always in the caller's transaction).

use mailcraft_model::{
    FlagState, FocusClass, Importance, MailItem, ResponseState, Sensitivity, message_id_key,
};
use rusqlite::{Connection, OptionalExtension, params};

use super::{from_sql_id, sql_len, to_sql_id};
use crate::packs::Kind;
use crate::{Result, StoreError};

pub const FLAG_READ: i64 = 1;
pub const FLAG_DRAFT: i64 = 1 << 1;
pub const FLAG_PINNED: i64 = 1 << 2;
pub const FLAG_IGNORED: i64 = 1 << 3;
pub const FLAG_HAS_ATTACH: i64 = 1 << 4;
pub const FLAG_NO_AUTO_ARCHIVE: i64 = 1 << 5;
pub const FLAG_FOCUS_OTHER: i64 = 1 << 6;
pub const FLAG_FLAGGED: i64 = 1 << 7;
pub const FLAG_COMPLETE: i64 = 1 << 8;
/// Importance in bits 9–10 (0 low, 1 normal, 2 high).
pub const IMPORTANCE_SHIFT: u32 = 9;
/// Sensitivity in bits 11–12 (0 normal, 1 personal, 2 private, 3 confidential).
pub const SENSITIVITY_SHIFT: u32 = 11;
/// Last response in bits 13–14 (0 none, 1 replied, 2 replied to all, 3 forwarded).
pub const RESPONSE_SHIFT: u32 = 13;

/// The `flags` column of `item`.
pub fn flags_of(item: &MailItem) -> i64 {
    let mut f = 0i64;
    let mut set = |on: bool, bit: i64| {
        if on {
            f |= bit;
        }
    };
    set(item.read, FLAG_READ);
    set(item.is_draft, FLAG_DRAFT);
    set(item.pinned, FLAG_PINNED);
    set(item.ignored, FLAG_IGNORED);
    set(!item.attachments.is_empty(), FLAG_HAS_ATTACH);
    set(item.no_auto_archive, FLAG_NO_AUTO_ARCHIVE);
    set(item.focus == FocusClass::Other, FLAG_FOCUS_OTHER);
    set(matches!(item.flag, FlagState::Flagged { .. }), FLAG_FLAGGED);
    set(matches!(item.flag, FlagState::Complete { .. }), FLAG_COMPLETE);
    let importance: i64 = match item.importance {
        Importance::Low => 0,
        Importance::Normal => 1,
        Importance::High => 2,
    };
    let sensitivity: i64 = match item.sensitivity {
        Sensitivity::Normal => 0,
        Sensitivity::Personal => 1,
        Sensitivity::Private => 2,
        Sensitivity::Confidential => 3,
    };
    let response: i64 = match item.response {
        ResponseState::None => 0,
        ResponseState::Replied(_) => 1,
        ResponseState::RepliedAll(_) => 2,
        ResponseState::Forwarded(_) => 3,
    };
    f | (importance << IMPORTANCE_SHIFT) | (sensitivity << SENSITIVITY_SHIFT) | (response << RESPONSE_SHIFT)
}

/// The folded sender of `item` for sorting by From (display name, else address).
pub fn sender_key(item: &MailItem) -> Option<String> {
    let m = item.from.as_ref().or(item.sender.as_ref())?;
    let name = m.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(m.addr.as_str());
    Some(name.chars().take(128).flat_map(char::to_lowercase).collect())
}

/// The folded subject of `item` for sorting by Subject (reply and forward prefixes dropped).
pub fn subject_key(item: &MailItem) -> String {
    mailcraft_model::subject::conversation_topic(&item.subject).chars().take(256).collect()
}

/// The JSON `item` is stored as.
pub fn encode(item: &MailItem) -> Result<String> {
    serde_json::to_string(item).map_err(|e| StoreError::Corrupt(format!("cannot encode mail item: {e}")))
}

/// The item stored in row `id` as `meta` (its id is the row's, whatever the JSON says).
pub fn decode(id: i64, meta: &str) -> Result<MailItem> {
    let mut item: MailItem = serde_json::from_str(meta)
        .map_err(|e| StoreError::Corrupt(format!("mail row {}: {e}", from_sql_id(id))))?;
    item.id = mailcraft_model::ItemId(from_sql_id(id));
    Ok(item)
}

/// The `bodies` row of entry `key` of `kind` (`None` when the content is not in the packs).
fn body_row(conn: &Connection, kind: Kind, key: Option<&str>) -> Result<Option<i64>> {
    let Some(key) = key else { return Ok(None) };
    Ok(conn
        .prepare_cached("SELECT id FROM bodies WHERE kind = ?1 AND key = ?2")?
        .query_row(params![kind as i64, key], |r| r.get(0))
        .optional()?)
}

/// What the counts and reference counts need of a stored row.
struct Old {
    folder: i64,
    read: bool,
    src: Option<i64>,
    rec: Option<i64>,
}

fn old_row(conn: &Connection, id: i64) -> Result<Option<Old>> {
    Ok(conn
        .prepare_cached("SELECT folder, flags, src, rec FROM mail WHERE id = ?1")?
        .query_row([id], |r| {
            Ok(Old {
                folder: r.get(0)?,
                read: r.get::<_, i64>(1)? & FLAG_READ != 0,
                src: r.get(2)?,
                rec: r.get(3)?,
            })
        })
        .optional()?)
}

fn count_in(conn: &Connection, folder: i64, read: bool) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO folder_counts(folder, total, unread) VALUES (?1, 1, ?2) \
         ON CONFLICT(folder) DO UPDATE SET total = total + 1, unread = unread + excluded.unread",
    )?
    .execute(params![folder, i64::from(!read)])?;
    Ok(())
}

fn count_out(conn: &Connection, folder: i64, read: bool) -> Result<()> {
    conn.prepare_cached(
        "UPDATE folder_counts SET total = max(total - 1, 0), unread = max(unread - ?2, 0) WHERE folder = ?1",
    )?
    .execute(params![folder, i64::from(!read)])?;
    Ok(())
}

fn add_ref(conn: &Connection, body: Option<i64>, delta: i64) -> Result<()> {
    if let Some(b) = body {
        conn.prepare_cached("UPDATE bodies SET refs = max(refs + ?2, 0) WHERE id = ?1")?
            .execute(params![b, delta])?;
    }
    Ok(())
}

/// Stores `item` (encoded as `meta`) in its row, replacing what was there, and moves the counts and
/// reference counts with it. Returns whether a row was replaced.
pub fn put(conn: &Connection, item: &MailItem, meta: &str) -> Result<bool> {
    let id = to_sql_id(item.id.0);
    let folder = to_sql_id(item.folder.0);
    let old = old_row(conn, id)?;
    let src = body_row(conn, Kind::Source, item.blob.as_deref())?;
    let rec = body_row(conn, Kind::Body, item.body_key.as_deref())?;
    let msgid = item.message_id.as_deref().map(message_id_key).filter(|k| !k.is_empty());
    conn.prepare_cached(
        "INSERT INTO mail(id, folder, received, sent, size, flags, thread, sender_key, subject_key, msgid_key, \
                          remote_id, src, rec, meta) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT(id) DO UPDATE SET folder = excluded.folder, received = excluded.received, \
           sent = excluded.sent, size = excluded.size, flags = excluded.flags, thread = excluded.thread, \
           sender_key = excluded.sender_key, subject_key = excluded.subject_key, msgid_key = excluded.msgid_key, \
           remote_id = excluded.remote_id, src = excluded.src, rec = excluded.rec, meta = excluded.meta",
    )?
    .execute(params![
        id,
        folder,
        item.received.unix,
        item.sent.map(|d| d.unix),
        sql_len(item.size),
        flags_of(item),
        item.conversation.map(to_sql_id),
        sender_key(item),
        subject_key(item),
        msgid,
        item.remote_id.as_deref(),
        src,
        rec,
        meta,
    ])?;
    match &old {
        Some(o) => {
            if o.folder != folder || o.read != item.read {
                count_out(conn, o.folder, o.read)?;
                count_in(conn, folder, item.read)?;
            }
            if o.src != src {
                add_ref(conn, o.src, -1)?;
                add_ref(conn, src, 1)?;
            }
            if o.rec != rec {
                add_ref(conn, o.rec, -1)?;
                add_ref(conn, rec, 1)?;
            }
        }
        None => {
            count_in(conn, folder, item.read)?;
            add_ref(conn, src, 1)?;
            add_ref(conn, rec, 1)?;
        }
    }
    Ok(old.is_some())
}

/// Deletes row `id` and takes it out of the counts. Returns whether there was one.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    let Some(old) = old_row(conn, id)? else { return Ok(false) };
    conn.prepare_cached("DELETE FROM mail WHERE id = ?1")?.execute([id])?;
    count_out(conn, old.folder, old.read)?;
    add_ref(conn, old.src, -1)?;
    add_ref(conn, old.rec, -1)?;
    Ok(true)
}

/// The stored JSON of row `id`.
pub fn meta(conn: &Connection, id: i64) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached("SELECT meta FROM mail WHERE id = ?1")?
        .query_row([id], |r| r.get(0))
        .optional()?)
}

/// Rebuilds `folder_counts` and the body reference counts from the rows (repair; tests compare them).
pub fn recount(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM folder_counts;
         INSERT INTO folder_counts(folder, total, unread)
           SELECT folder, count(*), sum(1 - (flags & 1)) FROM mail GROUP BY folder;
         UPDATE bodies SET refs = 0;
         UPDATE bodies SET refs = refs + t.n
           FROM (SELECT src AS b, count(*) AS n FROM mail WHERE src IS NOT NULL GROUP BY src) AS t
           WHERE bodies.id = t.b;
         UPDATE bodies SET refs = refs + t.n
           FROM (SELECT rec AS b, count(*) AS n FROM mail WHERE rec IS NOT NULL GROUP BY rec) AS t
           WHERE bodies.id = t.b;",
    )?;
    Ok(())
}
