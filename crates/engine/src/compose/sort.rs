//! Sort Text: how paragraphs, list items and table rows are compared (by the whole text or one field, as
//! text, numbers or dates, ascending or descending, with or without a header).

use std::cmp::Ordering;

/// What a sort key is read as.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKind {
    /// Text (numbers that are the whole key still sort as numbers).
    #[default]
    Text,
    /// The first number in the key; keys without one sort first.
    Number,
    /// A date in the key (`2026-10-21`, `10/21/2026`, `21.10.2026`, `October 21, 2026`, `21 Oct 2026`);
    /// keys without one sort first.
    Date,
}

impl SortKind {
    /// The kind called `name` (`text`, `number`, `date`).
    pub fn from_name(name: &str) -> Option<SortKind> {
        match name.trim().to_ascii_lowercase().as_str() {
            "text" => Some(SortKind::Text),
            "number" => Some(SortKind::Number),
            "date" => Some(SortKind::Date),
            _ => None,
        }
    }
}

/// Sort Text options.
#[derive(Clone, Debug, PartialEq)]
pub struct SortSpec {
    pub descending: bool,
    pub kind: SortKind,
    /// Sort by this field (0-based): a column of a table, or a part of each paragraph between separators.
    /// `None`: the whole paragraph (a table's first column).
    pub field: Option<usize>,
    /// What separates the fields of a paragraph.
    pub separator: char,
    /// The first paragraph or row is a header and keeps its place.
    pub header: bool,
    pub match_case: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        SortSpec {
            descending: false,
            kind: SortKind::Text,
            field: None,
            separator: '\t',
            header: false,
            match_case: false,
        }
    }
}

impl SortSpec {
    /// Plain ascending / descending text sort (the old one-click Sort).
    pub fn text(descending: bool) -> SortSpec {
        SortSpec { descending, ..SortSpec::default() }
    }

    /// The part of a paragraph's text this sort compares.
    pub(crate) fn key<'a>(&self, text: &'a str) -> &'a str {
        match self.field {
            Some(n) => text.split(self.separator).nth(n).unwrap_or(""),
            None => text,
        }
    }

    /// Compares two keys.
    pub(crate) fn cmp(&self, a: &str, b: &str) -> Ordering {
        let ord = match self.kind {
            // Keys that are a whole (finite) number come first, in numeric order, then the other keys as
            // text. Ranking by class first keeps this a total order (mixing number and text comparisons
            // between the same keys made cycles, which `sort_by` may panic on).
            SortKind::Text => match (whole_number(a), whole_number(b)) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) if self.match_case => a.cmp(b),
                (None, None) => a.to_lowercase().cmp(&b.to_lowercase()),
            },
            SortKind::Number => cmp_opt(number_key(a), number_key(b)),
            SortKind::Date => match (date_key(a), date_key(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (None, Some(_)) => Ordering::Less,
                (Some(_), None) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
        };
        if self.descending { ord.reverse() } else { ord }
    }
}

/// The key read as one finite number when that is all it holds (`12`, `-3.5`); not `NaN` or `inf`.
fn whole_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t.len() > MAX_NUMBER_CHARS || !t.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn cmp_opt(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Longest run of characters read as one number.
const MAX_NUMBER_CHARS: usize = 64;

/// The first number in `s` (`-1,234.5`, `$12`, `3.5 kg`).
pub(crate) fn number_key(s: &str) -> Option<f64> {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.iter().position(char::is_ascii_digit)?;
    let neg = start > 0 && chars.get(start - 1) == Some(&'-');
    let mut text = String::new();
    let mut seen_dot = false;
    for c in chars.iter().skip(start).take(MAX_NUMBER_CHARS) {
        match c {
            '0'..='9' => text.push(*c),
            ',' => {}
            '.' if !seen_dot => {
                seen_dot = true;
                text.push('.');
            }
            _ => break,
        }
    }
    let v = text.trim_end_matches('.').parse::<f64>().ok().filter(|v| v.is_finite())?;
    Some(if neg { -v } else { v })
}

const MONTHS: [&str; 12] =
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn month_of(word: &str) -> Option<u32> {
    let w = word.trim_end_matches('.').to_ascii_lowercase();
    if w.len() < 3 {
        return None;
    }
    MONTHS.iter().position(|m| w.starts_with(m)).and_then(|i| u32::try_from(i + 1).ok())
}

/// Days since 0000-03-01 of a civil date (for ordering only).
fn day_number(y: i64, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1..=9999).contains(&y) {
        return None;
    }
    let (y, m) = if m <= 2 { (y - 1, i64::from(m) + 9) } else { (y, i64::from(m) - 3) };
    Some(y * 365 + y / 4 - y / 100 + y / 400 + (153 * m + 2) / 5 + i64::from(d))
}

fn year_of(n: u32) -> i64 {
    let n = i64::from(n);
    if n < 100 { 2000 + n } else { n }
}

