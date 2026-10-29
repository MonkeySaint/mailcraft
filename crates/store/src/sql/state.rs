//! [`SqlStore`]: a whole profile in `store.db` (storage stage S6). It takes the same [`Op`]s the legacy
//! journal takes and gives back the same [`Mailbox`] (`Op::apply` is the specification; the tests replay
//! random ops through both).
//!
//! Each [`SqlStore::append`] is one transaction (one fsync). Mail goes through [`super::rows`] like
//! [`super::SqlRepo`]'s writes; folders, calendar, contact, task and note items are rows of their own;
//! configuration parts are one JSON row each (the field names of [`SmallState`]) and each account's sync
//! state another, so a flag change on synchronised mail writes the item, its counts and one sync row.
//!
//! A whole-mailbox `Replace` (still journaled for calendar, contact, task and note changes until storage
//! stage S12) writes only the rows that differ: the store keeps a 128-bit digest of every mail and PIM row
//! it holds (about 40 bytes per item) and skips rows whose JSON is unchanged.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, PoisonError};

use mailcraft_model::account::AccountSync;
use mailcraft_model::{AccountId, Folder, FolderId, ItemId, MailItem, Mailbox};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::{Db, from_sql_id, in_savepoint, rows, to_sql_id};
use crate::config::SmallState;
use crate::{Op, Result, StoreError};

/// `pim.kind` of each item map.
pub const PIM_APPOINTMENT: u8 = 0;
pub const PIM_CONTACT: u8 = 1;
pub const PIM_TASK: u8 = 2;
pub const PIM_NOTE: u8 = 3;

type Digest = u128;

fn digest(s: &str) -> Digest {
    let half = |salt: u8| {
        let mut h = DefaultHasher::new();
        salt.hash(&mut h);
        s.hash(&mut h);
        h.finish()
    };
    (u128::from(half(0)) << 64) | u128::from(half(1))
}

fn json<T: Serialize + ?Sized>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|e| StoreError::Corrupt(format!("cannot encode: {e}")))
}

fn parse<T: serde::de::DeserializeOwned>(what: &str, s: &str) -> Result<T> {
    serde_json::from_str(s).map_err(|e| StoreError::Corrupt(format!("store.db {what}: {e}")))
}

/// The whole configuration of `mb` (every part set).
pub fn full_state(mb: &Mailbox) -> SmallState {
    SmallState {
        accounts: Some(mb.accounts.iter().map(mailcraft_model::Account::without_sync).collect()),
        categories: Some(mb.categories.clone()),
        rules: Some(mb.rules.clone()),
        quick_steps: Some(mb.quick_steps.clone()),
        views: Some(mb.views.clone()),
        signatures: Some(mb.signatures.clone()),
        signature_defaults: Some(mb.signature_defaults.clone()),
        recent_recipients: Some(mb.recent_recipients.clone()),
        custom_dictionary: Some(mb.custom_dictionary.clone()),
        junk: Some(mb.junk.clone()),
    }
}

/// Digest changes a batch makes, applied once it commits.
#[derive(Default)]
struct Pending {
    mail: HashMap<ItemId, Option<Digest>>,
    pim: HashMap<(u8, ItemId), Option<Digest>>,
    accounts: Option<BTreeSet<AccountId>>,
    next_id: Option<u64>,
    /// JSON bytes written (for the size cap).
    bytes: u64,
}

/// A profile's mailbox in `store.db`.
#[derive(Debug)]
pub struct SqlStore {
    db: Db,
    known: Mutex<Known>,
}

/// What the store knows of the rows it holds.
#[derive(Debug, Default)]
struct Known {
    mail: HashMap<ItemId, Digest>,
    pim: HashMap<(u8, ItemId), Digest>,
    accounts: BTreeSet<AccountId>,
    next_id: u64,
    generation: u64,
}

