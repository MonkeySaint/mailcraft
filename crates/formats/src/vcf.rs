//! vCard: RFC 6350 (4.0) and RFC 2426 (3.0) read/write, lenient with 2.1 (bare TYPE parameters,
//! quoted-printable values with CHARSET).
//!
//! Mapping to [`Contact`]: N → name parts, FN when N is missing, ORG → company;department, TITLE → job title,
//! EMAIL (first three), TEL (TYPE work/home/cell/fax/pager/car → phone kind), ADR (TYPE work/home → Business,
//! Home, else Other), URL, IMPP (and the common X-…IM properties), BDAY, NOTE, CATEGORIES. MailCraft-only
//! detail that has no standard property is kept in `X-MAILCRAFT-*` extension properties/parameters.

use mailcraft_model::Contact;
use mailcraft_model::contact::PostalAddress;

use crate::text::{CategoryList, ContentLine, MAX_CATEGORIES, escape, split_raw, split_unescaped, unescape};
use crate::{Result, check_size};

/// Largest vCard input accepted.
pub const MAX_VCF_BYTES: usize = 32 * 1024 * 1024;
/// Most cards read from one input.
pub const MAX_CARDS: usize = 100_000;
/// Most properties read per card.
pub const MAX_PROPS_PER_CARD: usize = 10_000;

/// vCard version to write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Version {
    /// RFC 2426.
    V3,
    /// RFC 6350.
    #[default]
    V4,
}

/// Parses every card in `data`.
pub fn read(data: &[u8]) -> Result<Vec<Contact>> {
    check_size("vcf", data.len(), MAX_VCF_BYTES)?;
    let text = crate::text::decode_bytes(data);
    Ok(read_str(&text))
}

/// Parses every card in `text` (lines outside BEGIN/END:VCARD are ignored).
pub fn read_str(text: &str) -> Vec<Contact> {
    let lines = join_qp_soft_breaks(crate::text::unfold(text));
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut current: Vec<ContentLine> = Vec::new();
    for line in &lines {
        let Some(cl) = ContentLine::parse(line) else { continue };
        if cl.name == "BEGIN" && cl.value.trim().eq_ignore_ascii_case("VCARD") {
            depth = depth.saturating_add(1);
            if depth == 1 {
                current.clear();
            }
            continue;
        }
        if cl.name == "END" && cl.value.trim().eq_ignore_ascii_case("VCARD") {
            if depth == 1 {
                out.push(contact_from_lines(&current));
                current.clear();
                if out.len() >= MAX_CARDS {
                    break;
                }
            }
            depth = depth.saturating_sub(1);
            continue;
        }
        // Properties of nested cards (2.1 AGENT) are ignored.
        if depth == 1 && current.len() < MAX_PROPS_PER_CARD {
            current.push(cl);
        }
    }
    out
}

/// vCard 2.1 quoted-printable values continue onto the next physical line after a trailing `=`.
fn join_qp_soft_breaks(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut continuing = false;
    for line in lines {
        if continuing && let Some(last) = out.last_mut() {
            let _ = last.pop(); // the soft-break '='
            last.push_str(&line);
            continuing = last.ends_with('=') && last.len() < crate::text::MAX_LINE_CHARS;
            continue;
        }
        let is_qp = line
            .split_once(':')
            .is_some_and(|(head, _)| head.to_ascii_uppercase().contains("QUOTED-PRINTABLE"));
        continuing = is_qp && line.ends_with('=');
        out.push(line);
    }
    out
}

