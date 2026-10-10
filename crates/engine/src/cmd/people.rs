//! People (contacts) commands (`people.*`).
//!
//! Contacts live in contacts folders (`Mailbox.contacts`). **Contact groups** (distribution lists) need no
//! model type of their own: a group is a [`Contact`] whose `notes` start with the line
//! [`CONTACT_GROUP_MARKER`], whose name is in `file_as`, and whose `emails` list holds the members as
//! address strings ("Name <a@b>"). [`is_group`] recognises them; the member list has no three-address limit.
//!
//! `people.new` / `people.newGroup` / `people.newFromSameCompany` create the item when field params are
//! given and otherwise ask the UI to open the contact / contact group window. Forwarding as a vCard is
//! commit 9 (formats): `people.forward` asks the UI to open the forward flow for now.

use serde_json::{Value, json};

use crate::cmd::calendar::{self, pim, pim::Kind};
use crate::cmd::util::opt_str;
use crate::{CmdError, CmdResult, CommandSpec, Session, always};
use mailcraft_model::contact::PostalAddress;
use mailcraft_model::text::contains_ci;
use mailcraft_model::{Contact, FolderId, ItemId, dates};

/// First line of `notes` marking a contact item as a contact group.
pub const CONTACT_GROUP_MARKER: &str = "Contact Group";
/// Most addresses a regular contact holds (Email, Email 2, Email 3).
pub const MAX_CONTACT_EMAILS: usize = 3;

/// True when the contact item is a contact group.
pub fn is_group(c: &Contact) -> bool {
    c.notes.lines().next().is_some_and(|l| l.trim() == CONTACT_GROUP_MARKER)
}

/// Name shown in lists: File As, else the full name, company or first address.
pub fn display_name(c: &Contact) -> String {
    if !c.file_as.trim().is_empty() {
        return c.file_as.trim().to_owned();
    }
    let full = c.full_name();
    if !full.is_empty() {
        return full;
    }
    if !c.company.trim().is_empty() {
        return c.company.trim().to_owned();
    }
    c.emails.first().cloned().unwrap_or_default()
}

/// Default File As: "Last, First" (or whichever part exists, else company, else address).
pub fn auto_file_as(c: &Contact) -> String {
    let first = c.first_name.trim();
    let last = c.last_name.trim();
    match (first.is_empty(), last.is_empty()) {
        (false, false) => format!("{last}, {first}"),
        (true, false) => last.to_owned(),
        (false, true) => first.to_owned(),
        (true, true) if !c.company.trim().is_empty() => c.company.trim().to_owned(),
        (true, true) => c.emails.first().cloned().unwrap_or_default(),
    }
}

/// User notes of a group (the marker line stripped).
fn group_notes(c: &Contact) -> String {
    match c.notes.split_once('\n') {
        Some((_, rest)) if is_group(c) => rest.to_owned(),
        _ if is_group(c) => String::new(),
        _ => c.notes.clone(),
    }
}

const CONTACT_FIELDS: [&str; 22] = [
    "fullName",
    "firstName",
    "middleName",
    "lastName",
    "title",
    "suffix",
    "fileAs",
    "company",
    "jobTitle",
    "department",
    "email",
    "email2",
    "email3",
    "emails",
    "phones",
    "addresses",
    "businessAddress",
    "webPage",
    "im",
    "birthday",
    "notes",
    "categories",
];

fn address_from(v: &Value, default_kind: &str) -> Result<PostalAddress, CmdError> {
    let Some(o) = v.as_object() else {
        return Err(CmdError::Params("addresses must be objects".into()));
    };
    let field = |k: &str| -> Result<String, CmdError> {
        match o.get(k) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(s)) if s.len() <= pim::MAX_TEXT => Ok(s.trim().to_owned()),
            Some(_) => Err(CmdError::Params(format!("address `{k}` must be a string"))),
        }
    };
    let kind = field("kind")?;
    Ok(PostalAddress {
        kind: if kind.is_empty() { default_kind.to_owned() } else { kind },
        street: field("street")?,
        city: field("city")?,
        region: field("region")?,
        postal_code: field("postalCode")?,
        country: field("country")?,
    })
}

