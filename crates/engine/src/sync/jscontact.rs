//! JSContact cards (RFC 9553, as JMAP for Contacts, RFC 9610, serves them) to and from [`Contact`].
//!
//! Mapping: `name` components (given, given2, surname, title, credential / generation) → name parts, else
//! `name.full` → first name; the first `organizations` entry → company and its first unit → department;
//! `titles` (kind title → job title, role → profession); `emails` (preferred first); `phones` (features
//! mobile, pager, fax and contexts work / private → the phone kinds MailCraft shows: Business, Home, Mobile,
//! Business Fax, Home Fax, Pager, Car, Other; another kind travels as the phone's `label`); `addresses`
//! (contexts work / private → Business / Home, components → street, city, region, postal code, country);
//! the first `links` URI → web page; the first `onlineServices` entry → IM address; `anniversaries` (birth,
//! wedding); `notes` (joined); `keywords` → categories; the first `nicknames` entry; a `relatedTo` spouse;
//! a `media` photo given as a `data:` URI → picture.
//!
//! Writing produces the same properties; an update replaces only those of them the edit changed (see
//! `jmap_pim.rs`; a property MailCraft does not map stays on the server as it is). Server data is
//! untrusted: every list is capped and nothing panics on odd input.

use mailcraft_model::Contact;
use mailcraft_model::contact::{ContactPicture, PostalAddress};
use serde_json::{Map, Value, json};

/// The card properties MailCraft writes (an update sets those the edit changed, `null` when the contact has
/// none any more).
pub(super) const MAPPED: [&str; 14] = [
    "name",
    "organizations",
    "titles",
    "emails",
    "phones",
    "addresses",
    "links",
    "onlineServices",
    "anniversaries",
    "notes",
    "keywords",
    "nicknames",
    "relatedTo",
    "media",
];

/// Sets `key` of the JSON object `v` (nothing when `v` is not an object).
fn set(v: &mut Value, key: &str, x: Value) {
    if let Some(m) = v.as_object_mut() {
        m.insert(key.to_owned(), x);
    }
}

/// Most entries read from one map of a card (emails, phones, …).
const MAX_ENTRIES: usize = 50;
/// Longest text kept from one value.
const MAX_TEXT: usize = 64 * 1024;

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).map(|s| s.chars().take(MAX_TEXT).collect::<String>()).unwrap_or_default()
}

/// The entries of a JSContact map (`{"k1": {...}}`), the preferred first (`pref` 1 = most preferred), then
/// by key.
fn entries(v: Option<&Value>) -> Vec<&Map<String, Value>> {
    let Some(m) = v.and_then(Value::as_object) else { return Vec::new() };
    let mut list: Vec<(u64, &String, &Map<String, Value>)> = m
        .iter()
        .filter_map(|(k, e)| {
            let o = e.as_object()?;
            Some((o.get("pref").and_then(Value::as_u64).unwrap_or(u64::MAX), k, o))
        })
        .collect();
    list.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    list.into_iter().take(MAX_ENTRIES).map(|(_, _, o)| o).collect()
}

fn flag(o: &Map<String, Value>, set: &str, name: &str) -> bool {
    o.get(set).and_then(|s| s.get(name)).and_then(Value::as_bool) == Some(true)
}

/// `YYYY-MM-DD` of a JSContact date (a PartialDate with year, month and day, or a Timestamp).
fn date_of(v: Option<&Value>) -> Option<String> {
    let d = v?;
    if let Some(utc) = d.get("utc").and_then(Value::as_str) {
        let day = utc.get(..10)?;
        return day.as_bytes().get(4).is_some_and(|b| *b == b'-').then(|| day.to_owned());
    }
    let n = |k: &str| d.get(k).and_then(Value::as_u64);
    let (y, m, day) = (n("year")?, n("month")?, n("day")?);
    ((1..=9999).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&day))
        .then(|| format!("{y:04}-{m:02}-{day:02}"))
}

