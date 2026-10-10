//! RRULE (RFC 5545 §3.3.10) parsing, formatting and expansion.
//!
//! Supported: FREQ=DAILY/WEEKLY/MONTHLY/YEARLY with INTERVAL, COUNT, UNTIL, BYDAY (with ordinals in
//! MONTHLY/YEARLY), BYMONTHDAY, BYMONTH, BYYEARDAY, BYWEEKNO (YEARLY), BYSETPOS and WKST. Sub-daily
//! frequencies are rejected; BYHOUR/BYMINUTE/BYSECOND are ignored (occurrences keep DTSTART's time of day). DTSTART is always
//! the first occurrence. Expansion is bounded by a period cap and a result cap.

use mailcraft_model::dates::{days_from_civil, days_in_month};

use super::value::{self, DAY, MAX_YEAR, MIN_YEAR};

/// Most recurrence periods examined by one expansion.
pub const MAX_PERIODS: usize = 200_000;
/// Default most occurrences returned by one expansion.
pub const MAX_OCCURRENCES: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Freq {
    fn name(self) -> &'static str {
        match self {
            Freq::Daily => "DAILY",
            Freq::Weekly => "WEEKLY",
            Freq::Monthly => "MONTHLY",
            Freq::Yearly => "YEARLY",
        }
    }
}

/// UNTIL bound as written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Until {
    /// DATE: the last day (local secs at midnight), inclusive.
    Date(i64),
    /// Floating/zoned DATE-TIME (local secs), inclusive.
    Local(i64),
    /// UTC DATE-TIME (unix seconds), inclusive.
    Utc(i64),
}

const DAY_NAMES: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

/// Weekday of a day number, Monday = 0.
pub fn weekday(day: i64) -> u8 {
    u8::try_from((day + 3).rem_euclid(7)).unwrap_or(0)
}

/// Parsed RRULE. Weekdays are Monday = 0 … Sunday = 6.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rrule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<Until>,
    /// (ordinal, weekday); ordinal 0 = every such weekday in the period.
    pub by_day: Vec<(i32, u8)>,
    pub by_month_day: Vec<i32>,
    pub by_month: Vec<u8>,
    pub by_year_day: Vec<i32>,
    /// Week numbers (YEARLY only): week 1 is the first week, starting on WKST, with at least four days of
    /// the year (ISO 8601 with WKST=MO); negative values count from the year's last week.
    pub by_week_no: Vec<i32>,
    pub by_set_pos: Vec<i32>,
    pub wkst: u8,
}

impl Default for Rrule {
    fn default() -> Self {
        Rrule {
            freq: Freq::Weekly,
            interval: 1,
            count: None,
            until: None,
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            by_month: Vec::new(),
            by_year_day: Vec::new(),
            by_week_no: Vec::new(),
            by_set_pos: Vec::new(),
            wkst: 0,
        }
    }
}

/// A comma list of values; repeats are dropped (first-seen order kept), so hostile lists of one value
/// repeated cannot multiply the work of every recurrence period.
fn parse_list<T: PartialEq>(v: &str, f: impl Fn(&str) -> Option<T>) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    for item in v.split(',').map(str::trim).filter(|s| !s.is_empty()).take(400) {
        let value = f(item).ok_or_else(|| format!("bad list value `{}`", crate::text::clip(item, 32)))?;
        if !out.contains(&value) {
            out.push(value);
        }
    }
    Ok(out)
}

fn parse_weekday(s: &str) -> Option<u8> {
    let s = s.trim().to_ascii_uppercase();
    DAY_NAMES.iter().position(|d| *d == s).and_then(|i| u8::try_from(i).ok())
}

fn parse_by_day(s: &str) -> Option<(i32, u8)> {
    let s = s.trim();
    let split = s.len().checked_sub(2)?;
    let (num, day) = (s.get(..split)?, s.get(split..)?);
    let wd = parse_weekday(day)?;
    let ord: i32 = if num.is_empty() || num == "+" { 0 } else { num.trim_start_matches('+').parse().ok()? };
    if ord.abs() > 53 {
        return None;
    }
    Some((ord, wd))
}

fn signed_in(s: &str, max: i32) -> Option<i32> {
    let n: i32 = s.trim_start_matches('+').parse().ok()?;
    (n != 0 && n.abs() <= max).then_some(n)
}