/// Applies contact field params (shared by new and update).
pub fn apply_contact_fields(c: &mut Contact, p: &Value) -> Result<(), CmdError> {
    let was_auto = c.file_as.trim().is_empty() || c.file_as == auto_file_as(c);
    if let Some(full) = pim::text(p, "fullName")? {
        let parts: Vec<&str> = full.split_whitespace().collect();
        c.first_name = parts.first().map(|x| (*x).to_owned()).unwrap_or_default();
        c.middle_name.clear();
        c.last_name.clear();
        if parts.len() >= 2 {
            c.last_name = parts.last().map(|x| (*x).to_owned()).unwrap_or_default();
            if let Some(mid) = parts.get(1..parts.len().saturating_sub(1)) {
                c.middle_name = mid.join(" ");
            }
        }
    }
    for (key, slot) in [
        ("firstName", &mut c.first_name),
        ("middleName", &mut c.middle_name),
        ("lastName", &mut c.last_name),
        ("title", &mut c.title),
        ("suffix", &mut c.suffix),
        ("company", &mut c.company),
        ("jobTitle", &mut c.job_title),
        ("department", &mut c.department),
        ("webPage", &mut c.web_page),
        ("im", &mut c.im_address),
        ("notes", &mut c.notes),
    ] {
        if let Some(v) = pim::text(p, key)? {
            *slot = v.trim().to_owned();
        }
    }
    if let Some(list) = pim::list(p, "emails")? {
        c.emails = list;
    }
    for (i, key) in ["email", "email2", "email3"].iter().enumerate() {
        if let Some(v) = pim::text(p, key)? {
            let v = v.trim().to_owned();
            while c.emails.len() <= i {
                c.emails.push(String::new());
            }
            if let Some(slot) = c.emails.get_mut(i) {
                *slot = v;
            }
        }
    }
    while c.emails.last().is_some_and(|e| e.is_empty()) {
        c.emails.pop();
    }
    if c.emails.len() > MAX_CONTACT_EMAILS {
        return Err(CmdError::Params(format!(
            "a contact holds at most {MAX_CONTACT_EMAILS} email addresses"
        )));
    }
    match p.get("phones") {
        None => {}
        Some(Value::Null) => c.phones.clear(),
        Some(Value::Object(o)) => {
            for (kind, num) in o {
                let num = match num {
                    Value::String(n) if n.len() <= 256 => n.trim().to_owned(),
                    Value::Null => String::new(),
                    _ => return Err(CmdError::Params("`phones` values must be strings".into())),
                };
                c.phones.retain(|(k, _)| !k.eq_ignore_ascii_case(kind));
                if !num.is_empty() {
                    c.phones.push((kind.clone(), num));
                }
            }
        }
        Some(_) => {
            return Err(CmdError::Params(
                "`phones` must be an object such as {\"Business\": \"...\"}".into(),
            ));
        }
    }
    match p.get("addresses") {
        None => {}
        Some(Value::Null) => c.addresses.clear(),
        Some(Value::Array(a)) if a.len() <= 16 => {
            c.addresses = a.iter().map(|x| address_from(x, "Business")).collect::<Result<_, _>>()?;
        }
        Some(_) => return Err(CmdError::Params("`addresses` must be a list of address objects".into())),
    }
    if let Some(v) = p.get("businessAddress").filter(|v| !v.is_null()) {
        let mut addr = address_from(v, "Business")?;
        addr.kind = "Business".into();
        c.addresses.retain(|x| !x.kind.eq_ignore_ascii_case("Business"));
        c.addresses.insert(0, addr);
    }
    match p.get("birthday") {
        None => {}
        Some(Value::Null) => c.birthday = None,
        Some(Value::String(b)) if b.trim().is_empty() => c.birthday = None,
        Some(Value::String(b)) => {
            let b = b.trim();
            if !valid_birthday(b) {
                return Err(CmdError::Params("`birthday` must be YYYY-MM-DD".into()));
            }
            c.birthday = Some(b.to_owned());
        }
        Some(_) => return Err(CmdError::Params("`birthday` must be a string".into())),
    }
    if let Some(v) = pim::list(p, "categories")? {
        c.categories = v;
    }
    match pim::text(p, "fileAs")? {
        Some(f) if !f.trim().is_empty() => c.file_as = f.trim().to_owned(),
        _ if was_auto => c.file_as = auto_file_as(c),
        _ => {}
    }
    Ok(())
}

/// Applies contact group params: `name`, `members` (address strings or contact ids), `notes`, `categories`.
pub fn apply_group_fields(s: &Session, c: &mut Contact, p: &Value) -> Result<(), CmdError> {
    if let Some(name) = pim::text(p, "name")? {
        let name = name.trim();
        if name.is_empty() {
            return Err(CmdError::Params("a contact group needs a name".into()));
        }
        c.file_as = name.to_owned();
    }
    if let Some(m) = p.get("members") {
        c.emails = group_members(s, m)?;
    }
    let notes = match pim::text(p, "notes")? {
        Some(n) => n,
        None => group_notes(c),
    };
    c.notes = if notes.is_empty() {
        CONTACT_GROUP_MARKER.to_owned()
    } else {
        format!("{CONTACT_GROUP_MARKER}\n{notes}")
    };
    if let Some(v) = pim::list(p, "categories")? {
        c.categories = v;
    }
    Ok(())
}

/// Member addresses from a `members` param: strings ("Name <a@b>") or contact ids (their first address;
/// a group id adds its members).
fn group_members(s: &Session, v: &Value) -> Result<Vec<String>, CmdError> {
    let items: Vec<Value> = match v {
        Value::Null => Vec::new(),
        Value::String(t) => t.split(';').map(|x| Value::String(x.to_owned())).collect(),
        Value::Array(a) if a.len() <= pim::MAX_LIST => a.clone(),
        _ => return Err(CmdError::Params("`members` must be a list of addresses or contact ids".into())),
    };
    let mut strings = Vec::new();
    for item in &items {
        match item {
            Value::String(t) if t.len() <= pim::MAX_TEXT => strings.push(t.clone()),
            Value::Number(n) => {
                let id = n.as_u64().map(ItemId);
                let c = id
                    .and_then(|i| s.mailbox.contacts.get(&i))
                    .ok_or_else(|| CmdError::Params(format!("no contact {n}")))?;
                strings.extend(contact_addresses(c));
            }
            _ => return Err(CmdError::Params("`members` must be a list of addresses or contact ids".into())),
        }
    }
    Ok(pim::parse_addresses(&strings).iter().map(mailcraft_model::Address::to_display_string).collect())
}

