//! Options › Advanced › International options, applied where mail is written and read:
//!
//! - "Automatically select encoding for outgoing messages" / "Preferred encoding for outgoing messages":
//!   on (default), text parts go out in UTF-8, which represents every text; off, they go out in the preferred
//!   encoding when it can represent the whole message, else in UTF-8 ([`outgoing_charset`]).
//! - "Automatically select encoding for outgoing vCards" / "Preferred encoding for outgoing vCards": the same
//!   for attached vCards (`text/vcard`), which then carry a `charset` parameter.
//! - "Allow international domain names in email addresses": on, an address such as `ann@bücher.example`
//!   is accepted and sent with its domain in ASCII form (`xn--bcher-kva.example`, RFC 5891 / RFC 3492); off,
//!   sending refuses it ([`idn_problem`]).
//! - "Use English for message headers on replies and forwards and for forward notifications": on (default),
//!   the quoted header block of a reply or forward says From / Sent / To / Subject; off, it uses the preferred
//!   authoring language (Options › Language) when MailCraft knows its words (French, Spanish, German), dates
//!   included ([`header_labels`], [`sent_date`]).
//! - "Allow UTF-8 support for the mailto: protocol": `mailto:` links (RFC 6068) decode their percent-encoded
//!   octets as UTF-8 when on, as Windows-1252 when off ([`parse_mailto`], `mail.newFromMailto`).

use mailcraft_mime::{Address, MessageBuilder};
use mailcraft_model::DateTime;

use crate::Session;
use crate::settings::Settings;

// ---------------------------------------------------------------------------------------------------------
// Outgoing encodings

/// The charset for outgoing text parts: `None` (UTF-8) while "Automatically select encoding" is on, else the
/// preferred encoding (`None` when that is UTF-8 too).
pub fn outgoing_charset(st: &Settings) -> Option<String> {
    preferred(st, "advanced.autoEncoding", "advanced.encoding")
}

/// The charset for outgoing vCards, like [`outgoing_charset`].
pub fn vcard_charset(st: &Settings) -> Option<String> {
    preferred(st, "advanced.autoVcardEncoding", "advanced.vcardEncoding")
}

fn preferred(st: &Settings, auto: &str, choice: &str) -> Option<String> {
    if st.bool(auto) {
        return None;
    }
    let cs = st.text(choice);
    (!cs.is_empty() && !cs.eq_ignore_ascii_case("utf-8")).then_some(cs)
}

/// True for an attachment type that is a vCard without its own charset.
fn is_plain_vcard(mime: &str) -> bool {
    let m = mime.to_ascii_lowercase();
    let base = m.split(';').next().unwrap_or("").trim();
    matches!(base, "text/vcard" | "text/x-vcard" | "text/directory") && !m.contains("charset")
}

/// Applies the international options to a message about to be sent: the text charset, the vCard encoding
/// and (when allowed) ASCII domains for every address. Drafts keep what the user typed.
pub(crate) fn apply_outgoing(s: &Session, b: &mut MessageBuilder) {
    let st = &s.settings;
    b.charset = outgoing_charset(st);
    if let Some(cs) = vcard_charset(st) {
        for a in &mut b.attachments {
            if !is_plain_vcard(&a.mime_type) {
                continue;
            }
            let Ok(text) = std::str::from_utf8(&a.data) else { continue };
            if let Some(bytes) = mailcraft_mime::charset::encode(text, &cs) {
                let base = a.mime_type.split(';').next().unwrap_or("text/vcard").trim().to_owned();
                a.data = bytes;
                a.mime_type = format!("{base}; charset={}", cs.to_ascii_lowercase());
            }
        }
    }
    if st.bool("advanced.idn") {
        let ascii = |m: &mut mailcraft_mime::Mailbox| {
            if let Ok(a) = mailcraft_mime::idn::addr_to_ascii(&m.addr) {
                m.addr = a;
            }
        };
        if let Some(m) = b.from.as_mut() {
            ascii(m);
        }
        if let Some(m) = b.sender.as_mut() {
            ascii(m);
        }
        for list in [&mut b.to, &mut b.cc, &mut b.bcc, &mut b.reply_to] {
            for a in list.iter_mut() {
                match a {
                    Address::Mailbox(m) => ascii(m),
                    Address::Group { members, .. } => members.iter_mut().for_each(&ascii),
                }
            }
        }
    }
}