impl Rrule {
    /// Parses an RRULE value (`FREQ=WEEKLY;BYDAY=MO,WE`). Unknown parts are ignored.
    pub fn parse(s: &str) -> Result<Rrule, String> {
        let mut r = Rrule::default();
        let mut freq = None;
        for part in s.trim().trim_start_matches("RRULE:").split(';').take(64) {
            let Some((k, v)) = part.split_once('=') else { continue };
            let v = v.trim();
            match k.trim().to_ascii_uppercase().as_str() {
                "FREQ" => {
                    freq = Some(match v.to_ascii_uppercase().as_str() {
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        "YEARLY" => Freq::Yearly,
                        "SECONDLY" | "MINUTELY" | "HOURLY" => {
                            return Err(format!("FREQ={v} is not supported"));
                        }
                        _ => return Err(format!("unknown FREQ `{}`", crate::text::clip(v, 16))),
                    })
                }
                "INTERVAL" => {
                    let n: u32 = v.parse().map_err(|_| "bad INTERVAL".to_owned())?;
                    r.interval = n.clamp(1, 10_000);
                }
                "COUNT" => r.count = Some(v.parse().map_err(|_| "bad COUNT".to_owned())?),
                "UNTIL" => {
                    let t = value::parse_time_value(v, None, None).ok_or_else(|| "bad UNTIL".to_owned())?;
                    r.until = Some(match t {
                        value::TimeValue::Date(d) => Until::Date(d),
                        value::TimeValue::Utc(u) => Until::Utc(u),
                        value::TimeValue::Local { secs, .. } => Until::Local(secs),
                    });
                }
                "BYDAY" => r.by_day = parse_list(v, parse_by_day)?,
                "BYMONTHDAY" => r.by_month_day = parse_list(v, |s| signed_in(s, 31))?,
                "BYMONTH" => {
                    r.by_month = parse_list(v, |s| s.parse::<u8>().ok().filter(|m| (1..=12).contains(m)))?
                }
                "BYYEARDAY" => r.by_year_day = parse_list(v, |s| signed_in(s, 366))?,
                "BYWEEKNO" => r.by_week_no = parse_list(v, |s| signed_in(s, 53))?,
                "BYSETPOS" => r.by_set_pos = parse_list(v, |s| signed_in(s, 366))?,
                "WKST" => r.wkst = parse_weekday(v).ok_or_else(|| "bad WKST".to_owned())?,
                _ => {}
            }
        }
        r.freq = freq.ok_or_else(|| "missing FREQ".to_owned())?;
        Ok(r)
    }

    /// RRULE value text.
    pub fn to_value(&self) -> String {
        let mut parts = vec![format!("FREQ={}", self.freq.name())];
        if self.interval > 1 {
            parts.push(format!("INTERVAL={}", self.interval));
        }
        if let Some(c) = self.count {
            parts.push(format!("COUNT={c}"));
        }
        match self.until {
            Some(Until::Date(d)) => parts.push(format!("UNTIL={}", value::format_date(d))),
            Some(Until::Local(l)) => parts.push(format!("UNTIL={}", value::format_date_time(l))),
            Some(Until::Utc(u)) => parts.push(format!("UNTIL={}Z", value::format_date_time(u))),
            None => {}
        }
        let join = |v: Vec<String>| v.join(",");
        if !self.by_day.is_empty() {
            let days = self
                .by_day
                .iter()
                .map(|(o, d)| {
                    let name = DAY_NAMES.get(usize::from(*d)).copied().unwrap_or("MO");
                    if *o == 0 { name.to_owned() } else { format!("{o}{name}") }
                })
                .collect();
            parts.push(format!("BYDAY={}", join(days)));
        }
        if !self.by_month_day.is_empty() {
            parts
                .push(format!("BYMONTHDAY={}", join(self.by_month_day.iter().map(i32::to_string).collect())));
        }
        if !self.by_month.is_empty() {
            parts.push(format!("BYMONTH={}", join(self.by_month.iter().map(u8::to_string).collect())));
        }
        if !self.by_year_day.is_empty() {
            parts.push(format!("BYYEARDAY={}", join(self.by_year_day.iter().map(i32::to_string).collect())));
        }
        if !self.by_week_no.is_empty() {
            parts.push(format!("BYWEEKNO={}", join(self.by_week_no.iter().map(i32::to_string).collect())));
        }
        if !self.by_set_pos.is_empty() {
            parts.push(format!("BYSETPOS={}", join(self.by_set_pos.iter().map(i32::to_string).collect())));
        }
        if self.wkst != 0 {
            parts.push(format!("WKST={}", DAY_NAMES.get(usize::from(self.wkst)).copied().unwrap_or("MO")));
        }
        parts.join(";")
    }
}

/// One occurrence: local wall-clock seconds and the UTC instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Occurrence {
    pub local: i64,
    pub unix: i64,
}

