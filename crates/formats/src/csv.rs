//! CSV (RFC 4180) and the contacts import/export mapping.
//!
//! The parser accepts CRLF or LF records, quoted fields with doubled quotes and embedded line breaks, and
//! comma, semicolon or tab delimiters (detected from the header line). The writer quotes fields that need it
//! and ends records with CRLF.
//!
//! Contacts export uses MailCraft's own column names ([`CONTACT_COLUMNS`]); import recognises those plus the
//! common spellings found in address-book exports (given/family name, "E-mail 2 Address", "Work Phone",
//! "Phone 1 - Type" / "Phone 1 - Value" pairs, "Address 1 - City", …), matched case- and punctuation-
//! insensitively.

use mailcraft_model::Contact;
use mailcraft_model::contact::PostalAddress;

use crate::{Result, check_size};

/// Largest CSV input accepted.
pub const MAX_CSV_BYTES: usize = 64 * 1024 * 1024;
/// Most records read.
pub const MAX_ROWS: usize = 1_000_000;
/// Most fields kept per record.
pub const MAX_FIELDS: usize = 1_024;
/// Longest field kept (chars); the rest is dropped.
pub const MAX_FIELD_CHARS: usize = 1 << 20;
/// Most phone numbers kept per contact.
pub const MAX_PHONES: usize = 64;

/// Picks the delimiter that splits the first line into the most fields (`,` on ties or no delimiter).
/// Characters inside quoted fields do not count.
pub fn detect_delimiter(text: &str) -> char {
    let first = first_record_line(text);
    let mut counts = [(',', 0usize), (';', 0), ('\t', 0)];
    let mut in_quotes = false;
    for c in first.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if !in_quotes && let Some(slot) = counts.iter_mut().find(|(d, _)| *d == c) {
            slot.1 += 1;
        }
    }
    let mut best = (',', 0usize);
    for (d, n) in counts {
        if n > best.1 {
            best = (d, n);
        }
    }
    best.0
}

/// First physical line outside quotes.
fn first_record_line(text: &str) -> &str {
    let mut in_quotes = false;
    for (i, c) in text.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '\n' | '\r' if !in_quotes => return text.get(..i).unwrap_or(text),
            _ => {}
        }
    }
    text
}

/// Parses CSV text into records (blank lines skipped).
pub fn parse(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut field_chars = 0usize;
    let mut in_quotes = false;
    let mut quoted_field = false;
    let mut chars = text.chars().peekable();
    let push_char = |field: &mut String, n: &mut usize, c: char| {
        if *n < MAX_FIELD_CHARS {
            field.push(c);
            *n += 1;
        }
    };
    let end_field = |row: &mut Vec<String>, field: &mut String, n: &mut usize| {
        if row.len() < MAX_FIELDS {
            row.push(std::mem::take(field));
        } else {
            field.clear();
        }
        *n = 0;
    };
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    push_char(&mut field, &mut field_chars, '"');
                } else {
                    in_quotes = false;
                }
            } else {
                push_char(&mut field, &mut field_chars, c);
            }
            continue;
        }
        if c == '"' && field.is_empty() && !quoted_field {
            in_quotes = true;
            quoted_field = true;
        } else if c == delimiter {
            end_field(&mut row, &mut field, &mut field_chars);
            quoted_field = false;
        } else if c == '\r' || c == '\n' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            end_field(&mut row, &mut field, &mut field_chars);
            // A line holding only `""` is a record with one empty field, not a blank line.
            let blank = row.len() == 1 && row.first().is_some_and(String::is_empty) && !quoted_field;
            quoted_field = false;
            if !blank {
                rows.push(std::mem::take(&mut row));
                if rows.len() >= MAX_ROWS {
                    return rows;
                }
            }
            row.clear();
        } else {
            push_char(&mut field, &mut field_chars, c);
        }
    }
    if !field.is_empty() || !row.is_empty() || quoted_field {
        end_field(&mut row, &mut field, &mut field_chars);
        let blank = row.len() == 1 && row.first().is_some_and(String::is_empty) && !quoted_field;
        if !blank && rows.len() < MAX_ROWS {
            rows.push(row);
        }
    }
    rows
}

