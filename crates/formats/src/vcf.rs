//! vCard: RFC 6350 (4.0) and RFC 2426 (3.0) read/write, lenient with 2.1 (bare TYPE parameters,
//! quoted-printable values with CHARSET).
//!
//! Mapping to [`Contact`]: N → name parts, FN when N is missing, ORG → company;department, TITLE → job title,
//! EMAIL (first three), TEL (TYPE work/home/cell/fax/pager/car → phone kind), ADR (TYPE work/home → Business,
//! Home, else Other), URL, IMPP (and the common X-…IM properties), BDAY, NOTE, CATEGORIES, CLASS (Private,
//! RFC 2426; written in 4.0 cards too). MailCraft-only
//! detail that has no standard property is kept in `X-MAILCRAFT-*` extension properties/parameters.
//!
//! Details page and picture:
//! - PHOTO → picture (PNG, JPEG or GIF, at most [`MAX_PICTURE_BYTES`] and [`MAX_PHOTO_SIDE`] pixels a side).
//!   Read from an RFC 6350 `data:image/…;base64,` URI, an RFC 2426 `ENCODING=b` value, or a vCard 2.1
//!   `ENCODING=BASE64` value; remote (http) photos are not fetched. Written as a `data:` URI (4.0) or
//!   `ENCODING=b;TYPE=JPEG|PNG|GIF` (3.0).
//! - NICKNAME → nickname; ROLE → profession; ANNIVERSARY (4.0; `X-ANNIVERSARY` in 3.0) → anniversary.
//! - Spouse/partner: RFC 6350 `RELATED;VALUE=text;TYPE=spouse` (4.0); `X-MAILCRAFT-SPOUSE` in 3.0, which has no
//!   RELATED property.
//! - Office, manager and assistant have no standard property: `X-MAILCRAFT-OFFICE`, `X-MAILCRAFT-MANAGER`,
//!   `X-MAILCRAFT-ASSISTANT`.
//! - Read also: the `X-` names other address books use for these fields (`X-MS-…`, `X-EVOLUTION-…`,
//!   `X-KADDRESSBOOK-X-…`, plain `X-MANAGER` and similar), and grouped `X-ABRELATEDNAMES` / `X-ABDATE` values
//!   with an `X-ABLABEL` naming the relation or date.
//! - KEY with an X.509 certificate → certificates (S/MIME, at most [`MAX_KEYS`] of [`MAX_KEY_BYTES`] each):
//!   an RFC 6350 `data:application/pkix-cert;base64,` URI, or an RFC 2426 / 2.1 `ENCODING=b|BASE64` value with
//!   `TYPE=X509` (or no type). Written as a `data:` URI (4.0) or `ENCODING=b;TYPE=X509` (3.0). PGP keys and
//!   key URLs are not read.

use mailcraft_model::Contact;
use mailcraft_model::contact::{ContactPicture, MAX_PICTURE_BYTES, PostalAddress};

use crate::text::{CategoryList, ContentLine, MAX_CATEGORIES, escape, split_raw, split_unescaped, unescape};
use crate::{Result, check_size};

/// Largest vCard input accepted.
pub const MAX_VCF_BYTES: usize = 32 * 1024 * 1024;
/// Most cards read from one input.
pub const MAX_CARDS: usize = 100_000;
/// Most properties read per card.
pub const MAX_PROPS_PER_CARD: usize = 10_000;
/// Longest side of a PHOTO kept (pixels; the contact window's limit).
pub const MAX_PHOTO_SIDE: u32 = 4096;
/// Most certificates (KEY) kept per card.
pub const MAX_KEYS: usize = 8;
/// Largest certificate kept (bytes).
pub const MAX_KEY_BYTES: usize = 64 * 1024;

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
            continuing = last.ends_with('=') && last.len() < crate::text::MAX_LINE_BYTES;
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

/// Whether a value is a URI rather than text (RELATED defaults to a URI value).
fn is_uri(v: &str) -> bool {
    let lower = v.trim().to_ascii_lowercase();
    ["urn:", "http:", "https:", "mailto:", "tel:", "data:", "cid:"].iter().any(|p| lower.starts_with(p))
}