/// Value with 2.1 quoted-printable / charset decoding applied (escapes not decoded).
fn raw_value(cl: &ContentLine) -> String {
    let qp = cl.param_values("ENCODING").iter().any(|e| e.eq_ignore_ascii_case("QUOTED-PRINTABLE"));
    if !qp {
        return cl.value.clone();
    }
    let bytes = crate::text::decode_qp(&cl.value);
    let enc = cl
        .param("CHARSET")
        .and_then(|c| encoding_rs::Encoding::for_label(c.trim().as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = enc.decode(&bytes);
    // Encoded CRLF / CR line breaks become the model's LF.
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// A structured value's components (each component's comma list joined with spaces).
fn components(cl: &ContentLine, n: usize) -> Vec<String> {
    let mut parts: Vec<String> = split_raw(&raw_value(cl), ';', 32)
        .iter()
        .map(|p| {
            split_raw(p, ',', 64)
                .iter()
                .map(|s| unescape(s).trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    parts.resize(n.max(parts.len()), String::new());
    parts
}

fn text_value(cl: &ContentLine) -> String {
    unescape(&raw_value(cl)).trim().to_owned()
}

fn phone_kind(cl: &ContentLine) -> String {
    if let Some(k) = cl.param("X-MAILCRAFT-KIND").filter(|k| !k.trim().is_empty()) {
        return k.trim().to_owned();
    }
    let has = |t: &str| cl.has_type(t);
    let kind = if has("cell") || has("mobile") || has("iphone") {
        "Mobile"
    } else if has("pager") {
        "Pager"
    } else if has("fax") {
        if has("home") { "Home Fax" } else { "Business Fax" }
    } else if has("car") {
        "Car"
    } else if has("home") {
        "Home"
    } else if has("work") || has("main") {
        "Business"
    } else {
        "Other"
    };
    kind.to_owned()
}

fn addr_kind(cl: &ContentLine) -> String {
    if let Some(k) = cl.param("X-MAILCRAFT-KIND").filter(|k| !k.trim().is_empty()) {
        return k.trim().to_owned();
    }
    if cl.has_type("work") {
        "Business".to_owned()
    } else if cl.has_type("home") {
        "Home".to_owned()
    } else {
        "Other".to_owned()
    }
}

fn strip_scheme<'a>(v: &'a str, scheme: &str) -> &'a str {
    match v.get(..scheme.len()) {
        Some(p) if p.eq_ignore_ascii_case(scheme) => v.get(scheme.len()..).unwrap_or(v),
        _ => v,
    }
}

/// vCard BDAY ("19700101", "1970-01-01", "1970-01-01T00:00:00Z") → "YYYY-MM-DD"; partial dates are dropped.
fn birthday(v: &str) -> Option<String> {
    let v = v.trim();
    if v.starts_with("--") {
        return None;
    }
    crate::csv::normalize_date(v)
}

fn contact_from_lines(lines: &[ContentLine]) -> Contact {
    let mut c = Contact::default();
    let mut fn_value = String::new();
    let mut have_n = false;
    let mut file_as = None;
    // RFC 6350 KIND:org (or the common X-ABShowAs:COMPANY): the card names an organisation, not a person.
    let mut is_org = false;
    let mut categories = CategoryList::default();
    for cl in lines {
        match cl.name.as_str() {
            "N" => {
                let p = components(cl, 5);
                let get = |i: usize| p.get(i).cloned().unwrap_or_default();
                c.last_name = get(0);
                c.first_name = get(1);
                c.middle_name = get(2);
                c.title = get(3);
                c.suffix = get(4);
                have_n = p.iter().any(|s| !s.is_empty());
            }
            "FN" => fn_value = text_value(cl),
            "ORG" => {
                let p = components(cl, 2);
                c.company = p.first().cloned().unwrap_or_default();
                c.department =
                    p.iter().skip(1).filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(", ");
            }
            "TITLE" => c.job_title = text_value(cl),
            "ROLE" if c.job_title.is_empty() => c.job_title = text_value(cl),
            "EMAIL" => {
                let v = text_value(cl);
                let v = strip_scheme(&v, "mailto:").trim().to_owned();
                if !v.is_empty() && c.emails.len() < 3 {
                    if cl.has_type("pref") || cl.param("PREF") == Some("1") {
                        c.emails.insert(0, v);
                    } else {
                        c.emails.push(v);
                    }
                }
            }
            "TEL" => {
                let v = text_value(cl);
                let v = strip_scheme(&v, "tel:").trim().to_owned();
                if !v.is_empty() && c.phones.len() < crate::csv::MAX_PHONES {
                    c.phones.push((phone_kind(cl), v));
                }
            }
            "ADR" => {
                let p = components(cl, 7);
                let get = |i: usize| p.get(i).cloned().unwrap_or_default();
                let street: Vec<String> =
                    [get(0), get(1), get(2)].into_iter().filter(|s| !s.is_empty()).collect();
                let a = PostalAddress {
                    kind: addr_kind(cl),
                    street: street.join("\n"),
                    city: get(3),
                    region: get(4),
                    postal_code: get(5),
                    country: get(6),
                };
                let empty = a.street.is_empty()
                    && a.city.is_empty()
                    && a.region.is_empty()
                    && a.postal_code.is_empty()
                    && a.country.is_empty();
                if !empty && c.addresses.len() < 16 {
                    c.addresses.push(a);
                }
            }
            "URL" if c.web_page.is_empty() => c.web_page = text_value(cl),
            "IMPP" | "X-AIM" | "X-ICQ" | "X-JABBER" | "X-MSN" | "X-YAHOO" | "X-SKYPE" | "X-GOOGLE-TALK"
            | "X-GADUGADU" | "X-QQ"
                if c.im_address.is_empty() =>
            {
                c.im_address = text_value(cl);
            }
            "BDAY" => c.birthday = birthday(&text_value(cl)),
            "NOTE" => {
                let v = unescape(&raw_value(cl));
                if c.notes.is_empty() {
                    c.notes = v;
                } else {
                    c.notes.push('\n');
                    c.notes.push_str(&v);
                }
            }
            "CATEGORIES" if !categories.is_full() => {
                for cat in split_unescaped(&raw_value(cl), ',', MAX_CATEGORIES) {
                    categories.add(&cat);
                }
            }
            "X-MAILCRAFT-FILE-AS" => file_as = Some(text_value(cl)),
            "KIND" => is_org = text_value(cl).eq_ignore_ascii_case("org"),
            "X-ABSHOWAS" => is_org = text_value(cl).eq_ignore_ascii_case("company"),
            _ => {}
        }
    }
    c.categories = categories.into_vec();
    // FN stands for the organisation when it repeats ORG, and for nobody when it repeats an address (the
    // writer's fallbacks for cards without a person name); otherwise it is the person's name.
    let names_company = is_org || (!c.company.is_empty() && fn_value.eq_ignore_ascii_case(&c.company));
    let names_email = c.emails.iter().any(|e| e.eq_ignore_ascii_case(&fn_value));
    if !have_n && names_company && c.company.is_empty() {
        c.company = fn_value.clone();
    }
    if !have_n && !fn_value.is_empty() && !names_company && !names_email {
        let words: Vec<&str> = fn_value.split_whitespace().collect();
        match words.as_slice() {
            [] => {}
            [one] => c.first_name = (*one).to_owned(),
            [first, rest @ .., last] => {
                c.first_name = (*first).to_owned();
                c.middle_name = rest.join(" ");
                c.last_name = (*last).to_owned();
            }
        }
        if c.first_name.is_empty() && c.last_name.is_empty() && c.company.is_empty() {
            c.company = fn_value.clone();
        }
    }
    c.file_as = file_as.filter(|f| !f.is_empty()).unwrap_or_else(|| crate::csv::default_file_as(&c));
    c
}

fn line(name: &str, params: Vec<(&str, String)>, value: String) -> ContentLine {
    ContentLine {
        group: None,
        name: name.to_owned(),
        params: params
            .into_iter()
            .map(|(n, v)| {
                let values = if n == "TYPE" { v.split(',').map(str::to_owned).collect() } else { vec![v] };
                (n.to_owned(), values)
            })
            .collect(),
        value,
    }
}

/// TYPE values for a phone kind, and whether the kind needs an X-MAILCRAFT-KIND to survive a round trip.
fn tel_types(kind: &str) -> (&'static str, bool) {
    match kind {
        "Business" => ("work,voice", false),
        "Home" => ("home,voice", false),
        "Mobile" => ("cell", false),
        "Business Fax" => ("work,fax", false),
        "Home Fax" => ("home,fax", false),
        "Pager" => ("pager", false),
        "Car" => ("car", false),
        "Other" => ("voice", false),
        k if k.to_ascii_lowercase().contains("home") => ("home,voice", true),
        k if k.to_ascii_lowercase().contains("fax") => ("fax", true),
        _ => ("work,voice", true),
    }
}

/// Serialises one contact.
pub fn write_one(c: &Contact, version: Version, out: &mut String) {
    let v4 = version == Version::V4;
    let mut lines: Vec<ContentLine> = Vec::new();
    lines.push(line("VERSION", vec![], if v4 { "4.0" } else { "3.0" }.to_owned()));
    let full = c.full_name();
    let fn_value = if !full.is_empty() {
        full
    } else if !c.company.is_empty() {
        c.company.clone()
    } else {
        c.emails.first().cloned().unwrap_or_default()
    };
    if v4 && c.full_name().is_empty() && !c.company.is_empty() {
        lines.push(line("KIND", vec![], "org".to_owned()));
    }
    lines.push(line("FN", vec![], escape(&fn_value)));
    let n = [&c.last_name, &c.first_name, &c.middle_name, &c.title, &c.suffix]
        .iter()
        .map(|s| escape(s))
        .collect::<Vec<_>>()
        .join(";");
    lines.push(line("N", vec![], n));
    if !c.company.is_empty() || !c.department.is_empty() {
        let org = if c.department.is_empty() {
            escape(&c.company)
        } else {
            format!("{};{}", escape(&c.company), escape(&c.department))
        };
        lines.push(line("ORG", vec![], org));
    }
    if !c.job_title.is_empty() {
        lines.push(line("TITLE", vec![], escape(&c.job_title)));
    }
    for (i, e) in c.emails.iter().enumerate().filter(|(_, e)| !e.is_empty()) {
        let params = match (v4, i) {
            (true, 0) => vec![("PREF", "1".to_owned())],
            (true, _) => vec![],
            (false, 0) => vec![("TYPE", "INTERNET,pref".to_owned())],
            (false, _) => vec![("TYPE", "INTERNET".to_owned())],
        };
        lines.push(line("EMAIL", params, escape(e)));
    }
    for (kind, number) in c.phones.iter().filter(|(_, n)| !n.is_empty()) {
        let (types, needs_kind) = tel_types(kind);
        let mut params = vec![("TYPE", types.to_owned())];
        if needs_kind {
            params.push(("X-MAILCRAFT-KIND", kind.clone()));
        }
        lines.push(line("TEL", params, escape(number)));
    }
    for a in &c.addresses {
        let (t, needs_kind) = match a.kind.as_str() {
            "Business" => ("work", false),
            "Home" => ("home", false),
            "Other" => ("", false),
            _ => ("", true),
        };
        let mut params = Vec::new();
        if !t.is_empty() {
            params.push(("TYPE", t.to_owned()));
        }
        if needs_kind && !a.kind.is_empty() {
            params.push(("X-MAILCRAFT-KIND", a.kind.clone()));
        }
        let value = ["", "", &a.street, &a.city, &a.region, &a.postal_code, &a.country]
            .iter()
            .map(|s| escape(s))
            .collect::<Vec<_>>()
            .join(";");
        lines.push(line("ADR", params, value));
    }
    if !c.web_page.is_empty() {
        lines.push(line("URL", vec![], c.web_page.replace(['\r', '\n'], "")));
    }
    if !c.im_address.is_empty() {
        lines.push(line("IMPP", vec![], c.im_address.replace(['\r', '\n'], "")));
    }
    if let Some(b) = c.birthday.as_deref().and_then(crate::csv::normalize_date) {
        lines.push(line("BDAY", vec![], if v4 { b.replace('-', "") } else { b }));
    }
    if !c.notes.is_empty() {
        lines.push(line("NOTE", vec![], escape(&c.notes)));
    }
    if !c.categories.is_empty() {
        let cats = c.categories.iter().map(|s| escape(s)).collect::<Vec<_>>().join(",");
        lines.push(line("CATEGORIES", vec![], cats));
    }
    if !c.file_as.is_empty() && c.file_as != crate::csv::default_file_as(c) {
        lines.push(line("X-MAILCRAFT-FILE-AS", vec![], escape(&c.file_as)));
    }
    out.push_str("BEGIN:VCARD\r\n");
    for l in &lines {
        l.write(out);
    }
    out.push_str("END:VCARD\r\n");
}

/// Serialises contacts as one vCard stream.
pub fn write(contacts: &[Contact], version: Version) -> String {
    let mut out = String::new();
    for c in contacts {
        write_one(c, version, &mut out);
    }
    out
}
