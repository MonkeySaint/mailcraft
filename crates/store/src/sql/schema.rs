//! Schema of `store.db` (storage ADR §3.2) and its migrations.
//!
//! Version 1 (`PRAGMA user_version=1`, `PRAGMA application_id` = [`APPLICATION_ID`]):
//! - `meta`: next item id, store generation, the version that created the file.
//! - `config`: one JSON row per configuration part (accounts without their sync state, categories, rules,
//!   quick steps, views, signatures, signature defaults, recent recipients, dictionary, junk lists).
//! - `account_sync`: each account's synchronisation state (server folder states, the queue of server
//!   changes), one JSON row per account, so a flag change on synchronised mail writes one small row.
//! - `folders`: one row per folder.
//! - `folder_counts`: total and unread mail per folder, kept in the same transaction as every mail write.
//! - `mail`: one row per mail item. `meta` holds the whole item as JSON (what is read back); the other
//!   columns are derived from it for the indexes: folder, received/sent (Unix seconds), size, flag bits
//!   ([`super::rows::flags_of`]), conversation, folded sender and subject keys, the normalised Message-ID key
//!   ([`mailcraft_model::message_id_key`]), server id, and the `bodies` rows of its source (`src`) and body
//!   record (`rec`).
//! - `bodies`: the content index of the pack files: per entry its kind (0 source, 1 body record), key (the
//!   item's `blob` / `body_key`), SHA-256 (de-duplication), lengths, place in a pack, codec and reference
//!   count (mail rows pointing at it).
//! - `packs`: per pack file its committed length (the crash rule: bytes past it are cut off on open), live
//!   and dead bytes.
//! - `pim`: calendar, contact, task and note items, one JSON row each (`kind` 0 appointment, 1 contact,
//!   2 task, 3 note).
//!
//! The tables later stages fill (threads by Message-ID, full-text index, recoverable items, per-folder sync
//! rows, the undo spill) are added by those stages as schema migrations.

use rusqlite::Connection;

use crate::{Result, StoreError};

/// Schema version written now.
pub const VERSION: i64 = 1;
/// `PRAGMA application_id` of a MailCraft store ("MCST").
pub const APPLICATION_ID: i64 = 0x4D43_5354;

const V1: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value BLOB);
CREATE TABLE config(key TEXT PRIMARY KEY, json TEXT NOT NULL);
CREATE TABLE account_sync(account INTEGER PRIMARY KEY, json TEXT NOT NULL);
CREATE TABLE folders(id INTEGER PRIMARY KEY, account INT NOT NULL, parent INT, json TEXT NOT NULL);
CREATE TABLE folder_counts(folder INTEGER PRIMARY KEY, total INT NOT NULL, unread INT NOT NULL);
CREATE TABLE packs(id INTEGER PRIMARY KEY, name TEXT NOT NULL, committed_len INT NOT NULL,
                   live INT NOT NULL DEFAULT 0, dead INT NOT NULL DEFAULT 0);
CREATE TABLE bodies(id INTEGER PRIMARY KEY, kind INT NOT NULL, key TEXT NOT NULL, sha256 BLOB NOT NULL,
                    raw_len INT NOT NULL, pack INT NOT NULL REFERENCES packs(id), off INT NOT NULL,
                    stored_len INT NOT NULL, codec INT NOT NULL, refs INT NOT NULL DEFAULT 0,
                    UNIQUE(kind, key), UNIQUE(kind, sha256));
CREATE TABLE mail(id INTEGER PRIMARY KEY, folder INT NOT NULL, received INT NOT NULL, sent INT,
                  size INT NOT NULL, flags INT NOT NULL, thread INT, sender_key TEXT, subject_key TEXT,
                  msgid_key TEXT, remote_id TEXT, src INT, rec INT, meta TEXT NOT NULL);
CREATE INDEX mail_by_folder_date ON mail(folder, received DESC, id);
CREATE INDEX mail_by_thread ON mail(thread, received) WHERE thread IS NOT NULL;
CREATE INDEX mail_by_folder_thread ON mail(folder, thread, received);
CREATE INDEX mail_by_remote ON mail(folder, remote_id) WHERE remote_id IS NOT NULL;
CREATE INDEX mail_by_msgid ON mail(msgid_key) WHERE msgid_key IS NOT NULL;
CREATE TABLE pim(kind INT NOT NULL, id INTEGER NOT NULL, folder INT NOT NULL, json TEXT NOT NULL,
                 PRIMARY KEY(kind, id));
";

/// Creates the schema in a new database, or checks an existing one: refused when it belongs to another
/// application or was written by a newer MailCraft (schema past [`VERSION`]).
pub fn migrate(conn: &Connection) -> Result<()> {
    let app: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if app != 0 && app != APPLICATION_ID {
        return Err(StoreError::Corrupt("store.db is not a MailCraft store".into()));
    }
    if version > VERSION {
        return Err(StoreError::Corrupt(format!(
            "store.db was written by a newer MailCraft (schema {version}; this version reads up to {VERSION})"
        )));
    }
    if version == VERSION {
        return Ok(());
    }
    if version == 0 {
        let tables: i64 =
            conn.query_row("SELECT count(*) FROM sqlite_schema WHERE type = 'table'", [], |r| r.get(0))?;
        if tables != 0 {
            return Err(StoreError::Corrupt("store.db holds tables but no MailCraft schema version".into()));
        }
        super::in_savepoint(conn, "schema", |c| {
            c.execute_batch(V1)?;
            c.execute(
                "INSERT INTO meta(key, value) VALUES ('created_by', ?1)",
                [concat!("mailcraft ", env!("CARGO_PKG_VERSION"))],
            )?;
            c.execute_batch(&format!(
                "PRAGMA application_id={APPLICATION_ID}; PRAGMA user_version={VERSION}"
            ))?;
            Ok(())
        })?;
    }
    Ok(())
}