/// A PHOTO value as a picture: a `data:` URI, or inline base64 (`ENCODING=b`, 2.1 `ENCODING=BASE64`). Remote
/// photos, other encodings, other formats, and pictures beyond the size limits give `None`.
fn photo(cl: &ContentLine) -> Option<ContactPicture> {
    let value = cl.value.trim();
    let inline_b64 = cl
        .param_values("ENCODING")
        .iter()
        .any(|e| e.eq_ignore_ascii_case("b") || e.eq_ignore_ascii_case("base64"));
    let payload = if inline_b64 {
        value
    } else {
        let rest = value.get(..5).filter(|p| p.eq_ignore_ascii_case("data:")).and(value.get(5..))?;
        let (meta, payload) = rest.split_once(',')?;
        let mut parts = meta.split(';').map(str::trim);
        let image =
            parts.next().is_some_and(|m| m.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("image/")));
        if !image || !parts.any(|x| x.eq_ignore_ascii_case("base64")) {
            return None;
        }
        payload
    };
    // Base64 is 4/3 of its bytes: refuse oversized photos before decoding.
    if payload.len() / 4 * 3 > MAX_PICTURE_BYTES.saturating_add(3) {
        return None;
    }
    let data = mailcraft_mime::transfer::decode_base64(payload.as_bytes());
    let pic = ContactPicture::sniff(&data)?;
    let (w, h) = pic.dimensions()?;
    (w > 0 && h > 0 && w <= MAX_PHOTO_SIDE && h <= MAX_PHOTO_SIDE).then_some(pic)
}

/// A KEY value holding an X.509 certificate, as base64 DER (see the module docs). Other keys give `None`.
fn certificate(cl: &ContentLine) -> Option<String> {
    let value = cl.value.trim();
    let inline_b64 = cl
        .param_values("ENCODING")
        .iter()
        .any(|e| e.eq_ignore_ascii_case("b") || e.eq_ignore_ascii_case("base64"));
    let payload = if inline_b64 {
        let types = cl.param_values("TYPE");
        let x509 = types.is_empty() || types.iter().any(|t| t.eq_ignore_ascii_case("x509"));
        // vCard 2.1 writes the type as a bare parameter (`KEY;X509;ENCODING=BASE64:`).
        if !x509 && !cl.has_type("x509") {
            return None;
        }
        value
    } else {
        let rest = value.get(..5).filter(|p| p.eq_ignore_ascii_case("data:")).and(value.get(5..))?;
        let (meta, payload) = rest.split_once(',')?;
        let mut parts = meta.split(';').map(str::trim);
        let mime = parts.next().unwrap_or_default().to_ascii_lowercase();
        let cert = ["application/pkix-cert", "application/x-x509-ca-cert", "application/x-x509-user-cert"]
            .contains(&mime.as_str());
        if !cert || !parts.any(|x| x.eq_ignore_ascii_case("base64")) {
            return None;
        }
        payload
    };
    if payload.len() / 4 * 3 > MAX_KEY_BYTES {
        return None;
    }
    let der = mailcraft_mime::transfer::decode_base64(payload.as_bytes());
    // A DER certificate is a SEQUENCE; anything else is not one.
    (der.first() == Some(&0x30) && der.len() > 64).then(|| mailcraft_mime::transfer::encode_base64_flat(&der))
}

/// TYPE value for a picture's MIME type (3.0 PHOTO).
fn photo_type(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "JPEG",
        "image/gif" => "GIF",
        _ => "PNG",
    }
}

/// Which Details field an `X-ABLABEL` (Apple's grouped labels, e.g. `_$!<Spouse>!$_`) names.
fn label_field(label: &str) -> Option<&'static str> {
    let l = label.to_ascii_lowercase();
    if l.contains("spouse") || l.contains("partner") {
        Some("spouse")
    } else if l.contains("manager") {
        Some("manager")
    } else if l.contains("assistant") {
        Some("assistant")
    } else if l.contains("anniversary") {
        Some("anniversary")
    } else {
        None
    }
}