impl SqlStore {
    /// The store over `db` (nothing is read until [`SqlStore::load`]).
    pub fn new(db: Db) -> SqlStore {
        SqlStore { db, known: Mutex::new(Known::default()) }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Bumped by every [`SqlStore::write_all`] (the legacy snapshot generation's counterpart).
    pub fn generation(&self) -> u64 {
        self.known().generation
    }

    fn known(&self) -> std::sync::MutexGuard<'_, Known> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reads the whole mailbox.
    pub fn load(&self) -> Result<Mailbox> {
        let conn = self.db.conn();
        let mut mb = Mailbox::default();
        let mut mail_digests = HashMap::new();
        let mut pim_digests = HashMap::new();
        // Configuration: the stored parts by their SmallState names.
        let mut parts = serde_json::Map::new();
        {
            let mut stmt = conn.prepare("SELECT key, json FROM config")?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let key: String = r.get(0)?;
                let text: String = r.get(1)?;
                let value: serde_json::Value = parse(&format!("config {key}"), &text)?;
                parts.insert(key, value);
            }
        }
        let state: SmallState = serde_json::from_value(serde_json::Value::Object(parts))
            .map_err(|e| StoreError::Corrupt(format!("store.db config: {e}")))?;
        state.apply(&mut mb);
        {
            let mut syncs: BTreeMap<AccountId, AccountSync> = BTreeMap::new();
            let mut stmt = conn.prepare("SELECT account, json FROM account_sync")?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                let text: String = r.get(1)?;
                let Ok(id) = u32::try_from(id) else { continue };
                syncs.insert(AccountId(id), parse("account sync state", &text)?);
            }
            // The first of duplicate ids takes the state (as SmallState::apply does).
            for a in &mut mb.accounts {
                if let Some(s) = syncs.remove(&a.id) {
                    a.sync = s;
                }
            }
        }
        {
            let mut stmt = conn.prepare("SELECT json FROM folders")?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let text: String = r.get(0)?;
                let f: Folder = parse("folder", &text)?;
                mb.folders.insert(f.id, f);
            }
        }
        {
            let mut stmt = conn.prepare("SELECT id, meta FROM mail")?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                let meta: String = r.get(1)?;
                let item = rows::decode(id, &meta)?;
                mail_digests.insert(item.id, digest(&meta));
                mb.mail.insert(item.id, item);
            }
        }
        {
            let mut stmt = conn.prepare("SELECT kind, id, json FROM pim")?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let kind: i64 = r.get(0)?;
                let id = ItemId(from_sql_id(r.get(1)?));
                let text: String = r.get(2)?;
                let d = digest(&text);
                let kind = match u8::try_from(kind) {
                    Ok(PIM_APPOINTMENT) => {
                        mb.appointments.insert(id, parse("appointment", &text)?);
                        PIM_APPOINTMENT
                    }
                    Ok(PIM_CONTACT) => {
                        mb.contacts.insert(id, parse("contact", &text)?);
                        PIM_CONTACT
                    }
                    Ok(PIM_TASK) => {
                        mb.tasks.insert(id, parse("task", &text)?);
                        PIM_TASK
                    }
                    Ok(PIM_NOTE) => {
                        mb.notes.insert(id, parse("note", &text)?);
                        PIM_NOTE
                    }
                    _ => {
                        log::warn!(
                            "store.db: skipped a calendar/contact/task/note row of unknown kind {kind}"
                        );
                        continue;
                    }
                };
                pim_digests.insert((kind, id), d);
            }
        }
        let meta_u64 = |key: &str| -> Result<u64> {
            let v: Option<i64> =
                conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?;
            Ok(v.map_or(0, from_sql_id))
        };
        mb.next_id = meta_u64("next_id")?;
        let generation = meta_u64("generation")?;
        drop(conn);
        *self.known() = Known {
            mail: mail_digests,
            pim: pim_digests,
            accounts: mb.accounts.iter().map(|a| a.id).collect(),
            next_id: mb.next_id,
            generation,
        };
        self.db.cache().clear();
        Ok(mb)
    }

    /// Writes `ops` in one transaction. Nothing is written when one of them fails, or when the JSON written
    /// would pass `cap` bytes ([`StoreError::TooLarge`]).
    pub fn append(&mut self, ops: &[Op], cap: u64) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let name = self.db.savepoint_name("ops");
        let mut pending = Pending::default();
        let known = self.known.get_mut().unwrap_or_else(PoisonError::into_inner);
        let res = {
            let conn = self.db.conn();
            in_savepoint(&conn, &name, |c| {
                for op in ops {
                    known.apply(c, op, &mut pending)?;
                }
                known.finish(c, &pending, cap)
            })
        };
        self.db.cache().clear();
        res?;
        known.absorb(pending);
        Ok(())
    }

    /// Writes `mb` as the whole state (rows that did not change are skipped) and bumps the generation.
    pub fn write_all(&mut self, mb: &Mailbox, cap: u64) -> Result<()> {
        let name = self.db.savepoint_name("all");
        let mut pending = Pending::default();
        let known = self.known.get_mut().unwrap_or_else(PoisonError::into_inner);
        let generation = known.generation.saturating_add(1);
        let res = {
            let conn = self.db.conn();
            in_savepoint(&conn, &name, |c| {
                known.replace(c, mb, &mut pending)?;
                set_meta(c, "generation", generation)?;
                known.finish(c, &pending, cap)
            })
        };
        self.db.cache().clear();
        res?;
        known.absorb(pending);
        known.generation = generation;
        Ok(())
    }
}