/// Addresses to use when mailing a contact: its first address with its name, or every member of a group.
pub fn contact_addresses(c: &Contact) -> Vec<String> {
    if is_group(c) {
        return c.emails.clone();
    }
    let Some(addr) = c.emails.iter().find(|e| !e.trim().is_empty()) else {
        return Vec::new();
    };
    let name = c.full_name();
    if name.is_empty() || addr.contains('<') {
        vec![addr.clone()]
    } else {
        vec![format!("{name} <{}>", addr.trim())]
    }
}

/// Summary of a contact for results.
pub fn contact_json(c: &Contact) -> Value {
    json!({
        "id": c.id.0,
        "folder": c.folder.0,
        "name": display_name(c),
        "fullName": c.full_name(),
        "fileAs": c.file_as,
        "group": is_group(c),
        "company": c.company,
        "jobTitle": c.job_title,
        "emails": c.emails,
        "phones": c.phones.iter().map(|(k, n)| json!({ "kind": k, "number": n })).collect::<Vec<_>>(),
        "categories": c.categories,
    })
}

/// Creates a contact in `folder` from field params; returns its id.
pub fn create_contact(s: &mut Session, folder: FolderId, p: &Value) -> Result<ItemId, CmdError> {
    let mut c = Contact { folder, ..Default::default() };
    apply_contact_fields(&mut c, p)?;
    if is_group(&c) {
        return Err(CmdError::Params(format!("notes cannot start with \"{CONTACT_GROUP_MARKER}\"")));
    }
    let id = ItemId(s.mailbox.alloc_id());
    c.id = id;
    s.mailbox.contacts.insert(id, c);
    Ok(id)
}

fn run_new(s: &mut Session, p: &Value) -> CmdResult {
    let folder = pim::new_item_folder(s, p, Kind::Contact)?;
    if !pim::has_any(p, &CONTACT_FIELDS) {
        return pim::dialog(s, "contact", json!({ "folder": folder.0, "focus": "fullName" }));
    }
    let id = create_contact(s, folder, p)?;
    Ok(s.mailbox.contacts.get(&id).map_or_else(|| json!({ "id": id.0 }), contact_json))
}

fn run_new_group(s: &mut Session, p: &Value) -> CmdResult {
    let folder = pim::new_item_folder(s, p, Kind::Contact)?;
    if !pim::has_any(p, &["name", "members", "notes", "categories"]) {
        return pim::dialog(s, "contactGroup", json!({ "folder": folder.0, "focus": "name" }));
    }
    if pim::text(p, "name")?.is_none_or(|n| n.trim().is_empty()) {
        return Err(CmdError::Params("`name` (string) required".into()));
    }
    let mut c = Contact { folder, notes: CONTACT_GROUP_MARKER.to_owned(), ..Default::default() };
    apply_group_fields(s, &mut c, p)?;
    let id = ItemId(s.mailbox.alloc_id());
    c.id = id;
    let out = contact_json(&c);
    s.mailbox.contacts.insert(id, c);
    let mut out = out;
    if let Some(o) = out.as_object_mut() {
        o.insert("id".into(), json!(id.0));
    }
    Ok(out)
}

fn run_update(s: &mut Session, p: &Value) -> CmdResult {
    let ids = pim::require_targets(s, p, Kind::Contact)?;
    let mut updated = Vec::new();
    for id in &ids {
        let Some(mut c) = s.mailbox.contacts.get(id).cloned() else { continue };
        if is_group(&c) {
            apply_group_fields(s, &mut c, p)?;
        } else {
            apply_contact_fields(&mut c, p)?;
            if is_group(&c) {
                return Err(CmdError::Params(format!("notes cannot start with \"{CONTACT_GROUP_MARKER}\"")));
            }
        }
        updated.push(c);
    }
    let out: Vec<Value> = updated.iter().map(contact_json).collect();
    for c in updated {
        s.mailbox.contacts.insert(c.id, c);
    }
    Ok(json!({ "updated": out }))
}

/// Contacts of `folder` in list order (by display name, then id).
fn ordered(s: &Session, folder: FolderId) -> Vec<ItemId> {
    let mut v: Vec<(String, ItemId)> = s
        .mailbox
        .contacts
        .values()
        .filter(|c| c.folder == folder)
        .map(|c| (display_name(c).to_lowercase(), c.id))
        .collect();
    v.sort();
    v.into_iter().map(|(_, id)| id).collect()
}

fn run_delete(s: &mut Session, p: &Value) -> CmdResult {
    let ids = pim::require_targets(s, p, Kind::Contact)?;
    let folder = ids.first().and_then(|id| pim::folder_of(&s.mailbox, *id));
    let order = folder.map(|f| ordered(s, f)).unwrap_or_default();
    let positions: Vec<usize> =
        order.iter().enumerate().filter(|(_, id)| ids.contains(id)).map(|(i, _)| i).collect();
    let neighbour = positions.iter().max().and_then(|&last| {
        order
            .iter()
            .skip(last.saturating_add(1))
            .find(|id| !ids.contains(id))
            .or_else(|| {
                order
                    .iter()
                    .take(positions.iter().min().copied().unwrap_or(0))
                    .rev()
                    .find(|id| !ids.contains(id))
            })
            .copied()
    });
    let done = pim::delete(s, &ids);
    if let Some(f) = folder
        && s.selection.folder == f
    {
        s.selection.items = neighbour.into_iter().collect();
        s.selection.focus = neighbour;
    } else {
        s.selection.items.retain(|i| !done.contains(i));
        if s.selection.focus.is_some_and(|f| done.contains(&f)) {
            s.selection.focus = None;
        }
    }
    Ok(json!({ "deleted": pim::ids_json(&done), "selected": neighbour.map(|n| n.0) }))
}