/// A date in `s`, as a day number.
pub(crate) fn date_key(s: &str) -> Option<i64> {
    let s: String = s.chars().take(200).collect();
    // Numeric forms: y-m-d, m/d/y, d.m.y.
    let tokens: Vec<&str> =
        s.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).collect();
    for t in &tokens {
        for sep in ['-', '/', '.'] {
            let parts: Vec<&str> = t.split(sep).collect();
            if parts.len() != 3 {
                continue;
            }
            let nums: Vec<u32> = parts.iter().filter_map(|p| p.parse::<u32>().ok()).collect();
            let [a, b, c] = nums.as_slice() else { continue };
            let day = match sep {
                '-' if parts.first().is_some_and(|p| p.len() == 4) => day_number(i64::from(*a), *b, *c),
                '/' => day_number(year_of(*c), *a, *b),
                '.' => day_number(year_of(*c), *b, *a),
                _ => None,
            };
            if day.is_some() {
                return day;
            }
        }
    }
    // Month names: "October 21, 2026", "21 Oct 2026".
    for (i, t) in tokens.iter().enumerate() {
        let Some(m) = month_of(t) else { continue };
        let num = |k: Option<usize>| {
            k.and_then(|k| tokens.get(k))
                .and_then(|t| t.trim_end_matches(['.', 's', 't', 'n', 'd', 'r', 'h']).parse::<u32>().ok())
        };
        let after = num(i.checked_add(1));
        let after2 = num(i.checked_add(2));
        let before = num(i.checked_sub(1));
        let found = match (before, after, after2) {
            (_, Some(d), Some(y)) if d <= 31 && y > 31 => day_number(year_of(y), m, d),
            (Some(d), Some(y), _) if d <= 31 => day_number(year_of(y), m, d),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_dates_are_read() {
        assert_eq!(number_key("Item 1,234.5 kg"), Some(1234.5));
        assert_eq!(number_key("-7 degrees"), Some(-7.0));
        assert_eq!(number_key("$12."), Some(12.0));
        assert_eq!(number_key("none"), None);
        let d = |s| date_key(s);
        assert!(d("2026-10-21") < d("2026-11-01"));
        assert_eq!(d("10/21/2026"), d("2026-10-21"));
        assert_eq!(d("21.10.2026"), d("2026-10-21"));
        assert_eq!(d("Meeting October 21, 2026"), d("2026-10-21"));
        assert_eq!(d("21 Oct 2026"), d("2026-10-21"));
        assert_eq!(d("13/45/2026"), None);
        assert_eq!(d("no date"), None);
        assert!(d("Dec 31, 1999") < d("Jan 1, 2000"));
    }

    #[test]
    fn specs_compare_keys() {
        let by_num = SortSpec { kind: SortKind::Number, ..SortSpec::default() };
        assert_eq!(by_num.cmp("item 9", "item 10"), Ordering::Less);
        assert_eq!(by_num.cmp("none", "item 1"), Ordering::Less);
        let desc = SortSpec { descending: true, ..by_num };
        assert_eq!(desc.cmp("item 9", "item 10"), Ordering::Greater);
        let cased = SortSpec { match_case: true, ..SortSpec::default() };
        assert_eq!(cased.cmp("b", "B"), Ordering::Greater);
        assert_eq!(SortSpec::default().cmp("b", "B"), Ordering::Equal);
        let field = SortSpec { field: Some(1), separator: ',', ..SortSpec::default() };
        assert_eq!(field.key("Smith,Anna,12"), "Anna");
        assert_eq!(field.key("alone"), "");
        assert_eq!(SortKind::from_name("Date"), Some(SortKind::Date));
        assert_eq!(SortKind::from_name("color"), None);
    }

    #[test]
    fn text_sort_is_a_total_order_on_mixed_keys() {
        // Regression: "5" < "10" (numbers) but "10" < "1x" < "5" (text) was a cycle, and NaN/inf parsed
        // as numbers equal to everything; `sort_by` may panic on such a comparator.
        let base = [
            "5",
            "10",
            "1x",
            "NaN",
            "nan",
            "inf",
            "-inf",
            "infinity",
            "",
            " ",
            "abc",
            "ABC",
            "2",
            "-1",
            "1e3",
            "0",
            "-0",
            "0.5",
            "x1",
            "10a",
            "9",
            "a10",
            "Z",
            "z",
            "3.25",
            "1,000",
            "007",
            "+4",
            "1e400",
            "-1e400",
            "\u{1F600}",
            "e",
            ".5",
            "5.",
            "1x",
            "10",
            "b",
            "B",
            "zz",
            "Item 2",
        ];
        for spec in [
            SortSpec::default(),
            SortSpec { match_case: true, ..SortSpec::default() },
            SortSpec { descending: true, ..SortSpec::default() },
            SortSpec { kind: SortKind::Number, ..SortSpec::default() },
            SortSpec { kind: SortKind::Date, ..SortSpec::default() },
        ] {
            for a in base {
                assert_eq!(spec.cmp(a, a), Ordering::Equal, "{a:?}");
                for b in base {
                    assert_eq!(spec.cmp(a, b), spec.cmp(b, a).reverse(), "{a:?} {b:?}");
                    for c in base {
                        if spec.cmp(a, b) != Ordering::Greater && spec.cmp(b, c) != Ordering::Greater {
                            assert_ne!(spec.cmp(a, c), Ordering::Greater, "{a:?} {b:?} {c:?}");
                        }
                    }
                }
            }
            let mut keys: Vec<&str> = base.iter().copied().cycle().take(400).collect();
            keys.sort_by(|a, b| spec.cmp(a, b));
            assert!(keys.windows(2).all(|w| spec.cmp(w[0], w[1]) != Ordering::Greater));
        }
        let text = SortSpec::default();
        assert_eq!(text.cmp("5", "10"), Ordering::Less);
        assert_eq!(text.cmp("10", "1x"), Ordering::Less);
        assert_eq!(text.cmp("NaN", "1"), Ordering::Greater);
    }

    #[test]
    fn hostile_keys_do_not_panic() {
        let long = "9".repeat(10_000);
        assert!(number_key(&long).is_some_and(f64::is_finite));
        let _ = date_key(&"1/".repeat(5000));
        let _ = date_key("Oct Oct Oct 99999999999 1");
        let _ = date_key("\u{1F600}-\u{1F600}-\u{1F600}");
    }
}