impl Known {
    fn finish(&self, c: &Connection, p: &Pending, cap: u64) -> Result<()> {
        if p.bytes > cap {
            return Err(StoreError::TooLarge(format!(
                "the changes take {} bytes, past the {cap} byte cap of one write",
                p.bytes
            )));
        }
        if let Some(n) = p.next_id
            && n != self.next_id
        {
            set_meta(c, "next_id", n)?;
        }
        Ok(())
    }

    fn absorb(&mut self, p: Pending) {
        for (id, d) in p.mail {
            match d {
                Some(d) => self.mail.insert(id, d),
                None => self.mail.remove(&id),
            };
        }
        for (k, d) in p.pim {
            match d {
                Some(d) => self.pim.insert(k, d),
                None => self.pim.remove(&k),
            };
        }
        if let Some(a) = p.accounts {
            self.accounts = a;
        }
        if let Some(n) = p.next_id {
            self.next_id = n;
        }
    }

    /// The mail digest as of this batch (its own earlier writes first).
    fn mail_digest(&self, p: &Pending, id: ItemId) -> Option<Digest> {
        match p.mail.get(&id) {
            Some(d) => *d,
            None => self.mail.get(&id).copied(),
        }
    }

    fn accounts_now<'a>(&'a self, p: &'a Pending) -> &'a BTreeSet<AccountId> {
        p.accounts.as_ref().unwrap_or(&self.accounts)
    }

    fn next_id_now(&self, p: &Pending) -> u64 {
        p.next_id.unwrap_or(self.next_id)
    }

    fn put_mail(&self, c: &Connection, item: &MailItem, p: &mut Pending, check: bool) -> Result<()> {
        let meta = rows::encode(item)?;
        let d = digest(&meta);
        if check && self.mail_digest(p, item.id) == Some(d) {
            return Ok(());
        }
        p.bytes = p.bytes.saturating_add(meta.len() as u64);
        rows::put(c, item, &meta)?;
        p.mail.insert(item.id, Some(d));
        Ok(())
    }

    fn apply(&self, c: &Connection, op: &Op, p: &mut Pending) -> Result<()> {
        match op {
            Op::PutMail(item) => {
                self.put_mail(c, item, p, true)?;
                p.next_id = Some(self.next_id_now(p).max(item.id.0));
            }
            Op::RemoveMail(id) => {
                rows::delete(c, to_sql_id(id.0))?;
                p.mail.insert(*id, None);
            }
            Op::PutFolder(f) => {
                put_folder(c, f, p)?;
                p.next_id = Some(self.next_id_now(p).max(f.id.0));
            }
            Op::RemoveFolder(id) => {
                c.prepare_cached("DELETE FROM folders WHERE id = ?1")?.execute([to_sql_id(id.0)])?;
                // As Op::apply: the folder's view settings go with it.
                let views: Option<BTreeMap<FolderId, mailcraft_model::ViewSettings>> =
                    config_part(c, "views")?;
                if let Some(mut views) = views
                    && views.remove(id).is_some()
                {
                    put_part(c, "views", &json(&views)?, p)?;
                }
            }
            Op::PutAccount(account) => {
                // The whole account, sync state included, matched by id (appended when missing).
                let mut accounts: Vec<mailcraft_model::Account> =
                    config_part(c, "accounts")?.unwrap_or_default();
                let bare = account.without_sync();
                match accounts.iter_mut().find(|a| a.id == account.id) {
                    Some(a) => *a = bare,
                    None => accounts.push(bare),
                }
                put_part(c, "accounts", &json(&accounts)?, p)?;
                put_sync(c, account.id, &account.sync, p)?;
                let mut ids = self.accounts_now(p).clone();
                ids.insert(account.id);
                p.accounts = Some(ids);
            }
            Op::Replace(mb) => self.replace(c, mb, p)?,
            Op::PutSync(id, sync) => {
                if self.accounts_now(p).contains(id) {
                    put_sync(c, *id, sync, p)?;
                } else {
                    log::warn!("store.db: sync state for account {}, which is not there; skipped", id.0);
                }
            }
            Op::PutConfig(state) => self.put_config(c, state, p)?,
        }
        Ok(())
    }