/// Sets `field` when it is still empty (the first value of a card wins).
fn set_once(field: &mut String, v: String) {
    if field.is_empty() {
        *field = v;
    }
}

fn contact_from_lines(lines: &[ContentLine]) -> Contact {
    contact_and_fields(lines).0
}

/// The Contact field each line of a card feeds, as [`contact_from_lines`] reads it (`None`: a line the
/// reader leaves alone, such as an unknown property, a fourth e-mail address or a second web page). A field
/// read from several names (spouse from `X-EVOLUTION-SPOUSE`, `RELATED;TYPE=spouse`, `X-ABRELATEDNAMES`…)
/// names every line it may come from, so replacing the field replaces all of them; in lists (e-mail
/// addresses, phones, addresses, certificates) and in the web page and IM address, only the lines read
/// (and, for the IM address, other forms of the same account) belong to the field. Used to merge an edit
/// into a server's card ([`crate::keep`]).
pub(crate) fn line_fields(lines: &[ContentLine]) -> Vec<Option<&'static str>> {
    contact_and_fields(lines).1
}

/// Same account in two forms (`skype:pat` and `pat`): the value without a URI scheme, compared without case.
fn same_account(a: &str, b: &str) -> bool {
    let bare = |v: &str| -> String {
        let v = v.trim();
        let v = match v.split_once(':') {
            Some((scheme, rest))
                if !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric()) =>
            {
                rest
            }
            _ => v,
        };
        v.trim().to_lowercase()
    };
    let (a, b) = (bare(a), bare(b));
    !a.is_empty() && a == b
}