/// Parses CSV bytes (UTF-8/UTF-16 with BOM, else Windows-1252), detecting the delimiter.
pub fn parse_bytes(data: &[u8]) -> Result<Vec<Vec<String>>> {
    check_size("csv", data.len(), MAX_CSV_BYTES)?;
    let text = crate::text::decode_bytes(data);
    let d = detect_delimiter(&text);
    Ok(parse(&text, d))
}

/// Quotes a field when it contains the delimiter, a quote, a line break, or leading/trailing spaces.
pub fn quote_field(field: &str, delimiter: char) -> String {
    let needs = field.contains(delimiter)
        || field.contains(['"', '\n', '\r'])
        || field.starts_with(' ')
        || field.ends_with(' ');
    if needs { format!("\"{}\"", field.replace('"', "\"\"")) } else { field.to_owned() }
}

/// Writes records with CRLF endings. A record of one empty field is written as `""` so that it does not
/// read back as a blank line.
pub fn write<S: AsRef<str>>(rows: &[Vec<S>], delimiter: char) -> String {
    let mut out = String::new();
    for row in rows {
        if let [only] = row.as_slice()
            && only.as_ref().is_empty()
        {
            out.push_str("\"\"\r\n");
            continue;
        }
        let fields: Vec<String> = row.iter().map(|f| quote_field(f.as_ref(), delimiter)).collect();
        out.push_str(&fields.join(&delimiter.to_string()));
        out.push_str("\r\n");
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Contacts

/// Part of a postal address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddrPart {
    Street,
    City,
    Region,
    Postal,
    Country,
}

/// What a column holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Col {
    First,
    Middle,
    Last,
    Title,
    Suffix,
    FileAs,
    FullName,
    Company,
    Department,
    JobTitle,
    Email(usize),
    Phone(&'static str),
    Addr(&'static str, AddrPart),
    WebPage,
    Im,
    Birthday,
    Notes,
    Categories,
    /// "Phone N - Type" / "Phone N - Value".
    PhoneType(u32),
    PhoneValue(u32),
    /// "Address N - Type" and parts.
    AddrNType(u32),
    AddrN(u32, AddrPart),
}

/// Phone kinds with their own export column, in column order.
const PHONE_KINDS: &[(&str, &str)] = &[
    ("Business", "Business Phone"),
    ("Business 2", "Business Phone 2"),
    ("Business Fax", "Business Fax"),
    ("Home", "Home Phone"),
    ("Home 2", "Home Phone 2"),
    ("Home Fax", "Home Fax"),
    ("Mobile", "Mobile Phone"),
    ("Pager", "Pager"),
    ("Assistant", "Assistant Phone"),
    ("Car", "Car Phone"),
    ("Company Main", "Company Main Phone"),
    ("Other", "Other Phone"),
];

const ADDR_KINDS: &[&str] = &["Business", "Home", "Other"];

const ADDR_PARTS: &[(AddrPart, &str)] = &[
    (AddrPart::Street, "Street"),
    (AddrPart::City, "City"),
    (AddrPart::Region, "State/Region"),
    (AddrPart::Postal, "Postal Code"),
    (AddrPart::Country, "Country"),
];

/// MailCraft's contact export columns, in order.
pub const CONTACT_COLUMNS: &[&str] = &[
    "First Name",
    "Middle Name",
    "Last Name",
    "Title",
    "Suffix",
    "File As",
    "Company",
    "Department",
    "Job Title",
    "Email",
    "Email 2",
    "Email 3",
    "Business Phone",
    "Business Phone 2",
    "Business Fax",
    "Home Phone",
    "Home Phone 2",
    "Home Fax",
    "Mobile Phone",
    "Pager",
    "Assistant Phone",
    "Car Phone",
    "Company Main Phone",
    "Other Phone",
    "Business Street",
    "Business City",
    "Business State/Region",
    "Business Postal Code",
    "Business Country",
    "Home Street",
    "Home City",
    "Home State/Region",
    "Home Postal Code",
    "Home Country",
    "Other Street",
    "Other City",
    "Other State/Region",
    "Other Postal Code",
    "Other Country",
    "Web Page",
    "IM Address",
    "Birthday",
    "Notes",
    "Categories",
];

/// Lower-case ASCII letters and digits only ("E-mail 2 Address" → "email2address").
fn norm(h: &str) -> String {
    h.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

fn addr_part_of(s: &str) -> Option<AddrPart> {
    Some(match s {
        "street" | "address" | "streetaddress" | "street1" | "address1" | "pobox" | "formatted" => {
            AddrPart::Street
        }
        "city" | "locality" | "town" => AddrPart::City,
        "state" | "region" | "stateregion" | "province" | "stateprovince" | "county" => AddrPart::Region,
        "postalcode" | "zip" | "zipcode" | "postcode" | "postal" => AddrPart::Postal,
        "country" | "countryregion" | "countrycode" => AddrPart::Country,
        _ => return None,
    })
}

/// Splits "phone12value" into ("phone", 12, "value").
fn numbered(n: &str) -> Option<(&str, u32, &str)> {
    let start = n.find(|c: char| c.is_ascii_digit())?;
    let prefix = n.get(..start)?;
    let rest = n.get(start..)?;
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let num: u32 = rest.get(..end)?.parse().ok()?;
    Some((prefix, num.min(1000), rest.get(end..)?))
}

fn column_of(header: &str) -> Option<Col> {
    let n = norm(header);
    let col = match n.as_str() {
        "firstname" | "givenname" | "first" | "given" | "forename" => Col::First,
        "middlename" | "additionalname" | "middle" | "additionalnames" => Col::Middle,
        "lastname" | "familyname" | "surname" | "last" | "family" => Col::Last,
        "title" | "prefix" | "nameprefix" | "honorificprefix" | "salutation" => Col::Title,
        "suffix" | "namesuffix" | "honorificsuffix" => Col::Suffix,
        "fileas" | "sortas" => Col::FileAs,
        "name" | "fullname" | "displayname" | "formattedname" => Col::FullName,
        "company" | "organization" | "organisation" | "org" | "companyname" | "organization1name"
        | "organisation1name" => Col::Company,
        "department" | "dept" | "organization1department" | "organisation1department" => Col::Department,
        "jobtitle" | "position" | "organization1title" | "organisation1title" | "jobposition" => {
            Col::JobTitle
        }
        "email" | "emailaddress" | "email1" | "email1address" | "email1value" | "primaryemail"
        | "emailaddress1" | "mail" => Col::Email(0),
        "email2" | "email2address" | "email2value" | "secondaryemail" | "emailaddress2" => Col::Email(1),
        "email3" | "email3address" | "email3value" | "emailaddress3" => Col::Email(2),
        "businessphone" | "workphone" | "phonework" | "businesstelephone" | "worktelephone"
        | "officephone" => Col::Phone("Business"),
        "businessphone2" | "workphone2" => Col::Phone("Business 2"),
        "businessfax" | "workfax" | "fax" | "faxnumber" => Col::Phone("Business Fax"),
        "homephone" | "phonehome" | "hometelephone" | "personalphone" => Col::Phone("Home"),
        "homephone2" => Col::Phone("Home 2"),
        "homefax" => Col::Phone("Home Fax"),
        "mobilephone" | "mobile" | "cellphone" | "cell" | "mobilenumber" | "cellular" | "mobiletelephone" => {
            Col::Phone("Mobile")
        }
        "pager" | "pagernumber" => Col::Phone("Pager"),
        "assistantphone" | "assistantsphone" => Col::Phone("Assistant"),
        "carphone" => Col::Phone("Car"),
        "companymainphone" | "mainphone" => Col::Phone("Company Main"),
        "otherphone" | "phone" | "phonenumber" | "telephone" | "primaryphone" | "tel" => Col::Phone("Other"),
        "webpage" | "website" | "url" | "web" | "homepage" | "personalwebpage" | "businesswebpage"
        | "website1value" => Col::WebPage,
        "imaddress" | "im" | "instantmessenger" | "chat" => Col::Im,
        "birthday" | "birthdate" | "dateofbirth" | "bday" => Col::Birthday,
        "notes" | "note" | "comments" | "comment" => Col::Notes,
        "categories" | "category" | "groupmembership" | "labels" | "groups" => Col::Categories,
        _ => return column_of_compound(&n),
    };
    Some(col)
}

fn column_of_compound(n: &str) -> Option<Col> {
    for (prefix, kind) in
        [("business", "Business"), ("work", "Business"), ("home", "Home"), ("other", "Other")]
    {
        if let Some(rest) = n.strip_prefix(prefix)
            && let Some(part) = addr_part_of(rest)
        {
            return Some(Col::Addr(kind, part));
        }
    }
    let (prefix, num, rest) = numbered(n)?;
    match (prefix, rest) {
        ("phone", "type") => Some(Col::PhoneType(num)),
        ("phone", "value" | "") => Some(Col::PhoneValue(num)),
        ("email", "value" | "address" | "") => {
            Some(Col::Email(usize::try_from(num).unwrap_or(9).saturating_sub(1)))
        }
        ("address", "type") => Some(Col::AddrNType(num)),
        ("address", part) => addr_part_of(part).map(|p| Col::AddrN(num, p)),
        _ => None,
    }
}

/// Phone kind for a free-form type label ("Work", "cell", "Home Fax", …).
fn phone_kind_of(label: &str) -> &'static str {
    let n = norm(label);
    let fax = n.contains("fax");
    if n.contains("mobile") || n.contains("cell") {
        "Mobile"
    } else if n.contains("pager") {
        "Pager"
    } else if n.contains("home") {
        if fax { "Home Fax" } else { "Home" }
    } else if n.contains("work") || n.contains("business") || n.contains("office") {
        if fax { "Business Fax" } else { "Business" }
    } else if fax {
        "Business Fax"
    } else if n.contains("main") {
        "Company Main"
    } else {
        "Other"
    }
}

fn addr_kind_of(label: &str) -> &'static str {
    let n = norm(label);
    if n.contains("home") {
        "Home"
    } else if n.contains("work") || n.contains("business") || n.contains("office") {
        "Business"
    } else {
        "Other"
    }
}

/// Normalises a birthday to "YYYY-MM-DD" (accepts `YYYY-MM-DD`, `YYYYMMDD`, `M/D/YYYY`, `D.M.YYYY`).
pub(crate) fn normalize_date(s: &str) -> Option<String> {
    let s = s.trim();
    let s = s.split(['T', ' ']).next().unwrap_or(s);
    let valid = |y: i64, m: i64, d: i64| -> Option<String> {
        let mm = u8::try_from(m).ok()?;
        let dd = u8::try_from(d).ok()?;
        if !(1..=9999).contains(&y) || !(1..=12).contains(&mm) || dd == 0 || dd > days_in(y, mm) {
            return None;
        }
        Some(format!("{y:04}-{mm:02}-{dd:02}"))
    };
    let nums: Vec<&str> = s.split(['-', '/', '.']).collect();
    match nums.as_slice() {
        [y, m, d] if y.len() == 4 => valid(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?),
        [m, d, y] if y.len() == 4 && s.contains('/') => {
            valid(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
        }
        [d, m, y] if y.len() == 4 && s.contains('.') => {
            valid(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
        }
        [one] if one.len() == 8 && one.bytes().all(|b| b.is_ascii_digit()) => {
            valid(one.get(..4)?.parse().ok()?, one.get(4..6)?.parse().ok()?, one.get(6..8)?.parse().ok()?)
        }
        _ => None,
    }
}

fn days_in(y: i64, m: u8) -> u8 {
    mailcraft_model::dates::days_in_month(y, m)
}

/// Default "File as": "Last, First", else the name part present, else company, else the first email.
pub fn default_file_as(c: &Contact) -> String {
    match (c.last_name.is_empty(), c.first_name.is_empty()) {
        (false, false) => format!("{}, {}", c.last_name, c.first_name),
        (false, true) => c.last_name.clone(),
        (true, false) => c.first_name.clone(),
        (true, true) if !c.company.is_empty() => c.company.clone(),
        _ => c.emails.first().cloned().unwrap_or_default(),
    }
}

fn split_categories(s: &str) -> Vec<String> {
    let mut out = crate::text::CategoryList::default();
    for c in s.split(":::").flat_map(|p| p.split([';', ','])) {
        if out.is_full() {
            break;
        }
        out.add(c.trim().trim_start_matches('*'));
    }
    out.into_vec()
}

/// Splits a one-field name "First Middle Last" into parts.
fn split_full_name(c: &mut Contact, full: &str) {
    let words: Vec<&str> = full.split_whitespace().collect();
    match words.as_slice() {
        [] => {}
        [one] => c.first_name = (*one).to_owned(),
        [first, rest @ .., last] => {
            c.first_name = (*first).to_owned();
            c.middle_name = rest.join(" ");
            c.last_name = (*last).to_owned();
        }
    }
}

fn set_phone(c: &mut Contact, kind: &str, number: &str) {
    let number = number.trim();
    if number.is_empty() {
        return;
    }
    // Several numbers in one cell (" ::: " separated) become several entries, up to the cap.
    for n in number.split(":::").map(str::trim).filter(|n| !n.is_empty()) {
        if c.phones.len() >= MAX_PHONES {
            break;
        }
        c.phones.push((kind.to_owned(), n.to_owned()));
    }
}

fn set_addr(addrs: &mut Vec<PostalAddress>, kind: &str, part: AddrPart, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let idx = match addrs.iter().position(|a| a.kind == kind) {
        Some(i) => i,
        None => {
            addrs.push(PostalAddress { kind: kind.to_owned(), ..Default::default() });
            addrs.len() - 1
        }
    };
    if let Some(a) = addrs.get_mut(idx) {
        let slot = match part {
            AddrPart::Street => &mut a.street,
            AddrPart::City => &mut a.city,
            AddrPart::Region => &mut a.region,
            AddrPart::Postal => &mut a.postal_code,
            AddrPart::Country => &mut a.country,
        };
        if slot.is_empty() {
            *slot = value.to_owned();
        } else if part == AddrPart::Street {
            slot.push('\n');
            slot.push_str(value);
        }
    }
}

/// Result of a contacts CSV import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContactImport {
    pub contacts: Vec<Contact>,
    /// Header names that mapped to nothing (ignored).
    pub unmapped_columns: Vec<String>,
}

/// Imports contacts from CSV bytes whose first record is the header.
pub fn import_contacts(data: &[u8]) -> Result<ContactImport> {
    let rows = parse_bytes(data)?;
    Ok(contacts_from_rows(&rows))
}

/// Imports contacts from parsed records (first record = header).
pub fn contacts_from_rows(rows: &[Vec<String>]) -> ContactImport {
    let mut out = ContactImport::default();
    let Some((header, records)) = rows.split_first() else { return out };
    let cols: Vec<Option<Col>> = header
        .iter()
        .map(|h| {
            let c = column_of(h.trim_start_matches('\u{feff}'));
            if c.is_none() && !h.trim().is_empty() {
                out.unmapped_columns.push(h.clone());
            }
            c
        })
        .collect();
    for rec in records {
        let mut c = Contact::default();
        let mut emails: [String; 3] = Default::default();
        let mut extra_emails: Vec<String> = Vec::new();
        let mut phone_types: Vec<(u32, String)> = Vec::new();
        let mut phone_values: Vec<(u32, String)> = Vec::new();
        let mut addr_types: Vec<(u32, String)> = Vec::new();
        let mut addr_parts: Vec<(u32, AddrPart, String)> = Vec::new();
        let mut full_name = String::new();
        let mut file_as = String::new();
        for (value, col) in rec.iter().zip(cols.iter()) {
            let Some(col) = col else { continue };
            let v = value.trim();
            if v.is_empty() {
                continue;
            }
            match *col {
                Col::First => c.first_name = v.to_owned(),
                Col::Middle => c.middle_name = v.to_owned(),
                Col::Last => c.last_name = v.to_owned(),
                Col::Title => c.title = v.to_owned(),
                Col::Suffix => c.suffix = v.to_owned(),
                Col::FileAs => file_as = v.to_owned(),
                Col::FullName => full_name = v.to_owned(),
                Col::Company => c.company = v.to_owned(),
                Col::Department => c.department = v.to_owned(),
                Col::JobTitle => c.job_title = v.to_owned(),
                Col::Email(i) => {
                    for (k, e) in v.split(":::").map(str::trim).filter(|e| !e.is_empty()).enumerate() {
                        match emails.get_mut(i.saturating_add(k)) {
                            Some(slot) if slot.is_empty() => *slot = e.to_owned(),
                            _ => extra_emails.push(e.to_owned()),
                        }
                    }
                }
                Col::Phone(kind) => set_phone(&mut c, kind, v),
                Col::Addr(kind, part) => set_addr(&mut c.addresses, kind, part, v),
                Col::WebPage => c.web_page = v.to_owned(),
                Col::Im => c.im_address = v.to_owned(),
                Col::Birthday => c.birthday = normalize_date(v),
                Col::Notes => c.notes = value.replace("\r\n", "\n"),
                Col::Categories => c.categories = split_categories(v),
                Col::PhoneType(n) => phone_types.push((n, v.to_owned())),
                Col::PhoneValue(n) => phone_values.push((n, v.to_owned())),
                Col::AddrNType(n) => addr_types.push((n, v.to_owned())),
                Col::AddrN(n, part) => addr_parts.push((n, part, v.to_owned())),
            }
        }
        for (n, number) in phone_values {
            let label = phone_types.iter().find(|(m, _)| *m == n).map(|(_, t)| t.as_str()).unwrap_or("");
            set_phone(&mut c, phone_kind_of(label), &number);
        }
        for (n, part, v) in addr_parts {
            let label = addr_types.iter().find(|(m, _)| *m == n).map(|(_, t)| t.as_str()).unwrap_or("");
            set_addr(&mut c.addresses, addr_kind_of(label), part, &v);
        }
        for e in emails.into_iter().chain(extra_emails) {
            if !e.is_empty() && c.emails.len() < 3 {
                c.emails.push(e);
            }
        }
        if c.first_name.is_empty() && c.last_name.is_empty() && !full_name.is_empty() {
            split_full_name(&mut c, &full_name);
        }
        c.file_as = if file_as.is_empty() { default_file_as(&c) } else { file_as };
        let empty =
            c.file_as.is_empty() && c.phones.is_empty() && c.addresses.is_empty() && c.notes.is_empty();
        if !empty {
            out.contacts.push(c);
        }
    }
    out
}

/// Exports contacts as CSV with [`CONTACT_COLUMNS`] (CRLF, comma-delimited, UTF-8).
pub fn export_contacts(contacts: &[Contact]) -> String {
    let mut rows: Vec<Vec<String>> = vec![CONTACT_COLUMNS.iter().map(|s| (*s).to_owned()).collect()];
    for c in contacts {
        let mut row: Vec<String> = vec![
            c.first_name.clone(),
            c.middle_name.clone(),
            c.last_name.clone(),
            c.title.clone(),
            c.suffix.clone(),
            c.file_as.clone(),
            c.company.clone(),
            c.department.clone(),
            c.job_title.clone(),
        ];
        for i in 0..3 {
            row.push(c.emails.get(i).cloned().unwrap_or_default());
        }
        let mut phone_cells: Vec<String> = vec![String::new(); PHONE_KINDS.len()];
        let mut leftovers: Vec<&str> = Vec::new();
        for (kind, number) in &c.phones {
            let slot = PHONE_KINDS.iter().position(|(k, _)| k.eq_ignore_ascii_case(kind));
            match slot.and_then(|i| phone_cells.get_mut(i)) {
                Some(cell) if cell.is_empty() => *cell = number.clone(),
                _ => leftovers.push(number),
            }
        }
        if let Some(other) = phone_cells.last_mut() {
            for n in leftovers {
                if other.is_empty() {
                    *other = n.to_owned();
                } else {
                    other.push_str(" ::: ");
                    other.push_str(n);
                }
            }
        }
        row.extend(phone_cells);
        for kind in ADDR_KINDS {
            let a = c.addresses.iter().find(|a| a.kind.eq_ignore_ascii_case(kind));
            for (part, _) in ADDR_PARTS {
                let v = a.map(|a| match part {
                    AddrPart::Street => a.street.clone(),
                    AddrPart::City => a.city.clone(),
                    AddrPart::Region => a.region.clone(),
                    AddrPart::Postal => a.postal_code.clone(),
                    AddrPart::Country => a.country.clone(),
                });
                row.push(v.unwrap_or_default());
            }
        }
        row.push(c.web_page.clone());
        row.push(c.im_address.clone());
        row.push(c.birthday.clone().unwrap_or_default());
        row.push(c.notes.clone());
        row.push(c.categories.join(";"));
        rows.push(row);
    }
    write(&rows, ',')
}

#[cfg(test)]
pub(crate) fn columns_consistent() -> bool {
    // Every export column maps back to itself on import.
    let phones_ok = PHONE_KINDS.iter().all(|(k, h)| column_of(h) == Some(Col::Phone(k)));
    let addr_ok = ADDR_KINDS.iter().all(|k| {
        ADDR_PARTS.iter().all(|(p, name)| column_of(&format!("{k} {name}")) == Some(Col::Addr(k, *p)))
    });
    phones_ok && addr_ok && CONTACT_COLUMNS.iter().all(|h| column_of(h).is_some())
}
