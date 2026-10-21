//! Command implementations, one module per area. Each module exposes `specs()`.
//!
//! Ownership for parallel work: core (app, nav), mail + respond, folder/view/search,
//! compose/format/insert/review, modules (calendar/people/tasks/notes), admin
//! (rules/quicksteps/categories/sendReceive/file/options/help).

pub mod app;
pub mod calendar;
pub mod categories;
pub mod compose;
pub mod file;
pub mod folder;
pub mod format;
pub mod help;
pub mod insert;
pub mod mail;
pub mod nav;
pub mod notes;
pub mod options;
pub mod people;
pub mod print;
pub mod quicksteps;
pub mod respond;
pub mod review;
pub mod rules;
pub mod search;
pub mod send_receive;
pub mod tasks;
pub mod util;
pub mod view;

use crate::Registry;

/// Every module's specs, concatenated and unsorted, before [`Registry::new`] sorts and dedups them.
/// Ids must be unique across modules (asserted by `registry_ids_are_unique`).
pub fn all_specs() -> Vec<crate::CommandSpec> {
    let mut v = Vec::new();
    v.extend(app::specs());
    v.extend(nav::specs());
    v.extend(mail::specs());
    v.extend(respond::specs());
    v.extend(folder::specs());
    v.extend(view::specs());
    v.extend(search::specs());
    v.extend(compose::specs());
    v.extend(format::specs());
    v.extend(insert::specs());
    v.extend(review::specs());
    v.extend(calendar::specs());
    v.extend(people::specs());
    v.extend(tasks::specs());
    v.extend(notes::specs());
    v.extend(rules::specs());
    v.extend(quicksteps::specs());
    v.extend(categories::specs());
    v.extend(send_receive::specs());
    v.extend(file::specs());
    v.extend(print::specs());
    v.extend(options::specs());
    v.extend(help::specs());
    v.extend(crate::archive::specs());
    v.extend(crate::smime::specs());
    v
}

/// The full registry.
pub fn registry() -> Registry {
    Registry::new(all_specs())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    #[test]
    fn registry_ids_are_unique() {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for s in super::all_specs() {
            *seen.entry(s.id).or_default() += 1;
        }
        let dups: Vec<_> = seen.iter().filter(|(_, n)| **n > 1).collect();
        assert!(dups.is_empty(), "duplicate command ids across modules: {dups:?}");
        assert_eq!(super::registry().len(), seen.len());
    }

    #[test]
    fn every_spec_has_label_and_location() {
        for s in super::all_specs() {
            assert!(!s.id.is_empty(), "empty id");
            assert!(s.id.contains('.'), "{}: id lacks an area prefix", s.id);
            assert!(!s.label.trim().is_empty(), "{}: empty label", s.id);
            assert!(!s.location.trim().is_empty(), "{}: empty location", s.id);
        }
    }
}