fn contact_and_fields(lines: &[ContentLine]) -> (Contact, Vec<Option<&'static str>>) {
    let mut c = Contact::default();
    let mut fields: Vec<Option<&'static str>> = Vec::with_capacity(lines.len());
    // Apple-style grouped labels: group → label.
    let labels: Vec<(&str, String)> = lines
        .iter()
        .filter(|cl| cl.name == "X-ABLABEL")
        .filter_map(|cl| Some((cl.group.as_deref()?, text_value(cl))))
        .take(256)
        .collect();
    let label_of = |cl: &ContentLine| -> Option<&'static str> {
        let g = cl.group.as_deref()?;
        labels.iter().find(|(lg, _)| lg.eq_ignore_ascii_case(g)).and_then(|(_, l)| label_field(l))
    };
    let mut fn_value = String::new();
    let mut have_n = false;
    let mut file_as = None;
    // RFC 6350 KIND:org (or the common X-ABShowAs:COMPANY): the card names an organisation, not a person.
    let mut is_org = false;
    let mut categories = CategoryList::default();
    for cl in lines {
        let field: Option<&'static str> = match cl.name.as_str() {
            "N" => {
                let p = components(cl, 5);
                let get = |i: usize| p.get(i).cloned().unwrap_or_default();
                c.last_name = get(0);
                c.first_name = get(1);
                c.middle_name = get(2);
                c.title = get(3);
                c.suffix = get(4);
                have_n = p.iter().any(|s| !s.is_empty());
                Some("N")
            }
            "FN" => {
                fn_value = text_value(cl);
                Some("FN")
            }
            "ORG" => {
                let p = components(cl, 2);
                c.company = p.first().cloned().unwrap_or_default();
                c.department =
                    p.iter().skip(1).filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(", ");
                Some("ORG")
            }
            "TITLE" => {
                c.job_title = text_value(cl);
                Some("TITLE")
            }
            "ROLE" | "X-PROFESSION" | "X-KADDRESSBOOK-X-PROFESSION" => {
                set_once(&mut c.profession, text_value(cl));
                Some("profession")
            }
            "NICKNAME" => {
                set_once(&mut c.nickname, text_value(cl));
                Some("nickname")
            }
            "X-MAILCRAFT-OFFICE"
            | "X-OFFICE"
            | "X-MS-OFFICE"
            | "X-EVOLUTION-OFFICE"
            | "X-KADDRESSBOOK-X-OFFICE" => {
                set_once(&mut c.office, text_value(cl));
                Some("office")
            }
            "X-MAILCRAFT-MANAGER"
            | "X-MANAGER"
            | "X-MS-MANAGER"
            | "X-EVOLUTION-MANAGER"
            | "X-KADDRESSBOOK-X-MANAGERSNAME" => {
                set_once(&mut c.manager, text_value(cl));
                Some("manager")
            }
            "X-MAILCRAFT-ASSISTANT"
            | "X-ASSISTANT"
            | "X-MS-ASSISTANT"
            | "X-EVOLUTION-ASSISTANT"
            | "X-KADDRESSBOOK-X-ASSISTANTSNAME" => {
                set_once(&mut c.assistant, text_value(cl));
                Some("assistant")
            }
            "X-MAILCRAFT-SPOUSE"
            | "X-SPOUSE"
            | "X-MS-SPOUSE"
            | "X-EVOLUTION-SPOUSE"
            | "X-KADDRESSBOOK-X-SPOUSESNAME" => {
                set_once(&mut c.spouse, text_value(cl));
                Some("spouse")
            }
            // A spouse by reference (a URI) is not shown, but it is still the spouse: a new one replaces it.
            "RELATED" if cl.has_type("spouse") => {
                let v = text_value(cl);
                let text = cl.param("VALUE").is_some_and(|t| t.eq_ignore_ascii_case("text")) || !is_uri(&v);
                if text {
                    set_once(&mut c.spouse, v);
                }
                Some("spouse")
            }
            "X-ABRELATEDNAMES" => match label_of(cl) {
                Some("spouse") => {
                    set_once(&mut c.spouse, text_value(cl));
                    Some("spouse")
                }
                Some("manager") => {
                    set_once(&mut c.manager, text_value(cl));
                    Some("manager")
                }
                Some("assistant") => {
                    set_once(&mut c.assistant, text_value(cl));
                    Some("assistant")
                }
                _ => None,
            },
            "ANNIVERSARY"
            | "X-ANNIVERSARY"
            | "X-MS-ANNIVERSARY"
            | "X-EVOLUTION-ANNIVERSARY"
            | "X-KADDRESSBOOK-X-ANNIVERSARY" => {
                if c.anniversary.is_none() {
                    c.anniversary = birthday(&text_value(cl));
                }
                Some("anniversary")
            }
            "X-ABDATE" if label_of(cl) == Some("anniversary") => {
                if c.anniversary.is_none() {
                    c.anniversary = birthday(&text_value(cl));
                }
                Some("anniversary")
            }
            "PHOTO" => {
                if c.picture.is_none() {
                    c.picture = photo(cl);
                }
                Some("PHOTO")
            }
            "KEY" if c.certificates.len() < MAX_KEYS => match certificate(cl) {
                Some(cert) => {
                    if !c.certificates.contains(&cert) {
                        c.certificates.push(cert);
                    }
                    Some("KEY")
                }
                None => None,
            },
            "EMAIL" => {
                let v = text_value(cl);
                let v = strip_scheme(&v, "mailto:").trim().to_owned();
                if !v.is_empty() && c.emails.len() < 3 {
                    if cl.has_type("pref") || cl.param("PREF") == Some("1") {
                        c.emails.insert(0, v);
                    } else {
                        c.emails.push(v);
                    }
                    Some("EMAIL")
                } else {
                    None
                }
            }
            "TEL" => {
                let v = text_value(cl);
                let v = strip_scheme(&v, "tel:").trim().to_owned();
                if !v.is_empty() && c.phones.len() < crate::csv::MAX_PHONES {
                    c.phones.push((phone_kind(cl), v));
                    Some("TEL")
                } else {
                    None
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
                    Some("ADR")
                } else {
                    None
                }
            }
            "URL" if c.web_page.is_empty() => {
                c.web_page = text_value(cl);
                Some("URL")
            }
            "IMPP" | "X-AIM" | "X-ICQ" | "X-JABBER" | "X-MSN" | "X-YAHOO" | "X-SKYPE" | "X-GOOGLE-TALK"
            | "X-GADUGADU" | "X-QQ" => {
                let v = text_value(cl);
                if c.im_address.is_empty() {
                    c.im_address = v;
                    Some("IM")
                } else if same_account(&c.im_address, &v) {
                    Some("IM")
                } else {
                    None
                }
            }
            "BDAY" => {
                c.birthday = birthday(&text_value(cl));
                Some("BDAY")
            }
            "NOTE" => {
                let v = unescape(&raw_value(cl));
                if c.notes.is_empty() {
                    c.notes = v;
                } else {
                    c.notes.push('\n');
                    c.notes.push_str(&v);
                }
                Some("NOTE")
            }
            "CATEGORIES" => {
                if !categories.is_full() {
                    for cat in split_unescaped(&raw_value(cl), ',', MAX_CATEGORIES) {
                        categories.add(&cat);
                    }
                }
                Some("CATEGORIES")
            }
            "X-MAILCRAFT-FILE-AS" => {
                file_as = Some(text_value(cl));
                Some("file-as")
            }
            // RFC 2426 CLASS: PRIVATE and CONFIDENTIAL cards are private contacts.
            "CLASS" => {
                let v = text_value(cl);
                c.private = v.eq_ignore_ascii_case("private") || v.eq_ignore_ascii_case("confidential");
                Some("CLASS")
            }
            "KIND" => {
                is_org = text_value(cl).eq_ignore_ascii_case("org");
                Some("kind")
            }
            "X-ABSHOWAS" => {
                is_org = text_value(cl).eq_ignore_ascii_case("company");
                Some("kind")
            }
            _ => None,
        };
        fields.push(field);
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
    (c, fields)
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
    if !c.profession.is_empty() {
        lines.push(line("ROLE", vec![], escape(&c.profession)));
    }
    if !c.nickname.is_empty() {
        lines.push(line("NICKNAME", vec![], escape(&c.nickname)));
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
    if let Some(a) = c.anniversary.as_deref().and_then(crate::csv::normalize_date) {
        if v4 {
            lines.push(line("ANNIVERSARY", vec![], a.replace('-', "")));
        } else {
            lines.push(line("X-ANNIVERSARY", vec![], a));
        }
    }
    for (name, value) in [
        ("X-MAILCRAFT-OFFICE", &c.office),
        ("X-MAILCRAFT-MANAGER", &c.manager),
        ("X-MAILCRAFT-ASSISTANT", &c.assistant),
    ] {
        if !value.is_empty() {
            lines.push(line(name, vec![], escape(value)));
        }
    }
    if !c.spouse.is_empty() {
        if v4 {
            let params = vec![("VALUE", "text".to_owned()), ("TYPE", "spouse".to_owned())];
            lines.push(line("RELATED", params, escape(&c.spouse)));
        } else {
            lines.push(line("X-MAILCRAFT-SPOUSE", vec![], escape(&c.spouse)));
        }
    }
    if let Some(p) = c.picture.as_ref().filter(|p| !p.data.is_empty()) {
        let b64 = mailcraft_mime::transfer::encode_base64_flat(&p.data);
        if v4 {
            let mime = photo_type(&p.mime).to_ascii_lowercase();
            lines.push(line("PHOTO", vec![], format!("data:image/{mime};base64,{b64}")));
        } else {
            let params = vec![("ENCODING", "b".to_owned()), ("TYPE", photo_type(&p.mime).to_owned())];
            lines.push(line("PHOTO", params, b64));
        }
    }
    for cert in c.certificates.iter().take(MAX_KEYS).filter(|k| !k.trim().is_empty()) {
        let b64: String = cert.chars().filter(|ch| !ch.is_whitespace()).collect();
        if v4 {
            lines.push(line("KEY", vec![], format!("data:application/pkix-cert;base64,{b64}")));
        } else {
            lines.push(line("KEY", vec![("ENCODING", "b".to_owned()), ("TYPE", "X509".to_owned())], b64));
        }
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
    if c.private {
        // RFC 2426 CLASS (vCard 4.0 dropped it; readers that know it still take it there).
        lines.push(line("CLASS", vec![], "PRIVATE".to_owned()));
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