    fn put_config(&self, c: &Connection, state: &SmallState, p: &mut Pending) -> Result<()> {
        let value =
            serde_json::to_value(state).map_err(|e| StoreError::Corrupt(format!("cannot encode: {e}")))?;
        let serde_json::Value::Object(parts) = value else {
            return Err(StoreError::Corrupt("configuration is not an object".into()));
        };
        for (key, v) in &parts {
            put_part(c, key, &json(v)?, p)?;
        }
        if let Some(accounts) = &state.accounts {
            // Accounts in both lists keep their sync state; one new to the list starts without any.
            let before = self.accounts_now(p).clone();
            let now: BTreeSet<AccountId> = accounts.iter().map(|a| a.id).collect();
            let mut stmt = c.prepare_cached("DELETE FROM account_sync WHERE account = ?1")?;
            let mut known: Vec<i64> = Vec::new();
            {
                let mut q = c.prepare_cached("SELECT account FROM account_sync")?;
                let mut rows = q.query([])?;
                while let Some(r) = rows.next()? {
                    known.push(r.get(0)?);
                }
            }
            for k in known {
                let keep = u32::try_from(k)
                    .is_ok_and(|id| before.contains(&AccountId(id)) && now.contains(&AccountId(id)));
                if !keep {
                    stmt.execute([k])?;
                }
            }
            p.accounts = Some(now);
        }
        Ok(())
    }