/// Why the recipients in `fields` cannot be sent to, as far as international domain names go: some have a
/// non-ASCII domain while "Allow international domain names" is off, or a domain that is not a valid
/// international domain name. `None` when all is well.
pub(crate) fn idn_problem(s: &Session, fields: &[&str]) -> Option<String> {
    let allow = s.settings.bool("advanced.idn");
    let mut off = Vec::new();
    let mut invalid = Vec::new();
    let entries = fields.iter().flat_map(|f| crate::cmd::compose::split_recipients(f));
    for entry in entries {
        for a in mailcraft_mime::parse_address_list(entry) {
            for m in a.mailboxes() {
                if !mailcraft_mime::idn::has_idn(&m.addr) {
                    continue;
                }
                if !allow {
                    off.push(m.addr.clone());
                } else if mailcraft_mime::idn::addr_to_ascii(&m.addr).is_err() {
                    invalid.push(m.addr.clone());
                }
            }
        }
    }
    if !off.is_empty() {
        return Some(format!(
            "International domain names in email addresses are turned off (File › Options › Advanced): {}",
            off.join("; ")
        ));
    }
    (!invalid.is_empty()).then(|| {
        format!("These domain names are not valid international domain names: {}", invalid.join("; "))
    })
}

// ---------------------------------------------------------------------------------------------------------
// Reply and forward headers

/// Words of the quoted header block of a reply or forward, in one language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderLabels {
    pub from: &'static str,
    pub sent: &'static str,
    pub to: &'static str,
    pub cc: &'static str,
    pub subject: &'static str,
    pub importance: &'static str,
    pub high: &'static str,
    pub low: &'static str,
    /// Separator line above a plain-text original.
    pub original: &'static str,
    /// Weekday names, Sunday first.
    weekdays: [&'static str; 7],
    months: [&'static str; 12],
    lang: Lang,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    En,
    Fr,
    Es,
    De,
}

/// English (the default, and every language MailCraft has no header words for).
pub const ENGLISH: HeaderLabels = HeaderLabels {
    from: "From:",
    sent: "Sent:",
    to: "To:",
    cc: "Cc:",
    subject: "Subject:",
    importance: "Importance:",
    high: "High",
    low: "Low",
    original: "-----Original Message-----",
    weekdays: ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"],
    months: [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ],
    lang: Lang::En,
};

const FRENCH: HeaderLabels = HeaderLabels {
    from: "De :",
    sent: "Envoyé :",
    to: "À :",
    cc: "Cc :",
    subject: "Objet :",
    importance: "Importance :",
    high: "Haute",
    low: "Basse",
    original: "-----Message original-----",
    weekdays: ["dimanche", "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi"],
    months: [
        "janvier",
        "février",
        "mars",
        "avril",
        "mai",
        "juin",
        "juillet",
        "août",
        "septembre",
        "octobre",
        "novembre",
        "décembre",
    ],
    lang: Lang::Fr,
};

const SPANISH: HeaderLabels = HeaderLabels {
    from: "De:",
    sent: "Enviado:",
    to: "Para:",
    cc: "CC:",
    subject: "Asunto:",
    importance: "Importancia:",
    high: "Alta",
    low: "Baja",
    original: "-----Mensaje original-----",
    weekdays: ["domingo", "lunes", "martes", "miércoles", "jueves", "viernes", "sábado"],
    months: [
        "enero",
        "febrero",
        "marzo",
        "abril",
        "mayo",
        "junio",
        "julio",
        "agosto",
        "septiembre",
        "octubre",
        "noviembre",
        "diciembre",
    ],
    lang: Lang::Es,
};

const GERMAN: HeaderLabels = HeaderLabels {
    from: "Von:",
    sent: "Gesendet:",
    to: "An:",
    cc: "Cc:",
    subject: "Betreff:",
    importance: "Wichtigkeit:",
    high: "Hoch",
    low: "Niedrig",
    original: "-----Originalnachricht-----",
    weekdays: ["Sonntag", "Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag"],
    months: [
        "Januar",
        "Februar",
        "März",
        "April",
        "Mai",
        "Juni",
        "Juli",
        "August",
        "September",
        "Oktober",
        "November",
        "Dezember",
    ],
    lang: Lang::De,
};

/// Every language's header words (for recognising a quoted header block).
const ALL: [&HeaderLabels; 4] = [&ENGLISH, &FRENCH, &SPANISH, &GERMAN];

/// The header words replies and forwards use: English while "Use English for message headers" is on,
/// else those of the preferred authoring language (English when MailCraft has none for it).
pub fn header_labels(st: &Settings) -> &'static HeaderLabels {
    if st.bool("advanced.englishHeaders") {
        return &ENGLISH;
    }
    let lang = st.text("language.authoring").to_ascii_lowercase();
    match lang.split('-').next().unwrap_or("") {
        "fr" => &FRENCH,
        "es" => &SPANISH,
        "de" => &GERMAN,
        _ => &ENGLISH,
    }
}