fn run_copy(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_copy(s, p, Kind::Contact)
}
fn run_move(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_move(s, p, Kind::Contact)
}
fn run_categorize(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_categorize(s, p, Kind::Contact)
}

/// Business card text: name, job title, company, addresses, phones, web page (what the card view and a
/// forwarded card's message body show).
pub fn business_card_lines(c: &Contact) -> Vec<String> {
    let mut lines = Vec::new();
    let name = display_name(c);
    let full = c.full_name();
    lines.push(if full.is_empty() { name } else { full });
    for v in [&c.job_title, &c.department, &c.company] {
        if !v.trim().is_empty() {
            lines.push(v.trim().to_owned());
        }
    }
    for (kind, number) in &c.phones {
        if !number.trim().is_empty() {
            lines.push(format!("{} {}", number.trim(), kind.trim()));
        }
    }
    for e in c.emails.iter().filter(|e| !e.trim().is_empty()) {
        lines.push(e.trim().to_owned());
    }
    for a in &c.addresses {
        let street = a.street.trim();
        let city_line = [a.city.trim(), a.region.trim(), a.postal_code.trim()]
            .iter()
            .filter(|x| !x.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        for l in [street, city_line.as_str(), a.country.trim()] {
            if !l.is_empty() {
                lines.push(l.to_owned());
            }
        }
    }
    if !c.web_page.trim().is_empty() {
        lines.push(c.web_page.trim().to_owned());
    }
    lines
}

/// File name for a contact's vCard ("Ann Lee.vcf").
fn vcard_name(c: &Contact) -> String {
    crate::compose::clean_attachment_name(&format!("{}.vcf", display_name(c)), "contact.vcf")
}

/// Forward Contact › As a Business Card / As an Outlook Contact: a new message with each contact attached as
/// a vCard (`text/vcard`, RFC 6350 via `mailcraft-formats`); a business card also puts the card text in the
/// body. Contact groups are skipped (a vCard has no member list).
fn run_forward(s: &mut Session, p: &Value) -> CmdResult {
    let as_kind = match opt_str(p, "as") {
        None | Some("contact") => "contact",
        Some("businessCard") => "businessCard",
        Some(_) => return Err(CmdError::Params("`as` must be businessCard|contact".into())),
    };
    let ids = pim::require_targets(s, p, Kind::Contact)?;
    let contacts: Vec<Contact> =
        ids.iter().filter_map(|i| s.mailbox.contacts.get(i)).filter(|c| !is_group(c)).cloned().collect();
    if contacts.is_empty() {
        return Err(CmdError::Params("contact groups cannot be forwarded as business cards".into()));
    }
    let names: Vec<String> = contacts.iter().map(display_name).collect();
    let subject = match names.as_slice() {
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} and {} more", rest.len()),
        [] => String::new(),
    };
    let body = if as_kind == "businessCard" {
        contacts.iter().map(|c| business_card_lines(c).join("\n")).collect::<Vec<_>>().join("\n\n")
    } else {
        String::new()
    };
    let attachments = contacts
        .iter()
        .map(|c| crate::compose::ComposeAttachment {
            filename: vcard_name(c),
            mime_type: "text/vcard".into(),
            data: mailcraft_formats::vcf::write(std::slice::from_ref(c), mailcraft_formats::vcf::Version::V3)
                .into_bytes(),
        })
        .collect();
    let mut compose = crate::compose::ComposeState {
        subject,
        body: mailcraft_model::Doc::from_plain_text(&body),
        attachments,
        dirty: true,
        ..Default::default()
    };
    crate::compose::insert_default_signature(&s.mailbox, &mut compose);
    let cid = s.open_compose(compose);
    Ok(json!({ "compose": cid, "as": as_kind, "contacts": contacts.len() }))
}

fn run_mail_merge(s: &mut Session, p: &Value) -> CmdResult {
    let contacts = match opt_str(p, "contacts") {
        None | Some("all") => "all",
        Some("selected") => "selected",
        Some(_) => return Err(CmdError::Params("`contacts` must be all|selected".into())),
    };
    let output = match opt_str(p, "output") {
        None | Some("letters") => "letters",
        Some(o @ ("email" | "labels" | "envelopes")) => o,
        Some(_) => return Err(CmdError::Params("`output` must be letters|email|labels|envelopes".into())),
    };
    let ids = if contacts == "selected" {
        pim::require_targets(s, p, Kind::Contact)?
    } else {
        pim::current_folder(s, Kind::Contact).map(|f| ordered(s, f)).unwrap_or_default()
    };
    pim::dialog(s, "mailMerge", json!({ "contacts": contacts, "output": output, "ids": pim::ids_json(&ids) }))
}

/// Addresses of the target contacts (groups expand to members), de-duplicated.
fn target_addresses(s: &Session, p: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in pim::targets(s, p, Kind::Contact) {
        if let Some(c) = s.mailbox.contacts.get(&id) {
            for a in contact_addresses(c) {
                if !out.contains(&a) {
                    out.push(a);
                }
            }
        }
    }
    out
}