/// A PartialDate for `YYYY-MM-DD` (or `YYYYMMDD`).
fn partial_date(d: &str) -> Option<Value> {
    let digits: String = d.trim().chars().take(10).filter(char::is_ascii_digit).collect();
    let num = |r: std::ops::Range<usize>| digits.get(r)?.parse::<u64>().ok();
    let (y, m, day) = (num(0..4)?, num(4..6)?, num(6..8)?);
    ((1..=9999).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&day))
        .then(|| json!({"@type": "PartialDate", "year": y, "month": m, "day": day}))
}

/// The contact a card describes (its `uid` separately).
pub(super) fn to_contact(card: &Value) -> (Contact, String) {
    let mut c = Contact::default();
    let uid = text(card.get("uid"));
    if let Some(name) = card.get("name") {
        let mut credentials = Vec::new();
        for comp in name.get("components").and_then(Value::as_array).into_iter().flatten().take(MAX_ENTRIES) {
            let value = text(comp.get("value"));
            let slot = match comp.get("kind").and_then(Value::as_str) {
                Some("given") => &mut c.first_name,
                Some("given2") => &mut c.middle_name,
                Some("surname") | Some("surname2") => &mut c.last_name,
                Some("title") => &mut c.title,
                Some("credential") | Some("generation") => {
                    credentials.push(value);
                    continue;
                }
                _ => continue,
            };
            if slot.is_empty() {
                *slot = value;
            } else if !value.is_empty() {
                slot.push(' ');
                slot.push_str(&value);
            }
        }
        c.suffix = credentials.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        if c.full_name().is_empty() {
            c.first_name = text(name.get("full"));
        }
    }
    if let Some(org) = entries(card.get("organizations")).first() {
        c.company = text(org.get("name"));
        c.department = org
            .get("units")
            .and_then(Value::as_array)
            .and_then(|u| u.first())
            .map(|u| text(u.get("name")))
            .unwrap_or_default();
    }
    for t in entries(card.get("titles")) {
        let slot = if t.get("kind").and_then(Value::as_str) == Some("role") {
            &mut c.profession
        } else {
            &mut c.job_title
        };
        if slot.is_empty() {
            *slot = text(t.get("name"));
        }
    }
    c.emails = entries(card.get("emails"))
        .iter()
        .map(|e| text(e.get("address")))
        .filter(|a| !a.is_empty())
        .collect();
    for p in entries(card.get("phones")) {
        let number = text(p.get("number"));
        if number.is_empty() {
            continue;
        }
        let work = flag(p, "contexts", "work");
        let home = flag(p, "contexts", "private");
        let label = text(p.get("label"));
        let kind = if flag(p, "features", "mobile") {
            "Mobile".to_owned()
        } else if flag(p, "features", "pager") {
            "Pager".to_owned()
        } else if flag(p, "features", "fax") {
            if home { "Home Fax".to_owned() } else { "Business Fax".to_owned() }
        } else if !label.is_empty() {
            label
        } else if home {
            "Home".to_owned()
        } else if work {
            "Business".to_owned()
        } else {
            "Other".to_owned()
        };
        c.phones.push((kind, number));
    }
    for a in entries(card.get("addresses")) {
        let mut addr = PostalAddress {
            kind: if flag(a, "contexts", "work") {
                "Business".to_owned()
            } else if flag(a, "contexts", "private") {
                "Home".to_owned()
            } else {
                "Other".to_owned()
            },
            ..Default::default()
        };
        let mut street: Vec<String> = Vec::new();
        for comp in a.get("components").and_then(Value::as_array).into_iter().flatten().take(MAX_ENTRIES) {
            let value = text(comp.get("value"));
            match comp.get("kind").and_then(Value::as_str) {
                Some("locality") => addr.city = value,
                Some("region") => addr.region = value,
                Some("postcode") => addr.postal_code = value,
                Some("country") => addr.country = value,
                Some("separator") | Some("countryCode") => {}
                _ if !value.is_empty() => street.push(value),
                _ => {}
            }
        }
        addr.street = street.join(" ");
        if addr.street.is_empty() && addr.city.is_empty() {
            addr.street = text(a.get("full"));
        }
        c.addresses.push(addr);
    }
    c.web_page = entries(card.get("links")).first().map(|l| text(l.get("uri"))).unwrap_or_default();
    c.im_address = entries(card.get("onlineServices"))
        .first()
        .map(|s| {
            let uri = text(s.get("uri"));
            if uri.is_empty() { text(s.get("user")) } else { uri }
        })
        .unwrap_or_default();
    for a in entries(card.get("anniversaries")) {
        let date = date_of(a.get("date"));
        match a.get("kind").and_then(Value::as_str) {
            Some("birth") if c.birthday.is_none() => c.birthday = date,
            Some("wedding") if c.anniversary.is_none() => c.anniversary = date,
            _ => {}
        }
    }
    c.notes = entries(card.get("notes"))
        .iter()
        .map(|n| text(n.get("note")))
        .filter(|n| !n.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if let Some(k) = card.get("keywords").and_then(Value::as_object) {
        c.categories = k
            .iter()
            .filter(|(_, on)| on.as_bool() == Some(true))
            .map(|(k, _)| k.chars().take(255).collect())
            .take(MAX_ENTRIES)
            .collect();
    }
    c.nickname = entries(card.get("nicknames")).first().map(|n| text(n.get("name"))).unwrap_or_default();
    if let Some(r) = card.get("relatedTo").and_then(Value::as_object) {
        c.spouse = r
            .iter()
            .find(|(_, v)| {
                v.get("relation").and_then(|x| x.get("spouse")).and_then(Value::as_bool) == Some(true)
            })
            .map(|(k, _)| k.chars().take(255).collect())
            .unwrap_or_default();
    }
    c.picture = entries(card.get("media"))
        .iter()
        .filter(|m| m.get("kind").and_then(Value::as_str) == Some("photo"))
        .find_map(|m| picture_of(m.get("uri").and_then(Value::as_str)?));
    (c, uid)
}

fn picture_of(uri: &str) -> Option<ContactPicture> {
    let rest = uri.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.ends_with(";base64") {
        return None;
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).ok()?;
    ContactPicture::sniff(&bytes)
}

/// The card properties for `c` (the [`MAPPED`] ones it has, plus `@type`, `version` and `uid`).
pub(super) fn from_contact(c: &Contact, uid: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("@type".into(), json!("Card"));
    m.insert("version".into(), json!("1.0"));
    m.insert("uid".into(), json!(uid));
    let mut comps = Vec::new();
    for (kind, value) in [
        ("title", &c.title),
        ("given", &c.first_name),
        ("given2", &c.middle_name),
        ("surname", &c.last_name),
        ("credential", &c.suffix),
    ] {
        if !value.is_empty() {
            comps.push(json!({"kind": kind, "value": value}));
        }
    }
    let full = c.full_name();
    if !comps.is_empty() || !full.is_empty() {
        m.insert("name".into(), json!({"components": comps, "full": full}));
    }
    if !c.company.is_empty() || !c.department.is_empty() {
        let mut org = json!({"name": c.company});
        if !c.department.is_empty() {
            set(&mut org, "units", json!([{"name": c.department}]));
        }
        m.insert("organizations".into(), json!({"o1": org}));
    }
    let mut titles = Map::new();
    if !c.job_title.is_empty() {
        titles.insert("t1".into(), json!({"name": c.job_title, "kind": "title"}));
    }
    if !c.profession.is_empty() {
        titles.insert("t2".into(), json!({"name": c.profession, "kind": "role"}));
    }
    if !titles.is_empty() {
        m.insert("titles".into(), Value::Object(titles));
    }
    let emails: Map<String, Value> = c
        .emails
        .iter()
        .filter(|e| !e.is_empty())
        .enumerate()
        .map(|(i, e)| {
            let mut v = json!({"address": e});
            if i == 0 {
                set(&mut v, "pref", json!(1));
            }
            (format!("e{}", i + 1), v)
        })
        .collect();
    if !emails.is_empty() {
        m.insert("emails".into(), Value::Object(emails));
    }
    let phones: Map<String, Value> = c
        .phones
        .iter()
        .filter(|(_, n)| !n.is_empty())
        .enumerate()
        .map(|(i, (kind, number))| {
            let (contexts, features, label) = match kind.as_str() {
                "Business" => (Some("work"), "voice", None),
                "Home" => (Some("private"), "voice", None),
                "Mobile" => (None, "mobile", None),
                "Business Fax" => (Some("work"), "fax", None),
                "Home Fax" => (Some("private"), "fax", None),
                "Pager" => (None, "pager", None),
                "Other" => (None, "voice", None),
                k => (None, "voice", Some(k)),
            };
            let mut v = json!({"number": number, "features": {features: true}});
            if let Some(ctx) = contexts {
                set(&mut v, "contexts", json!({ctx: true}));
            }
            if let Some(l) = label {
                set(&mut v, "label", json!(l));
            }
            (format!("p{}", i + 1), v)
        })
        .collect();
    if !phones.is_empty() {
        m.insert("phones".into(), Value::Object(phones));
    }
    let addresses: Map<String, Value> = c
        .addresses
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let comps: Vec<Value> = [
                ("name", &a.street),
                ("locality", &a.city),
                ("region", &a.region),
                ("postcode", &a.postal_code),
                ("country", &a.country),
            ]
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| json!({"kind": k, "value": v}))
            .collect();
            let mut v = json!({"components": comps});
            match a.kind.as_str() {
                "Business" => set(&mut v, "contexts", json!({"work": true})),
                "Home" => set(&mut v, "contexts", json!({"private": true})),
                _ => {}
            }
            (format!("a{}", i + 1), v)
        })
        .collect();
    if !addresses.is_empty() {
        m.insert("addresses".into(), Value::Object(addresses));
    }
    if !c.web_page.is_empty() {
        m.insert("links".into(), json!({"l1": {"uri": c.web_page}}));
    }
    if !c.im_address.is_empty() {
        let v = if c.im_address.contains(':') {
            json!({"uri": c.im_address})
        } else {
            json!({"user": c.im_address})
        };
        m.insert("onlineServices".into(), json!({"s1": v}));
    }
    let mut ann = Map::new();
    if let Some(d) = c.birthday.as_deref().and_then(partial_date) {
        ann.insert("b".into(), json!({"kind": "birth", "date": d}));
    }
    if let Some(d) = c.anniversary.as_deref().and_then(partial_date) {
        ann.insert("w".into(), json!({"kind": "wedding", "date": d}));
    }
    if !ann.is_empty() {
        m.insert("anniversaries".into(), Value::Object(ann));
    }
    if !c.notes.is_empty() {
        m.insert("notes".into(), json!({"n1": {"note": c.notes}}));
    }
    if !c.categories.is_empty() {
        let k: Map<String, Value> =
            c.categories.iter().filter(|k| !k.is_empty()).map(|k| (k.clone(), json!(true))).collect();
        m.insert("keywords".into(), Value::Object(k));
    }
    if !c.nickname.is_empty() {
        m.insert("nicknames".into(), json!({"k1": {"name": c.nickname}}));
    }
    if !c.spouse.is_empty() {
        m.insert("relatedTo".into(), json!({c.spouse.clone(): {"relation": {"spouse": true}}}));
    }
    if let Some(p) = &c.picture {
        use base64::Engine;
        let uri =
            format!("data:{};base64,{}", p.mime, base64::engine::general_purpose::STANDARD.encode(&p.data));
        m.insert("media".into(), json!({"m1": {"kind": "photo", "uri": uri}}));
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_reads_and_writes_back_the_same_contact() {
        let card = json!({
            "@type": "Card", "uid": "u-1",
            "name": {"components": [{"kind": "surname", "value": "Example"}, {"kind": "given", "value": "Pat"},
                {"kind": "given2", "value": "Q."}, {"kind": "title", "value": "Dr."},
                {"kind": "credential", "value": "Jr."}], "full": "Dr. Pat Q. Example Jr."},
            "organizations": {"k1": {"name": "Example Co", "units": [{"name": "Research"}]}},
            "titles": {"k1": {"name": "Engineer", "kind": "title"}},
            "emails": {"k2": {"address": "second@example.org"}, "k1": {"address": "pat@example.net", "pref": 1}},
            "phones": {"k1": {"number": "+1 555 0100", "features": {"mobile": true}},
                "k2": {"number": "+1 555 0101", "contexts": {"work": true}, "features": {"voice": true}},
                "k3": {"number": "+1 555 0102", "contexts": {"private": true}, "features": {"fax": true}}},
            "addresses": {"k1": {"contexts": {"work": true}, "components": [
                {"kind": "number", "value": "1"}, {"kind": "name", "value": "Main St"},
                {"kind": "locality", "value": "Springfield"}, {"kind": "region", "value": "IL"},
                {"kind": "postcode", "value": "62701"}, {"kind": "country", "value": "USA"}]}},
            "anniversaries": {"k1": {"kind": "birth", "date": {"@type": "PartialDate", "year": 1990, "month": 2, "day": 28}}},
            "notes": {"k1": {"note": "A note"}},
            "keywords": {"Red": true, "Work": true},
            "links": {"k1": {"uri": "https://example.com/"}},
            "nicknames": {"k1": {"name": "Patty"}},
            "relatedTo": {"Sam Example": {"relation": {"spouse": true}}},
        });
        let (c, uid) = to_contact(&card);
        assert_eq!(uid, "u-1");
        assert_eq!(
            (c.title.as_str(), c.first_name.as_str(), c.middle_name.as_str(), c.last_name.as_str()),
            ("Dr.", "Pat", "Q.", "Example")
        );
        assert_eq!(c.suffix, "Jr.");
        assert_eq!((c.company.as_str(), c.department.as_str()), ("Example Co", "Research"));
        assert_eq!(c.job_title, "Engineer");
        assert_eq!(c.emails, ["pat@example.net", "second@example.org"], "the preferred address first");
        assert_eq!(
            c.phones,
            [
                ("Mobile".to_owned(), "+1 555 0100".to_owned()),
                ("Business".to_owned(), "+1 555 0101".to_owned()),
                ("Home Fax".to_owned(), "+1 555 0102".to_owned())
            ]
        );
        let a = &c.addresses[0];
        assert_eq!(
            (a.kind.as_str(), a.street.as_str(), a.city.as_str(), a.postal_code.as_str()),
            ("Business", "1 Main St", "Springfield", "62701")
        );
        assert_eq!(c.birthday.as_deref(), Some("1990-02-28"));
        assert_eq!(c.notes, "A note");
        assert_eq!(c.categories, ["Red", "Work"]);
        assert_eq!(c.web_page, "https://example.com/");
        assert_eq!(c.nickname, "Patty");
        assert_eq!(c.spouse, "Sam Example");
        // Written and read again: the same contact.
        let back = Value::Object(from_contact(&c, &uid));
        let (c2, uid2) = to_contact(&back);
        assert_eq!(uid2, "u-1");
        assert_eq!(c2, c);
    }

    #[test]
    fn odd_cards_never_panic() {
        for v in [
            json!(null),
            json!([]),
            json!({"name": 5, "emails": [], "phones": {"x": 1}, "anniversaries": {"a": {"date": {"year": -1}}}}),
            json!({"name": {"full": "Only Full"}, "media": {"m": {"kind": "photo", "uri": "data:image/png;base64,@@"}}}),
            json!({"anniversaries": {"a": {"kind": "birth", "date": {"@type": "Timestamp", "utc": "x"}}}}),
        ] {
            let (c, _) = to_contact(&v);
            let _ = from_contact(&c, "");
        }
        let (c, _) = to_contact(&json!({"name": {"full": "Only Full"}}));
        assert_eq!(c.first_name, "Only Full");
    }
}