/// The "Sent:" value in the labels' language: "Thursday, October 8, 2026 4:30 AM", "jeudi 8 octobre 2026
/// 04:30", "jueves, 8 de octubre de 2026 4:30", "Donnerstag, 8. Oktober 2026 04:30".
pub fn sent_date(t: DateTime, offset_min: i32, l: &HeaderLabels) -> String {
    if l.lang == Lang::En {
        return mailcraft_model::ListTime::long(t, offset_min);
    }
    let lt = mailcraft_model::dates::LocalTime::of(t, offset_min);
    let wd = usize::try_from(lt.day_number.saturating_add(4).rem_euclid(7)).unwrap_or(0);
    let weekday = l.weekdays.get(wd).copied().unwrap_or("");
    let month = l.months.get(usize::from(lt.month.saturating_sub(1))).copied().unwrap_or("");
    let (d, y, h, m) = (lt.day, lt.year, lt.hour, lt.minute);
    match l.lang {
        Lang::Fr => format!("{weekday} {d} {month} {y} {h:02}:{m:02}"),
        Lang::Es => format!("{weekday}, {d} de {month} de {y} {h}:{m:02}"),
        Lang::De => format!("{weekday}, {d}. {month} {y} {h:02}:{m:02}"),
        Lang::En => mailcraft_model::ListTime::long(t, offset_min),
    }
}

/// True when `line` starts a quoted header block in any language MailCraft writes ("From: …", "De : …").
pub fn is_header_start(line: &str) -> bool {
    let t = line.trim_start();
    ALL.iter().any(|l| t.starts_with(l.from))
}

/// True when `line` is the separator above a plain-text original in any language MailCraft writes.
pub fn is_original_separator(line: &str) -> bool {
    let t = line.trim();
    ALL.iter().any(|l| t == l.original)
}

// ---------------------------------------------------------------------------------------------------------
// mailto: (RFC 6068)

/// Longest `mailto:` URL accepted.
pub const MAX_MAILTO: usize = 64 * 1024;

/// A parsed `mailto:` URL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mailto {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
}

/// Decodes `%XX` escapes into octets, then the octets as UTF-8 (`utf8`, lossy) or Windows-1252.
fn pct_decode(s: &str, utf8: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        let hex = |k: usize| b.get(k).and_then(|h| char::from(*h).to_digit(16));
        if c == b'%'
            && let (Some(hi), Some(lo)) = (hex(i.saturating_add(1)), hex(i.saturating_add(2)))
        {
            out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
            i = i.saturating_add(3);
            continue;
        }
        out.push(c);
        i = i.saturating_add(1);
    }
    if utf8 {
        String::from_utf8_lossy(&out).into_owned()
    } else {
        mailcraft_mime::charset::decode(&out, "windows-1252")
    }
}

/// Addresses of a `to=` / `cc=` / `bcc=` value (comma-separated addr-specs).
fn addr_list(v: &str) -> Vec<String> {
    v.split(',').map(str::trim).filter(|a| !a.is_empty()).map(str::to_owned).take(500).collect()
}