fn run_meeting(s: &mut Session, p: &Value) -> CmdResult {
    let attendees = target_addresses(s, p);
    let mut params = json!({ "attendees": attendees });
    if let (Some(o), Some(extra)) = (params.as_object_mut(), p.as_object()) {
        for (k, v) in extra {
            if !matches!(k.as_str(), "ids" | "id" | "attendees" | "folder") {
                o.insert(k.clone(), v.clone());
            }
        }
    }
    calendar::create_meeting(s, &params)
}

fn run_assign_task(s: &mut Session, p: &Value) -> CmdResult {
    let to = target_addresses(s, p).join("; ");
    let folder = pim::default_folder(s, Kind::Task).map(|f| f.0);
    pim::dialog(s, "taskRequest", json!({ "to": to, "folder": folder, "focus": "subject" }))
}

/// Contact prefilled from `c`: company, business address and business phone numbers.
fn same_company(c: &Contact, folder: FolderId) -> Contact {
    Contact {
        folder,
        company: c.company.clone(),
        addresses: c.addresses.iter().filter(|a| a.kind.eq_ignore_ascii_case("Business")).cloned().collect(),
        phones: c
            .phones
            .iter()
            .filter(|(k, _)| k.to_ascii_lowercase().starts_with("business"))
            .cloned()
            .collect(),
        ..Default::default()
    }
}

fn run_new_from_same_company(s: &mut Session, p: &Value) -> CmdResult {
    let source = pim::targets(s, p, Kind::Contact)
        .into_iter()
        .find_map(|id| s.mailbox.contacts.get(&id).filter(|c| !is_group(c)).cloned())
        .ok_or_else(|| CmdError::Params("select a contact first".into()))?;
    let folder = if pim::is_kind_folder(s, source.folder, Kind::Contact) {
        source.folder
    } else {
        pim::new_item_folder(s, &Value::Null, Kind::Contact)?
    };
    let mut c = same_company(&source, folder);
    if !pim::has_any(p, &CONTACT_FIELDS) {
        let prefill = json!({
            "company": c.company,
            "businessAddress": c.addresses.first().map(|a| json!({
                "street": a.street, "city": a.city, "region": a.region,
                "postalCode": a.postal_code, "country": a.country,
            })),
            "phones": c.phones.iter().map(|(k, n)| (k.clone(), json!(n))).collect::<serde_json::Map<_, _>>(),
        });
        return pim::dialog(
            s,
            "contact",
            json!({ "folder": folder.0, "focus": "fullName", "prefill": prefill }),
        );
    }
    apply_contact_fields(&mut c, p)?;
    if is_group(&c) {
        return Err(CmdError::Params(format!("notes cannot start with \"{CONTACT_GROUP_MARKER}\"")));
    }
    let id = ItemId(s.mailbox.alloc_id());
    c.id = id;
    let out = contact_json(&c);
    s.mailbox.contacts.insert(id, c);
    Ok(out)
}

/// Contacts (and groups) in contacts folders matching `text` by name, company or address.
pub fn find_contacts(s: &Session, text: &str) -> Vec<ItemId> {
    let text = text.trim();
    let mut v: Vec<(String, ItemId)> = s
        .mailbox
        .contacts
        .values()
        .filter(|c| pim::is_kind_folder(s, c.folder, Kind::Contact))
        .filter(|c| {
            contains_ci(&display_name(c), text)
                || contains_ci(&c.full_name(), text)
                || contains_ci(&c.company, text)
                || (!is_group(c) && c.emails.iter().any(|e| contains_ci(e, text)))
        })
        .map(|c| (display_name(c).to_lowercase(), c.id))
        .collect();
    v.sort();
    v.into_iter().map(|(_, id)| id).collect()
}

fn run_search(s: &mut Session, p: &Value) -> CmdResult {
    let text = match pim::text(p, "text")? {
        Some(t) if !t.trim().is_empty() => t,
        _ => return Ok(json!({ "focus": "searchPeople" })),
    };
    let ids = find_contacts(s, &text);
    let matches: Vec<Value> =
        ids.iter().filter_map(|id| s.mailbox.contacts.get(id)).map(contact_json).collect();
    let dialog = match ids.as_slice() {
        [] => None,
        [one] => Some(("contactCard", json!({ "id": one.0 }))),
        many => Some(("chooseContact", json!({ "ids": pim::ids_json(many), "text": text }))),
    };
    let mut out = json!({ "matches": matches });
    if let Some((name, params)) = dialog {
        s.notify(crate::Notice::OpenDialog(name.to_owned(), params));
        if let Some(o) = out.as_object_mut() {
            o.insert("dialog".into(), json!(name));
        }
    }
    Ok(out)
}

fn contact_selected(s: &Session) -> Option<&'static str> {
    if pim::selected(s, Kind::Contact).is_empty() { Some("no contact selected") } else { None }
}

fn single_contact_selected(s: &Session) -> Option<&'static str> {
    let any = pim::selected(s, Kind::Contact)
        .iter()
        .any(|id| s.mailbox.contacts.get(id).is_some_and(|c| !is_group(c)));
    if any { None } else { Some("no contact selected") }
}

/// Validates a "YYYY-MM-DD" date string (exposed for the contact window).
pub fn valid_birthday(b: &str) -> bool {
    let mut it = b.split('-');
    let (Some(y), Some(m), Some(d), None) = (it.next(), it.next(), it.next(), it.next()) else {
        return false;
    };
    match (y.parse::<i64>(), m.parse::<u8>(), d.parse::<u8>()) {
        (Ok(y), Ok(m), Ok(d)) => dates::days_from_civil(y, m, d).is_some(),
        _ => false,
    }
}