/// Inputs to [`expand`].
pub struct Expand<'a> {
    pub rule: &'a Rrule,
    /// DTSTART as local secs.
    pub dtstart: i64,
    /// Event length in seconds (occurrences overlapping the window are returned).
    pub duration: i64,
    /// Local secs → unix seconds (time-zone resolution).
    pub to_utc: &'a dyn Fn(i64) -> i64,
    /// True for an excluded occurrence (EXDATE), given (local, unix).
    pub excluded: &'a dyn Fn(i64, i64) -> bool,
}

fn month_index(day: i64) -> (i64, i64) {
    let (y, m, _) = mailcraft_model::dates::civil_from_days(day);
    (y, y * 12 + i64::from(m) - 1)
}

/// Day numbers in [first, last] with weekday `wd`, picked by ordinal (0 = all, n = nth, -n = nth from end).
fn nth_weekdays(first: i64, last: i64, ord: i32, wd: u8, out: &mut Vec<i64>) {
    if last < first {
        return;
    }
    let start = first + (i64::from(wd) - i64::from(weekday(first))).rem_euclid(7);
    if start > last {
        return;
    }
    let count = (last - start) / 7 + 1;
    match ord {
        0 => {
            for i in 0..count {
                out.push(start + i * 7);
            }
        }
        n if n > 0 => {
            let i = i64::from(n) - 1;
            if i < count {
                out.push(start + i * 7);
            }
        }
        n => {
            let i = count + i64::from(n);
            if i >= 0 {
                out.push(start + i * 7);
            }
        }
    }
}

fn resolve_month_day(md: i32, dim: u8) -> Option<u8> {
    let dim = i32::from(dim);
    let d = if md > 0 { md } else { dim + md + 1 };
    (1..=dim).contains(&d).then(|| u8::try_from(d).ok()).flatten()
}

impl Rrule {
    /// Candidate days of month (y, m) for MONTHLY/YEARLY-by-month; `start_dom` used when nothing expands.
    fn month_candidates(&self, y: i64, m: u8, start_dom: u8, out: &mut Vec<i64>) {
        let dim = days_in_month(y, m);
        let Some(first) = days_from_civil(y, m, 1) else { return };
        let last = first + i64::from(dim) - 1;
        let md: Option<Vec<i64>> = (!self.by_month_day.is_empty()).then(|| {
            self.by_month_day
                .iter()
                .filter_map(|&d| resolve_month_day(d, dim))
                .map(|d| first + i64::from(d) - 1)
                .collect()
        });
        let bd: Option<Vec<i64>> = (!self.by_day.is_empty()).then(|| {
            let mut v = Vec::new();
            for &(ord, wd) in &self.by_day {
                nth_weekdays(first, last, ord, wd, &mut v);
            }
            v.sort_unstable();
            v.dedup();
            v
        });
        match (md, bd) {
            (Some(md), Some(bd)) => out.extend(md.into_iter().filter(|d| bd.binary_search(d).is_ok())),
            (Some(md), None) => out.extend(md),
            (None, Some(bd)) => out.extend(bd),
            (None, None) => {
                if start_dom <= dim {
                    out.push(first + i64::from(start_dom) - 1);
                }
            }
        }
    }

    /// Start of the WKST-based week containing `day`.
    fn week_start(&self, day: i64) -> i64 {
        day - (i64::from(weekday(day)) - i64::from(self.wkst)).rem_euclid(7)
    }

    /// YEARLY with BYWEEKNO: the days of the listed weeks of year `y` (BYDAY weekdays, else DTSTART's),
    /// limited by BYMONTH/BYMONTHDAY/BYYEARDAY. Returns the first day of week 1 (which may be in `y - 1`).
    fn week_candidates(&self, y: i64, start_day: i64, out: &mut Vec<i64>) -> Option<i64> {
        let week1 = self.week_start(days_from_civil(y, 1, 4)?);
        let next_week1 = self.week_start(days_from_civil(y.checked_add(1)?, 1, 4)?);
        let weeks = (next_week1 - week1) / 7;
        let weekdays: Vec<u8> = if self.by_day.is_empty() {
            vec![weekday(start_day)]
        } else {
            self.by_day.iter().map(|&(_, wd)| wd).collect()
        };
        for &n in &self.by_week_no {
            let n = i64::from(n);
            let idx = if n > 0 { n } else { weeks + n + 1 };
            if !(1..=weeks).contains(&idx) {
                continue;
            }
            let ws = week1 + (idx - 1) * 7;
            for &wd in &weekdays {
                out.push(ws + (i64::from(wd) - i64::from(self.wkst)).rem_euclid(7));
            }
        }
        out.retain(|&d| self.day_matches_limits(d, false));
        Some(week1)
    }