    fn replace(&self, c: &Connection, mb: &Mailbox, p: &mut Pending) -> Result<()> {
        // Exhaustive on purpose: a new Mailbox field fails to compile here until it is stored.
        let Mailbox {
            accounts,
            folders,
            mail,
            appointments,
            contacts,
            tasks,
            notes,
            categories: _,
            rules: _,
            quick_steps: _,
            views: _,
            next_id,
            signatures: _,
            signature_defaults: _,
            recent_recipients: _,
            custom_dictionary: _,
            junk: _,
        } = mb;
        // Configuration (the parts named `_` above) as a whole.
        c.execute("DELETE FROM config", [])?;
        let state = full_state(mb);
        let value =
            serde_json::to_value(&state).map_err(|e| StoreError::Corrupt(format!("cannot encode: {e}")))?;
        if let serde_json::Value::Object(parts) = value {
            for (key, v) in &parts {
                put_part(c, key, &json(v)?, p)?;
            }
        }
        c.execute("DELETE FROM account_sync", [])?;
        let mut seen = BTreeSet::new();
        for a in accounts {
            if seen.insert(a.id) {
                put_sync(c, a.id, &a.sync, p)?;
            }
        }
        p.accounts = Some(seen);
        c.execute("DELETE FROM folders", [])?;
        for f in folders.values() {
            put_folder(c, f, p)?;
        }
        // Mail: changed rows only, then the rows that are gone.
        let mut keep: BTreeSet<ItemId> = BTreeSet::new();
        for item in mail.items() {
            keep.insert(item.id);
            self.put_mail(c, item, p, true)?;
        }
        let stored: BTreeSet<ItemId> = {
            let mut ids: BTreeSet<ItemId> = self.mail.keys().copied().collect();
            for (id, d) in &p.mail {
                match d {
                    Some(_) => ids.insert(*id),
                    None => ids.remove(id),
                };
            }
            ids
        };
        for id in stored.difference(&keep) {
            rows::delete(c, to_sql_id(id.0))?;
            p.mail.insert(*id, None);
        }
        // Calendar, contacts, tasks, notes.
        let mut pim_keep: BTreeSet<(u8, ItemId)> = BTreeSet::new();
        let pim_now = |k: (u8, ItemId), p: &Pending| match p.pim.get(&k) {
            Some(d) => *d,
            None => self.pim.get(&k).copied(),
        };
        let mut put_pim = |kind: u8,
                           id: ItemId,
                           folder: FolderId,
                           text: String,
                           p: &mut Pending|
         -> Result<()> {
            let d = digest(&text);
            pim_keep.insert((kind, id));
            if pim_now((kind, id), p) == Some(d) {
                return Ok(());
            }
            p.bytes = p.bytes.saturating_add(text.len() as u64);
            c.prepare_cached("INSERT OR REPLACE INTO pim(kind, id, folder, json) VALUES (?1, ?2, ?3, ?4)")?
                .execute(params![i64::from(kind), to_sql_id(id.0), to_sql_id(folder.0), text])?;
            p.pim.insert((kind, id), Some(d));
            Ok(())
        };
        for (id, a) in appointments {
            put_pim(PIM_APPOINTMENT, *id, a.folder, json(a)?, p)?;
        }
        for (id, x) in contacts {
            put_pim(PIM_CONTACT, *id, x.folder, json(x)?, p)?;
        }
        for (id, t) in tasks {
            put_pim(PIM_TASK, *id, t.folder, json(t)?, p)?;
        }
        for (id, n) in notes {
            put_pim(PIM_NOTE, *id, n.folder, json(n)?, p)?;
        }
        let mut stored_pim: BTreeSet<(u8, ItemId)> = self.pim.keys().copied().collect();
        for (k, d) in &p.pim {
            match d {
                Some(_) => stored_pim.insert(*k),
                None => stored_pim.remove(k),
            };
        }
        for k in stored_pim.difference(&pim_keep) {
            c.prepare_cached("DELETE FROM pim WHERE kind = ?1 AND id = ?2")?
                .execute(params![i64::from(k.0), to_sql_id(k.1.0)])?;
            p.pim.insert(*k, None);
        }
        p.next_id = Some(*next_id);
        if *next_id == self.next_id {
            // `finish` writes next_id only when it changed; a Replace must also set it when the row is
            // missing (a new store).
            set_meta(c, "next_id", *next_id)?;
        }
        Ok(())
    }
}

fn set_meta(c: &Connection, key: &str, v: u64) -> Result<()> {
    c.prepare_cached("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)")?
        .execute(params![key, to_sql_id(v)])?;
    Ok(())
}

fn put_part(c: &Connection, key: &str, text: &str, p: &mut Pending) -> Result<()> {
    p.bytes = p.bytes.saturating_add(text.len() as u64);
    c.prepare_cached("INSERT OR REPLACE INTO config(key, json) VALUES (?1, ?2)")?
        .execute(params![key, text])?;
    Ok(())
}

fn config_part<T: serde::de::DeserializeOwned>(c: &Connection, key: &str) -> Result<Option<T>> {
    let text: Option<String> = c
        .prepare_cached("SELECT json FROM config WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()?;
    text.map(|t| parse(&format!("config {key}"), &t)).transpose()
}

fn put_sync(c: &Connection, id: AccountId, sync: &AccountSync, p: &mut Pending) -> Result<()> {
    let text = json(sync)?;
    p.bytes = p.bytes.saturating_add(text.len() as u64);
    c.prepare_cached("INSERT OR REPLACE INTO account_sync(account, json) VALUES (?1, ?2)")?
        .execute(params![i64::from(id.0), text])?;
    Ok(())
}

fn put_folder(c: &Connection, f: &Folder, p: &mut Pending) -> Result<()> {
    let text = json(f)?;
    p.bytes = p.bytes.saturating_add(text.len() as u64);
    c.prepare_cached("INSERT OR REPLACE INTO folders(id, account, parent, json) VALUES (?1, ?2, ?3, ?4)")?
        .execute(params![
            to_sql_id(f.id.0),
            i64::from(f.account.0),
            f.parent.map(|x| to_sql_id(x.0)),
            text
        ])?;
    Ok(())
}
