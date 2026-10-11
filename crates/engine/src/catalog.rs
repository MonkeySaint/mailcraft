//! The feature catalog: every observed command (feature names only, from
//! `plan/reference/INVENTORY.md`) mapped to our command ids. `parity()` compares it with the registry;
//! the gap is the work list (`cargo xtask parity` → docs/parity.md).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};

use crate::Registry;

/// `Area|Location|Feature|command id` (`-` = not applicable to MailCraft, with the reason in the next field
/// after a `#`).
pub const CATALOG: &str = concat!(
    include_str!("../catalog/mail.txt"),
    "\n",
    include_str!("../catalog/compose.txt"),
    "\n",
    include_str!("../catalog/calendar.txt"),
    "\n",
    include_str!("../catalog/people.txt"),
    "\n",
    include_str!("../catalog/tasks.txt"),
    "\n",
    include_str!("../catalog/notes.txt"),
    "\n",
    include_str!("../catalog/search.txt"),
    "\n",
    include_str!("../catalog/context.txt"),
    "\n",
    include_str!("../catalog/read.txt"),
    "\n",
);

/// One catalog row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogRow {
    /// Catalog area as written ("Mail", "Compose", "Context", …).
    pub area: &'static str,
    /// Where the feature lives in the observed client ("Home › Respond", "message-row", …).
    pub location: &'static str,
    /// Feature name as observed.
    pub feature: &'static str,
    /// Our command id; `None` when the feature is not applicable to MailCraft.
    pub id: Option<&'static str>,
    /// Why the feature is not applicable (empty for mapped rows).
    pub reason: &'static str,
}

impl CatalogRow {
    /// Lower-case area key used in reports ("mail", "compose", …).
    pub fn area_key(&self) -> String {
        self.area.trim().to_lowercase()
    }
}

/// Parses [`CATALOG`]. Blank lines and `#` comments are skipped; malformed lines (fewer than four fields) are
/// ignored.
pub fn catalog_rows() -> Vec<CatalogRow> {
    CATALOG
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|line| {
            let mut parts = line.splitn(5, '|').map(str::trim);
            let area = parts.next()?;
            let location = parts.next()?;
            let feature = parts.next()?;
            let id = parts.next()?;
            let reason = parts.next().unwrap_or("").trim_start_matches('#').trim();
            if id.is_empty() {
                return None;
            }
            let id = (id != "-").then_some(id);
            Some(CatalogRow { area, location, feature, id, reason: if id.is_some() { "" } else { reason } })
        })
        .collect()
}

/// Per-area tally.
#[derive(Default)]
struct AreaTally {
    mapped: usize,
    implemented: usize,
    missing: BTreeSet<&'static str>,
}

/// Parity report: total, mapped, implemented (id exists in the registry), missing ids by area.
///
/// `{total_rows, not_applicable, mapped_rows, implemented_rows, percent, by_area: {area: {mapped,
/// implemented, missing: [id]}}, missing_ids: [id]}`; `percent` is implemented / mapped × 100 (one decimal).
pub fn parity(reg: &Registry) -> Value {
    let rows = catalog_rows();
    let mut not_applicable = 0usize;
    let mut mapped = 0usize;
    let mut implemented = 0usize;
    let mut by_area: BTreeMap<String, AreaTally> = BTreeMap::new();
    let mut missing_all: BTreeSet<&'static str> = BTreeSet::new();
    for row in &rows {
        let tally = by_area.entry(row.area_key()).or_default();
        match row.id {
            None => not_applicable = not_applicable.saturating_add(1),
            Some(id) => {
                mapped = mapped.saturating_add(1);
                tally.mapped = tally.mapped.saturating_add(1);
                if reg.get(id).is_some() {
                    implemented = implemented.saturating_add(1);
                    tally.implemented = tally.implemented.saturating_add(1);
                } else {
                    tally.missing.insert(id);
                    missing_all.insert(id);
                }
            }
        }
    }
    let percent = percent_of(implemented, mapped);
    let areas: serde_json::Map<String, Value> = by_area
        .into_iter()
        .map(|(area, t)| {
            let missing: Vec<&str> = t.missing.into_iter().collect();
            (area, json!({"mapped": t.mapped, "implemented": t.implemented, "missing": missing}))
        })
        .collect();
    let missing_ids: Vec<&str> = missing_all.into_iter().collect();
    json!({
        "total_rows": rows.len(),
        "not_applicable": not_applicable,
        "mapped_rows": mapped,
        "implemented_rows": implemented,
        "percent": percent,
        "by_area": areas,
        "missing_ids": missing_ids,
    })
}

/// `part / whole × 100` rounded to one decimal (0 when `whole` is 0).
fn percent_of(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    let tenths = (part as f64 * 1000.0 / whole as f64).round();
    tenths / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_parses() {
        let lines = CATALOG.lines().filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#')).count();
        assert_eq!(catalog_rows().len(), lines);
    }

    #[test]
    fn rows_are_well_formed() {
        for row in catalog_rows() {
            assert!(!row.area.is_empty() && !row.feature.is_empty(), "{row:?}");
            match row.id {
                Some(id) => {
                    let (area, name) = id.split_once('.').unwrap_or(("", ""));
                    assert!(!area.is_empty() && !name.is_empty(), "bad id in {row:?}");
                }
                None => assert!(!row.reason.is_empty(), "not-applicable row without a reason: {row:?}"),
            }
        }
    }

    #[test]
    fn parity_counts_add_up() {
        let reg = crate::cmd::registry();
        let p = parity(&reg);
        let total = p["total_rows"].as_u64().unwrap_or(0);
        let na = p["not_applicable"].as_u64().unwrap_or(0);
        let mapped = p["mapped_rows"].as_u64().unwrap_or(0);
        let implemented = p["implemented_rows"].as_u64().unwrap_or(0);
        assert!(total > 1000);
        assert_eq!(total, na + mapped);
        assert!(implemented <= mapped);
        let by_area = p["by_area"].as_object().cloned().unwrap_or_default();
        for area in ["mail", "compose", "calendar", "people", "tasks", "notes", "search", "context", "read"] {
            assert!(by_area.contains_key(area), "missing area {area}");
        }
        let sum: u64 = by_area.values().map(|a| a["mapped"].as_u64().unwrap_or(0)).sum();
        assert_eq!(sum, mapped);
    }

    #[test]
    fn empty_registry_has_zero_percent() {
        let p = parity(&Registry::default());
        assert_eq!(p["implemented_rows"], 0);
        assert_eq!(p["percent"], 0.0);
        assert_eq!(percent_of(1, 3), 33.3);
        assert_eq!(percent_of(0, 0), 0.0);
    }
}
