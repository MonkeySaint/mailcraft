//! The configuration part of a mailbox, journaled on its own (storage stage S4, `plan/storage-adr.md` §3.2):
//! the accounts without their sync state, categories, rules, quick steps, views, signatures, Auto-Complete,
//! the dictionary and the junk lists. [`crate::Op::PutConfig`] carries only the parts that changed, and an
//! account's sync state (its queue of server changes among it) travels in [`crate::Op::PutSync`], so neither
//! a new category nor a flag change on synchronised mail journals the whole mailbox.

use std::collections::BTreeMap;

use mailcraft_model::account::AccountSync;
use mailcraft_model::{
    Account, AccountId, AccountSignatures, Category, FolderId, JunkSettings, Mailbox, QuickStep,
    RecentRecipient, Rule, Signature, ViewSettings,
};
use serde::{Deserialize, Serialize};

/// The configuration parts that changed, each one whole; a part left out (`None`) stays as it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmallState {
    /// The account list in its order, without sync state (`sync` is empty here and ignored when applied:
    /// each account keeps its own, and an account new to the list starts with none; [`crate::Op::PutSync`]
    /// sets it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<Account>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<Category>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<Rule>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quick_steps: Option<Vec<QuickStep>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub views: Option<BTreeMap<FolderId, ViewSettings>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signatures: Option<Vec<Signature>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_defaults: Option<Vec<AccountSignatures>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_recipients: Option<Vec<RecentRecipient>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_dictionary: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub junk: Option<JunkSettings>,
}

/// `Some(after)` when it differs from `before`.
fn part<T: PartialEq + Clone>(before: &T, after: &T) -> Option<T> {
    (before != after).then(|| after.clone())
}

impl SmallState {
    /// The configuration parts that differ from `before` to `after`, as they are in `after`; `None` when they
    /// are all the same. Accounts are compared without their sync state.
    pub fn changed(before: &Mailbox, after: &Mailbox) -> Option<SmallState> {
        // Exhaustive on purpose: a new Mailbox field fails to compile here until it is placed.
        let Mailbox {
            accounts,
            // Journaled per entry (PutFolder / RemoveFolder, PutMail / RemoveMail) or kept ahead by them.
            folders: _,
            mail: _,
            next_id: _,
            // Not configuration: journaled as a whole-mailbox Replace until storage stage S12.
            appointments: _,
            contacts: _,
            tasks: _,
            notes: _,
            categories,
            rules,
            quick_steps,
            views,
            signatures,
            signature_defaults,
            recent_recipients,
            custom_dictionary,
            junk,
        } = after;
        let accounts_same = before.accounts.len() == accounts.len()
            && before.accounts.iter().zip(accounts).all(|(b, a)| b.eq_except_sync(a));
        let state = SmallState {
            accounts: (!accounts_same).then(|| accounts.iter().map(Account::without_sync).collect()),
            categories: part(&before.categories, categories),
            rules: part(&before.rules, rules),
            quick_steps: part(&before.quick_steps, quick_steps),
            views: part(&before.views, views),
            signatures: part(&before.signatures, signatures),
            signature_defaults: part(&before.signature_defaults, signature_defaults),
            recent_recipients: part(&before.recent_recipients, recent_recipients),
            custom_dictionary: part(&before.custom_dictionary, custom_dictionary),
            junk: part(&before.junk, junk),
        };
        (!state.is_empty()).then_some(state)
    }

    /// Whether no part is set.
    pub fn is_empty(&self) -> bool {
        let SmallState {
            accounts,
            categories,
            rules,
            quick_steps,
            views,
            signatures,
            signature_defaults,
            recent_recipients,
            custom_dictionary,
            junk,
        } = self;
        accounts.is_none()
            && categories.is_none()
            && rules.is_none()
            && quick_steps.is_none()
            && views.is_none()
            && signatures.is_none()
            && signature_defaults.is_none()
            && recent_recipients.is_none()
            && custom_dictionary.is_none()
            && junk.is_none()
    }

    /// Puts the parts that are set into `mb`. Each account in a new list keeps the sync state it had (matched
    /// by id; the first of duplicate ids takes it); an account new to the list starts without any.
    pub fn apply(self, mb: &mut Mailbox) {
        let SmallState {
            accounts,
            categories,
            rules,
            quick_steps,
            views,
            signatures,
            signature_defaults,
            recent_recipients,
            custom_dictionary,
            junk,
        } = self;
        if let Some(accounts) = accounts {
            let mut syncs: BTreeMap<AccountId, AccountSync> = BTreeMap::new();
            for a in std::mem::take(&mut mb.accounts) {
                syncs.entry(a.id).or_insert(a.sync);
            }
            mb.accounts = accounts
                .into_iter()
                .map(|mut a| {
                    a.sync = syncs.remove(&a.id).unwrap_or_default();
                    a
                })
                .collect();
        }
        if let Some(v) = categories {
            mb.categories = v;
        }
        if let Some(v) = rules {
            mb.rules = v;
        }
        if let Some(v) = quick_steps {
            mb.quick_steps = v;
        }
        if let Some(v) = views {
            mb.views = v;
        }
        if let Some(v) = signatures {
            mb.signatures = v;
        }
        if let Some(v) = signature_defaults {
            mb.signature_defaults = v;
        }
        if let Some(v) = recent_recipients {
            mb.recent_recipients = v;
        }
        if let Some(v) = custom_dictionary {
            mb.custom_dictionary = v;
        }
        if let Some(v) = junk {
            mb.junk = v;
        }
    }
}

/// Sets the sync state of the account with `id` (the first, should ids repeat). An account the mailbox
/// lacks is skipped: the op that removed it came later in the same journal, or the journal was cut.
pub(crate) fn put_sync(mb: &mut Mailbox, id: AccountId, sync: AccountSync) {
    match mb.accounts.iter_mut().find(|a| a.id == id) {
        Some(a) => a.sync = sync,
        None => log::warn!("journal sets the sync state of account {}, which is not there; skipped", id.0),
    }
}