pub fn specs() -> Vec<CommandSpec> {
    let spec = |id, label, location, shortcut, params, mutates, enabled, run| CommandSpec {
        id,
        label,
        location,
        shortcut,
        params,
        mutates,
        enabled,
        run,
    };
    const FIELDS: &str = r#"{"fullName"?, "firstName"?, "middleName"?, "lastName"?, "title"?, "suffix"?, "fileAs"?, "company"?, "jobTitle"?, "department"?, "email"?, "email2"?, "email3"?, "emails"?: [string], "phones"?: {"Business": string, "Mobile": string, ...}, "addresses"?: [{"kind","street","city","region","postalCode","country"}], "businessAddress"?: {...}, "webPage"?, "im"?, "birthday"?: "YYYY-MM-DD", "notes"?, "categories"?: [string], "folder"?: number} (no fields = open a blank contact window)"#;
    vec![
        spec(
            "people.assignTask",
            "Assign Task",
            "People › Home › Communicate › More",
            "",
            r#"{"ids"?: [number]}"#,
            false,
            always,
            run_assign_task,
        ),
        spec(
            "people.delete",
            "Delete",
            "People › Home › Delete",
            "Delete",
            r#"{"ids"?: [number]}"#,
            true,
            contact_selected,
            run_delete,
        ),
        spec(
            "people.forward",
            "Forward Contact",
            "People › Home › Share › Forward Contact",
            "Mod+F",
            r#"{"as": "businessCard|contact", "ids"?: [number]}"#,
            false,
            always,
            run_forward,
        ),
        spec(
            "people.mailMerge",
            "Mail Merge",
            "People › Home › Actions",
            "",
            r#"{"contacts": "all|selected", "output": "letters|email|labels|envelopes"}"#,
            false,
            always,
            run_mail_merge,
        ),
        spec(
            "people.meeting",
            "Meeting",
            "People › Home › Communicate",
            "",
            r#"{"ids"?: [number], ...appointment fields}"#,
            true,
            always,
            run_meeting,
        ),
        spec(
            "people.new",
            "New Contact",
            "People › Home › New; Home › New Items › Contact",
            "Mod+Shift+C",
            FIELDS,
            true,
            always,
            run_new,
        ),
        spec(
            "people.newFromSameCompany",
            "Contact from the Same Company",
            "People › Home › New Items",
            "",
            r#"{"ids"?: [number], ...contact fields} (no fields = open the prefilled contact window)"#,
            true,
            single_contact_selected,
            run_new_from_same_company,
        ),
        spec(
            "people.newGroup",
            "New Contact Group",
            "People › Home › New; Home › New Items › Contact Group",
            "Mod+Shift+L",
            r#"{"name"?: string, "members"?: [string | contact id], "notes"?: string, "folder"?: number} (no fields = open the contact group window)"#,
            true,
            always,
            run_new_group,
        ),
        spec(
            "people.search",
            "Search People",
            "Home › Find",
            "F11",
            r#"{"text": string}"#,
            false,
            always,
            run_search,
        ),
        spec(
            "people.update",
            "Edit Contact",
            "People › Item",
            "",
            r#"{"id": number | "ids": [number], ...contact fields | group fields {"name", "members", "notes"}}"#,
            true,
            always,
            run_update,
        ),
        spec(
            "people.copy",
            "Copy to Folder",
            "People › Item",
            "",
            r#"{"folder": number, "id"?: number, "ids"?: [number]}"#,
            true,
            always,
            run_copy,
        ),
        spec(
            "people.move",
            "Move to Folder",
            "People › Home › Actions › Move",
            "",
            r#"{"folder": number, "id"?: number, "ids"?: [number]}"#,
            true,
            always,
            run_move,
        ),
        spec(
            "people.categorize",
            "Categorize",
            "People › Home › Tags",
            "",
            r#"{"name": string, "on"?: bool, "clear"?: bool, "id"?: number, "ids"?: [number]}"#,
            true,
            always,
            run_categorize,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::calendar::tests::{role, session};
    use crate::{Notice, Selection};
    use mailcraft_model::FolderRole;

    fn select(s: &mut Session, ids: &[u64]) {
        let folder = role(s, FolderRole::Contacts);
        s.selection = Selection {
            folder,
            items: ids.iter().map(|i| ItemId(*i)).collect(),
            focus: ids.first().map(|i| ItemId(*i)),
        };
    }

    fn add(s: &mut Session, p: Value) -> u64 {
        s.run("people.new", &p).unwrap()["id"].as_u64().unwrap()
    }

    #[test]
    fn new_without_fields_opens_window() {
        let mut s = session();
        let r = s.run("people.new", &json!({})).unwrap();
        assert_eq!(r["dialog"], "contact");
        assert!(
            matches!(s.notices.last(), Some(Notice::OpenDialog(n, p)) if n == "contact" && p["focus"] == "fullName")
        );
        assert!(s.mailbox.contacts.is_empty());
    }

    #[test]
    fn new_contact_with_fields() {
        let mut s = session();
        let r = s
            .run(
                "people.new",
                &json!({"fullName": "Ann Marie Lee", "email": "ann@example.com", "company": "Example Co",
                        "phones": {"Business": "+1 555 0100"}, "birthday": "1990-02-28"}),
            )
            .unwrap();
        let c = &s.mailbox.contacts[&ItemId(r["id"].as_u64().unwrap())];
        assert_eq!(
            (c.first_name.as_str(), c.middle_name.as_str(), c.last_name.as_str()),
            ("Ann", "Marie", "Lee")
        );
        assert_eq!(c.file_as, "Lee, Ann");
        assert_eq!(c.folder, role(&s, FolderRole::Contacts));
        assert_eq!(c.phones, vec![("Business".to_string(), "+1 555 0100".to_string())]);
        assert!(matches!(s.run("people.new", &json!({"birthday": "1990-02-30"})), Err(CmdError::Params(_))));
        assert!(matches!(
            s.run("people.new", &json!({"emails": ["a@x", "b@x", "c@x", "d@x"]})),
            Err(CmdError::Params(_))
        ));
        assert!(s.mailbox.validate().is_empty());
    }

    #[test]
    fn update_recomputes_auto_file_as_but_keeps_custom() {
        let mut s = session();
        let id = add(&mut s, json!({"firstName": "Bo", "lastName": "Ek"}));
        s.run("people.update", &json!({"id": id, "lastName": "Berg"})).unwrap();
        assert_eq!(s.mailbox.contacts[&ItemId(id)].file_as, "Berg, Bo");
        s.run("people.update", &json!({"id": id, "fileAs": "Boss"})).unwrap();
        s.run("people.update", &json!({"id": id, "firstName": "Bob"})).unwrap();
        assert_eq!(s.mailbox.contacts[&ItemId(id)].file_as, "Boss");
    }

    #[test]
    fn contact_group_round_trip() {
        let mut s = session();
        let ann = add(&mut s, json!({"fullName": "Ann Lee", "email": "ann@example.com"}));
        assert_eq!(s.run("people.newGroup", &json!({})).unwrap()["dialog"], "contactGroup");
        assert!(matches!(s.run("people.newGroup", &json!({"members": ["x@y"]})), Err(CmdError::Params(_))));
        let r = s
            .run("people.newGroup", &json!({"name": "Team", "members": [ann, "Bob <bob@example.org>"]}))
            .unwrap();
        let gid = ItemId(r["id"].as_u64().unwrap());
        let g = &s.mailbox.contacts[&gid];
        assert!(is_group(g));
        assert_eq!(display_name(g), "Team");
        assert_eq!(
            g.emails,
            vec!["Ann Lee <ann@example.com>".to_string(), "Bob <bob@example.org>".to_string()]
        );
        s.run("people.update", &json!({"id": gid.0, "notes": "weekly", "members": ["c@example.net"]}))
            .unwrap();
        let g = &s.mailbox.contacts[&gid];
        assert_eq!(g.notes, "Contact Group\nweekly");
        assert_eq!(g.emails, vec!["c@example.net".to_string()]);
        assert!(r["group"].as_bool().unwrap());
    }

    #[test]
    fn delete_selects_neighbour_and_is_disabled_without_selection() {
        let mut s = session();
        let a = add(&mut s, json!({"fullName": "Ann A"}));
        let b = add(&mut s, json!({"fullName": "Bob B"}));
        let c = add(&mut s, json!({"fullName": "Cy C"}));
        select(&mut s, &[]);
        assert!(matches!(s.run("people.delete", &json!({})), Err(CmdError::Disabled(_))));
        select(&mut s, &[b]);
        let r = s.run("people.delete", &json!({})).unwrap();
        assert_eq!(r["selected"], c);
        assert_eq!(s.selection.focus, Some(ItemId(c)));
        assert_eq!(s.mailbox.contacts[&ItemId(b)].folder, role(&s, FolderRole::DeletedItems));
        select(&mut s, &[c]);
        let r = s.run("people.delete", &json!({})).unwrap();
        assert_eq!(r["selected"], a);
    }

    #[test]
    fn forward_and_mail_merge_open_dialogs() {
        let mut s = session();
        let a = add(&mut s, json!({"fullName": "Ann A"}));
        select(&mut s, &[a]);
        let r = s.run("people.forward", &json!({"as": "fax"}));
        assert!(matches!(r, Err(CmdError::Params(_))));
        let r = s.run("people.mailMerge", &json!({"contacts": "all", "output": "labels"})).unwrap();
        assert_eq!(r["dialog"], "mailMerge");
        assert!(matches!(s.notices.last(), Some(Notice::OpenDialog(_, p)) if p["ids"] == json!([a])));
        assert!(matches!(s.run("people.mailMerge", &json!({"output": "fax"})), Err(CmdError::Params(_))));
    }

    #[test]
    fn forward_attaches_vcards() {
        let mut s = session();
        let a = add(
            &mut s,
            json!({"fullName": "Ann Lee", "email": "ann@example.com", "company": "Example Co.", "jobTitle": "Editor",
                   "phones": {"Mobile": "+1 555 0100"}}),
        );
        let g =
            s.run("people.newGroup", &json!({"name": "Team", "members": ["x@example.com"]})).unwrap()["id"]
                .as_u64()
                .unwrap();
        select(&mut s, &[a, g]);
        let r = s.run("people.forward", &json!({"as": "businessCard"})).unwrap();
        assert_eq!(r["contacts"], 1, "the group is skipped");
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.subject, "Lee, Ann");
        assert_eq!(c.attachments.len(), 1);
        assert_eq!(c.attachments[0].filename, "Lee, Ann.vcf");
        assert_eq!(c.attachments[0].mime_type, "text/vcard");
        let card = String::from_utf8(c.attachments[0].data.clone()).unwrap();
        assert!(card.starts_with("BEGIN:VCARD"), "{card}");
        assert!(card.contains("ann@example.com"));
        let back = mailcraft_formats::vcf::read_str(&card);
        assert_eq!(back[0].emails, vec!["ann@example.com".to_string()]);
        let text = c.body.to_plain_text();
        assert!(
            text.contains("Ann Lee") && text.contains("Editor") && text.contains("+1 555 0100 Mobile"),
            "{text}"
        );
        // As an Outlook contact: the vCard alone.
        let r = s.run("people.forward", &json!({"as": "contact", "ids": [a]})).unwrap();
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert!(c.body.to_plain_text().trim().is_empty());
        assert!(matches!(s.run("people.forward", &json!({"ids": [g]})), Err(CmdError::Params(_))));
    }

    #[test]
    fn meeting_and_assign_task_use_selected_addresses() {
        let mut s = session();
        let a = add(&mut s, json!({"fullName": "Ann Lee", "email": "ann@example.com"}));
        let b = add(&mut s, json!({"fullName": "No Mail"}));
        select(&mut s, &[a, b]);
        let r = s.run("people.meeting", &json!({})).unwrap();
        let appt = &s.mailbox.appointments[&ItemId(r["id"].as_u64().unwrap())];
        assert_eq!(appt.attendees.len(), 1);
        assert_eq!(appt.folder, role(&s, FolderRole::Calendar));
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.to, "Ann Lee <ann@example.com>");
        let r = s.run("people.assignTask", &json!({})).unwrap();
        assert_eq!(r["dialog"], "taskRequest");
        assert!(
            matches!(s.notices.last(), Some(Notice::OpenDialog(_, p)) if p["to"] == "Ann Lee <ann@example.com>")
        );
    }

    #[test]
    fn new_from_same_company_prefills() {
        let mut s = session();
        let a = add(
            &mut s,
            json!({"fullName": "Ann Lee", "company": "Example Co", "phones": {"Business": "1", "Mobile": "2"},
                   "businessAddress": {"street": "1 Main", "city": "Town"}}),
        );
        select(&mut s, &[]);
        assert!(matches!(s.run("people.newFromSameCompany", &json!({})), Err(CmdError::Disabled(_))));
        select(&mut s, &[a]);
        let r = s.run("people.newFromSameCompany", &json!({})).unwrap();
        assert_eq!(r["dialog"], "contact");
        assert!(matches!(s.notices.last(), Some(Notice::OpenDialog(_, p))
            if p["prefill"]["company"] == "Example Co" && p["prefill"]["phones"] == json!({"Business": "1"})
               && p["prefill"]["businessAddress"]["city"] == "Town"));
        let r = s.run("people.newFromSameCompany", &json!({"fullName": "Bo Ek"})).unwrap();
        let c = &s.mailbox.contacts[&ItemId(r["id"].as_u64().unwrap())];
        assert_eq!(c.company, "Example Co");
        assert_eq!(c.phones.len(), 1);
        assert_eq!(c.addresses.len(), 1);
        // Notes cannot turn the new contact into a contact group (same rule as people.new).
        let before = s.mailbox.contacts.len();
        let r = s.run("people.newFromSameCompany", &json!({"fullName": "Cy", "notes": "Contact Group\nx"}));
        assert!(matches!(r, Err(CmdError::Params(_))), "{r:?}");
        assert_eq!(s.mailbox.contacts.len(), before);
    }

    #[test]
    fn search_finds_and_opens_card() {
        let mut s = session();
        let a = add(&mut s, json!({"fullName": "Ann Lee", "email": "ann@example.com"}));
        add(&mut s, json!({"fullName": "Anna Berg"}));
        assert_eq!(s.run("people.search", &json!({})).unwrap()["focus"], "searchPeople");
        let r = s.run("people.search", &json!({"text": "lee"})).unwrap();
        assert_eq!(r["dialog"], "contactCard");
        assert!(matches!(s.notices.last(), Some(Notice::OpenDialog(_, p)) if p["id"] == a));
        let r = s.run("people.search", &json!({"text": "ann"})).unwrap();
        assert_eq!(r["matches"].as_array().unwrap().len(), 2);
        assert_eq!(r["dialog"], "chooseContact");
        let r = s.run("people.search", &json!({"text": "zzz"})).unwrap();
        assert!(r["matches"].as_array().unwrap().is_empty());
    }

    #[test]
    fn copy_move_categorize_contacts() {
        let mut s = session();
        let a = add(&mut s, json!({"fullName": "Ann Lee"}));
        let root = role(&s, FolderRole::Contacts);
        let sub = s.mailbox.create_folder(root, "Clients").unwrap();
        let r = s.run("people.copy", &json!({"id": a, "folder": sub.0})).unwrap();
        assert_eq!(r["copies"].as_array().unwrap().len(), 1);
        s.run("people.move", &json!({"id": a, "folder": sub.0})).unwrap();
        assert_eq!(s.mailbox.contacts[&ItemId(a)].folder, sub);
        s.run("people.categorize", &json!({"id": a, "name": "Blue category", "on": true})).unwrap();
        assert_eq!(s.mailbox.contacts[&ItemId(a)].categories, vec!["Blue category".to_string()]);
        assert!(valid_birthday("2000-02-29"));
        assert!(!valid_birthday("2001-02-29"));
    }
}