    fn day_matches_limits(&self, day: i64, check_by_day: bool) -> bool {
        let (y, m, d) = mailcraft_model::dates::civil_from_days(day);
        if !self.by_month.is_empty() && !self.by_month.contains(&m) {
            return false;
        }
        if !self.by_month_day.is_empty() {
            let dim = days_in_month(y, m);
            if !self.by_month_day.iter().any(|&md| resolve_month_day(md, dim) == Some(d)) {
                return false;
            }
        }
        if check_by_day && !self.by_day.is_empty() && !self.by_day.iter().any(|&(_, wd)| wd == weekday(day)) {
            return false;
        }
        if !self.by_year_day.is_empty() {
            let Some(jan1) = days_from_civil(y, 1, 1) else { return false };
            let len = if mailcraft_model::dates::is_leap_year(y) { 366 } else { 365 };
            let yd = day - jan1 + 1;
            if !self.by_year_day.iter().any(|&n| {
                let n = i64::from(n);
                (if n > 0 { n } else { len + n + 1 }) == yd
            }) {
                return false;
            }
        }
        true
    }

    /// Candidate days of period `k` (sorted, unique, BYSETPOS applied). `None` when out of range.
    fn period(&self, k: i64, start_day: i64) -> Option<(i64, Vec<i64>)> {
        let step = k.checked_mul(i64::from(self.interval))?;
        let (sy, smi) = month_index(start_day);
        let (_, _, sdom) = mailcraft_model::dates::civil_from_days(start_day);
        let mut out = Vec::new();
        let first_day;
        match self.freq {
            Freq::Daily => {
                let day = start_day.checked_add(step)?;
                if mailcraft_model::dates::civil_from_days(day).0 > MAX_YEAR {
                    return None;
                }
                first_day = day;
                if self.day_matches_limits(day, true) {
                    out.push(day);
                }
            }
            Freq::Weekly => {
                let ws0 = self.week_start(start_day);
                let ws = ws0.checked_add(step.checked_mul(7)?)?;
                if mailcraft_model::dates::civil_from_days(ws).0 > MAX_YEAR {
                    return None;
                }
                first_day = ws;
                if self.by_day.is_empty() {
                    out.push(start_day.checked_add(step.checked_mul(7)?)?);
                } else {
                    for &(_, wd) in &self.by_day {
                        out.push(ws + (i64::from(wd) - i64::from(self.wkst)).rem_euclid(7));
                    }
                }
                out.retain(|&d| {
                    let (y, m, _) = mailcraft_model::dates::civil_from_days(d);
                    y <= MAX_YEAR && (self.by_month.is_empty() || self.by_month.contains(&m))
                });
            }
            Freq::Monthly => {
                let mi = smi.checked_add(step)?;
                let (y, m) = (mi.div_euclid(12), u8::try_from(mi.rem_euclid(12) + 1).ok()?);
                if !(MIN_YEAR..=MAX_YEAR).contains(&y) {
                    return None;
                }
                first_day = days_from_civil(y, m, 1)?;
                if self.by_month.is_empty() || self.by_month.contains(&m) {
                    self.month_candidates(y, m, sdom, &mut out);
                }
                if !self.by_year_day.is_empty() {
                    out.retain(|&d| self.day_matches_limits(d, false));
                }
            }
            Freq::Yearly => {
                let y = sy.checked_add(step)?;
                if !(MIN_YEAR..=MAX_YEAR).contains(&y) {
                    return None;
                }
                let jan1 = days_from_civil(y, 1, 1)?;
                let dec31 = days_from_civil(y, 12, 31)?;
                first_day = if self.by_week_no.is_empty() {
                    jan1
                } else {
                    self.week_candidates(y, start_day, &mut out)?.min(jan1)
                };
                if !self.by_week_no.is_empty() {
                    // Candidates come from the listed weeks.
                } else if !self.by_year_day.is_empty() {
                    let len = dec31 - jan1 + 1;
                    for &n in &self.by_year_day {
                        let n = i64::from(n);
                        let yd = if n > 0 { n } else { len + n + 1 };
                        if (1..=len).contains(&yd) {
                            out.push(jan1 + yd - 1);
                        }
                    }
                    out.retain(|&d| {
                        let (_, m, _) = mailcraft_model::dates::civil_from_days(d);
                        (self.by_month.is_empty() || self.by_month.contains(&m))
                            && (self.by_day.is_empty() || self.by_day.iter().any(|&(_, wd)| wd == weekday(d)))
                    });
                } else if self.by_month.is_empty() && !self.by_day.is_empty() && self.by_month_day.is_empty()
                {
                    // BYDAY ordinals count within the year.
                    for &(ord, wd) in &self.by_day {
                        nth_weekdays(jan1, dec31, ord, wd, &mut out);
                    }
                } else {
                    let (_, sm, _) = mailcraft_model::dates::civil_from_days(start_day);
                    let months: Vec<u8> = if !self.by_month.is_empty() {
                        self.by_month.clone()
                    } else if !self.by_month_day.is_empty() {
                        (1..=12).collect()
                    } else {
                        vec![sm]
                    };
                    for m in months {
                        self.month_candidates(y, m, sdom, &mut out);
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        if !self.by_set_pos.is_empty() {
            let len = i64::try_from(out.len()).unwrap_or(i64::MAX);
            let mut picked: Vec<i64> = self
                .by_set_pos
                .iter()
                .filter_map(|&p| {
                    let p = i64::from(p);
                    let idx = if p > 0 { p - 1 } else { len + p };
                    usize::try_from(idx).ok().and_then(|i| out.get(i).copied())
                })
                .collect();
            picked.sort_unstable();
            picked.dedup();
            out = picked;
        }
        Some((first_day, out))
    }

    /// Periods from DTSTART's period to the period containing `day` (0 when `day` is earlier).
    fn periods_until(&self, start_day: i64, day: i64) -> i64 {
        if day <= start_day {
            return 0;
        }
        let n = match self.freq {
            Freq::Daily => day - start_day,
            Freq::Weekly => {
                let ws = |d: i64| self.week_start(d);
                (ws(day) - ws(start_day)) / 7
            }
            Freq::Monthly => month_index(day).1 - month_index(start_day).1,
            Freq::Yearly => month_index(day).0 - month_index(start_day).0,
        };
        n / i64::from(self.interval.max(1))
    }
}

/// Occurrences that overlap `[from, to)` (unix seconds), in order, at most `max`.
pub fn expand(x: &Expand<'_>, from: i64, to: i64, max: usize) -> Vec<Occurrence> {
    expand_periods(x, from, to, max, MAX_PERIODS)
}

/// [`expand`] examining at most `max_periods` periods.
pub fn expand_periods(x: &Expand<'_>, from: i64, to: i64, max: usize, max_periods: usize) -> Vec<Occurrence> {
    let mut out = Vec::new();
    let rule = x.rule;
    let duration = x.duration.max(0);
    let start_day = x.dtstart.div_euclid(DAY);
    let tod = x.dtstart.rem_euclid(DAY);
    let mut produced: u64 = 0;
    let past_until = |local: i64, unix: i64| match rule.until {
        Some(Until::Date(d)) => local.div_euclid(DAY) > d.div_euclid(DAY),
        Some(Until::Local(l)) => local > l,
        Some(Until::Utc(u)) => unix > u,
        None => false,
    };
    let mut emit = |local: i64, out: &mut Vec<Occurrence>| -> bool {
        // Returns false to stop.
        let unix = (x.to_utc)(local);
        if past_until(local, unix) {
            return false;
        }
        if let Some(c) = rule.count
            && produced >= u64::from(c)
        {
            return false;
        }
        produced += 1;
        if unix >= to {
            return false;
        }
        if unix.saturating_add(duration) > from || unix >= from {
            if !(x.excluded)(local, unix) {
                out.push(Occurrence { local, unix });
            }
            if out.len() >= max {
                return false;
            }
        }
        true
    };
    if !emit(x.dtstart, &mut out) {
        return out;
    }
    // Skip whole periods before the window when nothing needs counting from the start.
    let k0 = if rule.count.is_none() {
        let target = from.saturating_sub(duration).div_euclid(DAY).saturating_sub(2);
        (rule.periods_until(start_day, target) - 1).max(0)
    } else {
        0
    };
    let to_bound = to.saturating_add(2 * DAY);
    let until_bound = match rule.until {
        Some(Until::Date(d) | Until::Local(d)) => Some(d.saturating_add(DAY)),
        Some(Until::Utc(u)) => Some(u.saturating_add(2 * DAY)),
        None => None,
    };
    for k in k0..k0.saturating_add(i64::try_from(max_periods).unwrap_or(i64::MAX)) {
        let Some((first_day, days)) = rule.period(k, start_day) else { break };
        let period_start = first_day.saturating_mul(DAY);
        if period_start > to_bound || until_bound.is_some_and(|u| period_start > u) {
            break;
        }
        for d in days {
            let local = d.saturating_mul(DAY).saturating_add(tod);
            if local <= x.dtstart {
                continue;
            }
            if !emit(local, &mut out) {
                return out;
            }
        }
    }
    out
}