/// Parses a `mailto:` URL: the addresses before `?`, then `to`, `cc`, `bcc`, `subject` and `body` fields
/// (case-insensitive names; other fields are ignored, as RFC 6068 § 6 advises for unsafe ones). `utf8` says
/// how percent-encoded octets decode (Options › Advanced › "Allow UTF-8 support for the mailto: protocol").
pub fn parse_mailto(url: &str, utf8: bool) -> Result<Mailto, String> {
    let url = url.trim();
    if url.len() > MAX_MAILTO {
        return Err("the mailto: link is too long".into());
    }
    let rest = match url.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("mailto:") => url.get(7..).unwrap_or(""),
        _ => return Err("not a mailto: link".into()),
    };
    // A fragment is not part of the message.
    let rest = rest.split('#').next().unwrap_or("");
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut m = Mailto { to: addr_list(&pct_decode(path, utf8)), ..Default::default() };
    for field in query.split('&').filter(|f| !f.is_empty()).take(100) {
        let (name, value) = field.split_once('=').unwrap_or((field, ""));
        let value = pct_decode(value, utf8);
        match pct_decode(name, true).to_ascii_lowercase().as_str() {
            "to" => m.to.extend(addr_list(&value)),
            "cc" => m.cc.extend(addr_list(&value)),
            "bcc" => m.bcc.extend(addr_list(&value)),
            "subject" => m.subject = value.chars().filter(|c| !c.is_control()).take(1000).collect(),
            "body" => m.body = value.replace("\r\n", "\n").replace('\r', "\n"),
            _ => {}
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::sample_session;
    use serde_json::json;

    #[test]
    fn mailto_links_decode_per_the_utf8_option() {
        let m = parse_mailto(
            "mailto:ann@example.org,bob@example.org?cc=carol@example.com&subject=Caf%C3%A9%20menu&body=Line%201%0D%0ALine%202&x-evil=1",
            true,
        )
        .unwrap();
        assert_eq!(m.to, ["ann@example.org", "bob@example.org"]);
        assert_eq!(m.cc, ["carol@example.com"]);
        assert_eq!(m.subject, "Café menu");
        assert_eq!(m.body, "Line 1\nLine 2");
        // Off: the same octets read as Windows-1252.
        assert_eq!(parse_mailto("mailto:?subject=Caf%C3%A9", false).unwrap().subject, "CafÃ©");
        assert_eq!(parse_mailto("mailto:?subject=Caf%E9", false).unwrap().subject, "Café");
        assert_eq!(parse_mailto("MAILTO:x@example.com?SUBJECT=Hi", true).unwrap().subject, "Hi");
        assert!(parse_mailto("http://example.com", true).is_err());
        assert!(parse_mailto(&format!("mailto:{}", "a".repeat(MAX_MAILTO)), true).is_err());
        // Junk escapes never panic.
        let junk = parse_mailto("mailto:%?subject=%zz%4&body=%", true).unwrap();
        assert_eq!(junk.subject, "%zz%4");
    }

    #[test]
    fn new_from_mailto_opens_a_message_and_follows_the_option() {
        let mut s = sample_session();
        let url = "mailto:ann@example.org?subject=Caf%C3%A9&body=Hello";
        let r = s.run("mail.newFromMailto", &json!({"url": url})).unwrap();
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.to, "ann@example.org");
        assert_eq!(c.subject, "Café");
        assert!(c.body.to_plain_text().contains("Hello"));
        s.run("options.set", &json!({"key": "advanced.mailtoUtf8", "value": false})).unwrap();
        let r = s.run("mail.newFromMailto", &json!({"url": url})).unwrap();
        assert_eq!(s.composes[&r["compose"].as_u64().unwrap()].subject, "CafÃ©");
        assert!(s.run("mail.newFromMailto", &json!({"url": "ftp://x"})).is_err());
        assert!(s.run("mail.newFromMailto", &json!({})).is_err());
    }

    #[test]
    fn header_words_follow_the_english_headers_option() {
        let mut st = Settings::default();
        st.set("language.authoring", &json!("fr-CA")).unwrap();
        assert_eq!(header_labels(&st), &ENGLISH, "on by default: English");
        st.set("advanced.englishHeaders", &json!(false)).unwrap();
        let fr = header_labels(&st);
        assert_eq!((fr.from, fr.subject), ("De :", "Objet :"));
        // Thursday 8 October 2026, 04:30 UTC.
        let t = DateTime::from_unix(1_791_433_800, 0);
        assert_eq!(sent_date(t, 0, &ENGLISH), "Thursday, October 8, 2026 4:30 AM");
        assert_eq!(sent_date(t, 0, fr), "jeudi 8 octobre 2026 04:30");
        st.set("language.authoring", &json!("de-DE")).unwrap();
        assert_eq!(sent_date(t, 0, header_labels(&st)), "Donnerstag, 8. Oktober 2026 04:30");
        st.set("language.authoring", &json!("es-ES")).unwrap();
        assert_eq!(sent_date(t, 0, header_labels(&st)), "jueves, 8 de octubre de 2026 4:30");
        st.set("language.authoring", &json!("en-GB")).unwrap();
        assert_eq!(header_labels(&st), &ENGLISH);
        assert!(is_header_start("  Von: Ann") && is_header_start("From: x") && !is_header_start("Fromage"));
        assert!(is_original_separator("-----Originalnachricht-----") && !is_original_separator("----"));
    }

    #[test]
    fn idn_problems_follow_the_option() {
        let mut s = sample_session();
        assert_eq!(idn_problem(&s, &["ann@bücher.example; bob@example.org"]), None);
        assert!(idn_problem(&s, &["ann@-bad-ü.example"]).is_some_and(|e| e.contains("not valid")));
        s.run("options.set", &json!({"key": "advanced.idn", "value": false})).unwrap();
        assert!(idn_problem(&s, &["ann@bücher.example"]).is_some_and(|e| e.contains("turned off")));
        assert!(
            idn_problem(&s, &["Bob <bob@example.org>; Ann <ann@bücher.example>"])
                .is_some_and(|e| e.ends_with(": ann@bücher.example"))
        );
        assert_eq!(idn_problem(&s, &["bob@example.org", ""]), None);
        // Sending refuses it with the reason.
        let c = s.run("mail.newEmail", &json!({})).unwrap()["compose"].as_u64().unwrap();
        s.run("compose.setTo", &json!({"compose": c, "value": "ann@bücher.example"})).unwrap();
        let r = s.run("compose.send", &json!({"compose": c})).unwrap();
        assert_eq!(r["sent"], false);
        assert!(r["error"].as_str().unwrap().contains("turned off"), "{r}");
    }

    #[test]
    fn outgoing_encodings_follow_the_options() {
        let mut s = sample_session();
        let mut b = MessageBuilder {
            from: Some(mailcraft_mime::Mailbox::new(Some("Me"), "me@example.com")),
            to: vec![Address::Mailbox(mailcraft_mime::Mailbox::new(None, "ann@bücher.example"))],
            subject: "Menu".into(),
            text: Some("Café crème".into()),
            attachments: vec![mailcraft_mime::AttachmentSpec {
                filename: "ann.vcf".into(),
                mime_type: "text/vcard".into(),
                data: "BEGIN:VCARD\r\nFN:Zoë\r\nEND:VCARD\r\n".as_bytes().to_vec(),
                content_id: None,
            }],
            ..Default::default()
        };
        let sent = |s: &Session, b: &MessageBuilder| {
            let mut b = b.clone();
            apply_outgoing(s, &mut b);
            String::from_utf8_lossy(&b.build()).into_owned()
        };
        // Defaults: UTF-8 text, vCard as it is, the domain in ASCII form.
        let raw = sent(&s, &b);
        assert!(raw.contains("text/plain; charset=\"utf-8\""), "{raw}");
        assert!(raw.contains("Caf=C3=A9"), "{raw}");
        assert!(raw.contains("ann@xn--bcher-kva.example"), "{raw}");
        assert!(raw.contains("Content-Type: text/vcard;"), "{raw}");
        assert!(!raw.contains("charset=windows-1252"));
        // Preferred encodings.
        for (k, v) in [
            ("advanced.autoEncoding", json!(false)),
            ("advanced.encoding", json!("iso-8859-1")),
            ("advanced.autoVcardEncoding", json!(false)),
            ("advanced.vcardEncoding", json!("windows-1252")),
        ] {
            s.run("options.set", &json!({"key": k, "value": v})).unwrap();
        }
        let raw = sent(&s, &b);
        assert!(raw.contains("text/plain; charset=\"iso-8859-1\""), "{raw}");
        assert!(raw.contains("Caf=E9 cr=E8me"), "{raw}");
        assert!(raw.contains("text/vcard; charset=windows-1252"), "{raw}");
        let parsed = mailcraft_mime::parse(raw.as_bytes()).unwrap();
        assert_eq!(parsed.text_body().as_deref().map(str::trim), Some("Café crème"));
        // An HTML body (the default compose format) declares the charset it goes out in.
        let html = mailcraft_html::doc_to_html(&mailcraft_model::Doc::from_plain_text("Café crème"));
        assert!(html.contains("<meta charset=\"utf-8\">"), "{html}");
        let with_html = MessageBuilder { html: Some(html), ..b.clone() };
        let parsed = mailcraft_mime::parse(sent(&s, &with_html).as_bytes()).unwrap();
        let body = parsed.html_body().unwrap();
        assert!(body.contains("<meta charset=\"iso-8859-1\">") && !body.contains("utf-8"), "{body}");
        assert!(body.contains("Café crème"), "{body}");
        // A text the preferred encoding cannot hold still goes out (in UTF-8).
        b.text = Some("Price: 5 €".into());
        let raw = sent(&s, &b);
        assert!(raw.contains("text/plain; charset=\"utf-8\""), "{raw}");
        // IDN off: the address is left alone (sending refuses it before this, see `idn_problem`).
        s.run("options.set", &json!({"key": "advanced.idn", "value": false})).unwrap();
        assert!(!sent(&s, &b).contains("xn--bcher-kva"));
    }
}
