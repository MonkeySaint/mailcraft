//! Contacts, calendar and tasks of an Exchange account over EWS ([MS-OXWSCONT] contacts, [MS-OXWSCAL]
//! calendar items with their recurrence, modified and deleted occurrences, [MS-OXWSTASK] tasks), through the
//! same folder synchronisation as CardDAV / CalDAV and JMAP (`dav.rs`).
//!
//! **One synchronisation for every protocol.** As `jmap_pim.rs` does for JMAP, [`Store`] shows the
//! synchroniser an EWS folder the way a WebDAV collection looks: the folder's address is
//! `https://ews.invalid/<contacts|calendar|tasks>/<folder id>/` (the reserved `.invalid` name never reaches
//! the network), an item's address comes from its UID like a resource name made here
//! ([`pim::resource_url`]), and its entity tag is the item's EWS change key. Items are read with GetItem
//! into the model and handed over as MailCraft's own vCard / iCalendar text; writes read that text back
//! into the model and send its properties (CreateItem, or UpdateItem with only the properties whose value
//! differs from the server's item read the same way, so what MailCraft does not show is left alone).
//! Writes carry the change key last read: an item changed on the server meanwhile is a failed
//! precondition (HTTP 412), as on a DAV server.
//!
//! **UIDs.** Contacts and tasks have no UID on Exchange: MailCraft writes its own into a named property
//! (`MailCraftUid`) so the address stays the same. Calendar items have one (`UID`); an item made elsewhere
//! without either gets a UID from its item id.
//!
//! **Meetings** are created and updated with `SendMeetingInvitations="SendToNone"`: MailCraft mails
//! invitations itself, the server never does on its behalf. A series' modified occurrences are read as
//! exceptions (RECURRENCE-ID) and its deleted occurrences as exception dates; edits of single occurrences
//! made here (a removed or changed occurrence) are not written back yet: such an update is refused with an
//! error the user sees, instead of reporting the series as saved while the server keeps the old occurrence.

use std::cell::RefCell;
use std::collections::BTreeMap;

use mailcraft_model::calendar::{Attendee, AttendeeRole, BusyStatus, Recurrence, ResponseStatus};
use mailcraft_model::contact::PostalAddress;
use mailcraft_model::task::{TaskPriority, TaskStatus};
use mailcraft_model::{Appointment, Contact, DateTime, Importance, Task};
use mailcraft_net::NetError;
use mailcraft_net::autoconfig::Element;
use mailcraft_net::dav::{Fetched, Listing, Precondition, Resource};
use mailcraft_net::ews::{EwsClient, ITEM_UID, esc, extended_named, is_code, strings};
use mailcraft_net::http::{percent_decode, percent_encode};

use super::ews::parse_datetime;
use super::pim::{self, PimKind};

pub(super) use super::pim::EWS_ORIGIN as ORIGIN;

fn segment(kind: PimKind) -> &'static str {
    match kind {
        PimKind::Contacts => "contacts",
        PimKind::Tasks => "tasks",
        PimKind::Calendar => "calendar",
    }
}

/// The distinguished folder of `kind`.
fn distinguished(kind: PimKind) -> &'static str {
    segment(kind)
}

/// The address an EWS folder gets.
pub(super) fn collection_url(kind: PimKind, folder: &str) -> String {
    format!("{ORIGIN}/{}/{}/", segment(kind), percent_encode(folder))
}

/// The EWS folder id of the collection at `url`.
fn collection_id(url: &str, kind: PimKind) -> Option<String> {
    let rest = url.trim().get(ORIGIN.len()..)?.strip_prefix('/')?;
    let rest = rest.strip_prefix(segment(kind))?.strip_prefix('/')?;
    let id = rest.trim_end_matches('/');
    if id.is_empty() || id.contains('/') {
        return None;
    }
    Some(percent_decode(id, false))
}

/// The account's folder of `kind`: (address, name).
pub(super) fn collections(c: &EwsClient, kind: PimKind) -> Result<Vec<(String, String)>, NetError> {
    let found = c.distinguished(&[distinguished(kind)])?;
    Ok(found
        .into_iter()
        .filter_map(|(_, f)| f)
        .map(|f| {
            (collection_url(kind, &f.id), if f.name.is_empty() { kind.label().to_owned() } else { f.name })
        })
        .collect())
}

fn http(status: u16, what: &str) -> NetError {
    NetError::Http { status, body: what.to_owned() }
}

fn tag(change_key: &str) -> String {
    format!("\"{change_key}\"")
}

const LIST_SHAPE_CAL: &str = concat!(
    "<t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>",
    r#"<t:FieldURI FieldURI="calendar:UID"/><t:FieldURI FieldURI="calendar:CalendarItemType"/>"#,
    r#"<t:ExtendedFieldURI DistinguishedPropertySetId="PublicStrings" PropertyName="MailCraftUid" PropertyType="String"/>"#,
    "</t:AdditionalProperties>"
);
const LIST_SHAPE: &str = concat!(
    "<t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>",
    r#"<t:ExtendedFieldURI DistinguishedPropertySetId="PublicStrings" PropertyName="MailCraftUid" PropertyType="String"/>"#,
    "</t:AdditionalProperties>"
);
const FULL_SHAPE: &str = concat!(
    "<t:BaseShape>AllProperties</t:BaseShape><t:BodyType>Text</t:BodyType><t:AdditionalProperties>",
    r#"<t:ExtendedFieldURI DistinguishedPropertySetId="PublicStrings" PropertyName="MailCraftUid" PropertyType="String"/>"#,
    "</t:AdditionalProperties>"
);
const FULL_SHAPE_CAL: &str = concat!(
    "<t:BaseShape>AllProperties</t:BaseShape><t:BodyType>Text</t:BodyType><t:AdditionalProperties>",
    r#"<t:ExtendedFieldURI DistinguishedPropertySetId="PublicStrings" PropertyName="MailCraftUid" PropertyType="String"/>"#,
    r#"<t:FieldURI FieldURI="calendar:RequiredAttendees"/><t:FieldURI FieldURI="calendar:OptionalAttendees"/>"#,
    r#"<t:FieldURI FieldURI="calendar:Resources"/><t:FieldURI FieldURI="calendar:ModifiedOccurrences"/>"#,
    r#"<t:FieldURI FieldURI="calendar:DeletedOccurrences"/>"#,
    "</t:AdditionalProperties>"
);

/// The UID of a listed or read item.
/// What an iCalendar object holds besides the series' own fields: its deleted occurrences (EXDATE, Unix
/// times) and its changed ones (RECURRENCE-ID, with a fingerprint of each). `None` when it holds no event.
pub(super) fn occurrence_signature(data: &str, tz_offset_min: i32) -> Option<(Vec<i64>, Vec<(i64, u64)>)> {
    let (items, _) = pim::read_events(data, tz_offset_min)?;
    let mut deleted: Vec<i64> = items
        .iter()
        .filter(|a| a.recurrence_id.is_none())
        .filter_map(|a| a.recurrence.as_ref())
        .flat_map(|r| r.exdates.iter().map(|d| d.unix))
        .collect();
    deleted.sort_unstable();
    deleted.dedup();
    let mut changed: Vec<(i64, u64)> =
        items.iter().filter_map(|a| Some((a.recurrence_id?.unix, pim::event_fingerprint(&[a])))).collect();
    changed.sort_unstable();
    Some((deleted, changed))
}

fn uid_of(el: &Element, id: &str) -> String {
    extended_named(el, "MailCraftUid")
        .or_else(|| el.child_text("UID").map(str::to_owned))
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| format!("ews-{:016x}", pim::fnv(id.as_bytes())))
}

fn item_id(el: &Element) -> Option<(String, String)> {
    let i = el.child("ItemId")?;
    Some((i.attr("Id")?.to_owned(), i.attr("ChangeKey").unwrap_or_default().to_owned()))
}

// ---------------------------------------------------------------------------------------------------
// Fields: the properties MailCraft maps, in schema order, for creating and for updating
// ---------------------------------------------------------------------------------------------------

/// One mapped property of an item.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Field {
    /// The path element (`<t:FieldURI …/>`, `<t:IndexedFieldURI …/>`).
    uri: String,
    /// Indexed properties: the container element and the entry key (`EmailAddresses`, `EmailAddress1`);
    /// physical address parts also name the part (`Street`).
    indexed: Option<(&'static str, String, Option<&'static str>)>,
    /// The value: the property element (or, indexed, the entry's content), `None` when empty.
    value: Option<String>,
}

fn simple(uri: &str, el: &str, text: Option<String>) -> Field {
    Field {
        uri: format!(r#"<t:FieldURI FieldURI="{uri}"/>"#),
        indexed: None,
        value: text.filter(|t| !t.trim().is_empty()).map(|t| format!("<t:{el}>{}</t:{el}>", esc(&t))),
    }
}

/// A property whose value is already XML (`None` when absent).
fn raw(uri: &str, xml: Option<String>) -> Field {
    Field { uri: format!(r#"<t:FieldURI FieldURI="{uri}"/>"#), indexed: None, value: xml }
}

fn indexed(uri: &str, container: &'static str, key: &str, text: Option<&str>) -> Field {
    Field {
        uri: format!(r#"<t:IndexedFieldURI FieldURI="{uri}" FieldIndex="{key}"/>"#),
        indexed: Some((container, key.to_owned(), None)),
        value: text.map(str::trim).filter(|t| !t.is_empty()).map(esc),
    }
}

fn address_part(key: &str, part: &'static str, text: &str) -> Field {
    Field {
        uri: format!(r#"<t:IndexedFieldURI FieldURI="contacts:PhysicalAddress:{part}" FieldIndex="{key}"/>"#),
        indexed: Some(("PhysicalAddresses", key.to_owned(), Some(part))),
        value: Some(text.trim())
            .filter(|t| !t.is_empty())
            .map(|t| format!("<t:{part}>{}</t:{part}>", esc(t))),
    }
}

fn dt(d: DateTime) -> String {
    d.to_rfc3339()
}

fn importance(i: Importance) -> &'static str {
    match i {
        Importance::Low => "Low",
        Importance::Normal => "Normal",
        Importance::High => "High",
    }
}

fn importance_of(s: Option<&str>) -> Importance {
    match s {
        Some("Low") => Importance::Low,
        Some("High") => Importance::High,
        _ => Importance::Normal,
    }
}

fn mailbox_xml(m: &mailcraft_mime::Mailbox) -> String {
    let name = m
        .name
        .as_deref()
        .filter(|n| !n.trim().is_empty())
        .map(|n| format!("<t:Name>{}</t:Name>", esc(n)))
        .unwrap_or_default();
    format!("<t:Mailbox>{name}<t:EmailAddress>{}</t:EmailAddress></t:Mailbox>", esc(&m.addr))
}

fn mailbox_of(el: &Element) -> Option<mailcraft_mime::Mailbox> {
    let m = el.child("Mailbox")?;
    let addr = m.child_text("EmailAddress")?.to_owned();
    let name = m.child_text("Name").map(str::to_owned).filter(|n| !n.is_empty());
    Some(mailcraft_mime::Mailbox { name, addr })
}

/// The phone keys of EWS ([MS-OXWSCDATA] PhoneNumberKeyType) for MailCraft's phone kinds.
const PHONES: [(&str, &str); 11] = [
    ("Business", "BusinessPhone"),
    ("Business 2", "BusinessPhone2"),
    ("Home", "HomePhone"),
    ("Home 2", "HomePhone2"),
    ("Mobile", "MobilePhone"),
    ("Business Fax", "BusinessFax"),
    ("Home Fax", "HomeFax"),
    ("Pager", "Pager"),
    ("Car", "CarPhone"),
    ("Assistant", "AssistantPhone"),
    ("Other", "OtherTelephone"),
];
const ADDRESSES: [&str; 3] = ["Business", "Home", "Other"];
const ADDRESS_PARTS: [&str; 5] = ["Street", "City", "State", "CountryOrRegion", "PostalCode"];

fn contact_fields(c: &Contact) -> Vec<Field> {
    let mut f = vec![
        simple(
            "item:Sensitivity",
            "Sensitivity",
            Some(if c.private { "Private" } else { "Normal" }.to_owned()),
        ),
        simple("item:Body", "Body", Some(c.notes.clone())),
        raw("item:Categories", categories_xml(&c.categories)),
        simple("contacts:FileAs", "FileAs", Some(c.file_as.clone())),
        simple("contacts:DisplayName", "DisplayName", Some(c.full_name())),
        simple("contacts:GivenName", "GivenName", Some(c.first_name.clone())),
        simple("contacts:MiddleName", "MiddleName", Some(c.middle_name.clone())),
        simple("contacts:Nickname", "Nickname", Some(c.nickname.clone())),
        simple("contacts:CompanyName", "CompanyName", Some(c.company.clone())),
    ];
    for (i, key) in ["EmailAddress1", "EmailAddress2", "EmailAddress3"].iter().enumerate() {
        f.push(indexed("contacts:EmailAddress", "EmailAddresses", key, c.emails.get(i).map(String::as_str)));
    }
    for kind in ADDRESSES {
        let a = c.addresses.iter().find(|a| a.kind.eq_ignore_ascii_case(kind));
        let parts = [
            a.map(|a| a.street.as_str()),
            a.map(|a| a.city.as_str()),
            a.map(|a| a.region.as_str()),
            a.map(|a| a.country.as_str()),
            a.map(|a| a.postal_code.as_str()),
        ];
        for (part, text) in ADDRESS_PARTS.iter().zip(parts) {
            f.push(address_part(kind, part, text.unwrap_or_default()));
        }
    }
    for (label, key) in PHONES {
        let n = c.phones.iter().find(|(k, _)| k.eq_ignore_ascii_case(label)).map(|(_, n)| n.as_str());
        f.push(indexed("contacts:PhoneNumber", "PhoneNumbers", key, n));
    }
    f.extend([
        simple("contacts:AssistantName", "AssistantName", Some(c.assistant.clone())),
        simple("contacts:Birthday", "Birthday", c.birthday.as_deref().map(|b| format!("{b}T00:00:00Z"))),
        simple("contacts:BusinessHomePage", "BusinessHomePage", Some(c.web_page.clone())),
        simple("contacts:Department", "Department", Some(c.department.clone())),
        simple("contacts:Generation", "Generation", Some(c.suffix.clone())),
    ]);
    f.push(indexed("contacts:ImAddress", "ImAddresses", "ImAddress1", Some(c.im_address.as_str())));
    f.extend([
        simple("contacts:JobTitle", "JobTitle", Some(c.job_title.clone())),
        simple("contacts:Manager", "Manager", Some(c.manager.clone())),
        simple("contacts:OfficeLocation", "OfficeLocation", Some(c.office.clone())),
        simple("contacts:Profession", "Profession", Some(c.profession.clone())),
        simple("contacts:SpouseName", "SpouseName", Some(c.spouse.clone())),
        simple("contacts:Surname", "Surname", Some(c.last_name.clone())),
        simple(
            "contacts:WeddingAnniversary",
            "WeddingAnniversary",
            c.anniversary.as_deref().map(|b| format!("{b}T00:00:00Z")),
        ),
    ]);
    f
}

fn categories_xml(c: &[String]) -> Option<String> {
    let list: String = c
        .iter()
        .filter(|x| !x.trim().is_empty())
        .map(|x| format!("<t:String>{}</t:String>", esc(x)))
        .collect();
    (!list.is_empty()).then(|| format!("<t:Categories>{list}</t:Categories>"))
}

/// A date-only value ("YYYY-MM-DD") of an xs:dateTime.
fn date_only(s: Option<&str>) -> Option<String> {
    let s = s?.trim();
    let d = s.get(..10)?;
    (d.len() == 10 && d.as_bytes().get(4) == Some(&b'-')).then(|| d.to_owned())
}

fn entry<'a>(el: &'a Element, container: &str, key: &str) -> Option<&'a Element> {
    el.child(container)?.all("Entry").find(|e| e.attr("Key") == Some(key))
}

fn to_contact(el: &Element) -> Contact {
    let t = |n: &str| el.child_text(n).unwrap_or_default().to_owned();
    let mut c = Contact {
        first_name: t("GivenName"),
        middle_name: t("MiddleName"),
        last_name: t("Surname"),
        suffix: t("Generation"),
        file_as: t("FileAs"),
        company: t("CompanyName"),
        job_title: t("JobTitle"),
        department: t("Department"),
        web_page: t("BusinessHomePage"),
        notes: el.child("Body").map(|b| b.text.trim().to_owned()).unwrap_or_default(),
        categories: strings(el, "Categories"),
        private: matches!(el.child_text("Sensitivity"), Some("Private" | "Confidential")),
        office: t("OfficeLocation"),
        profession: t("Profession"),
        manager: t("Manager"),
        assistant: t("AssistantName"),
        nickname: t("Nickname"),
        spouse: t("SpouseName"),
        birthday: date_only(el.child_text("Birthday")),
        anniversary: date_only(el.child_text("WeddingAnniversary")),
        ..Default::default()
    };
    for key in ["EmailAddress1", "EmailAddress2", "EmailAddress3"] {
        if let Some(e) =
            entry(el, "EmailAddresses", key).map(|e| e.text.trim().to_owned()).filter(|e| !e.is_empty())
        {
            c.emails
                .push(e.strip_prefix("SMTP:").or_else(|| e.strip_prefix("smtp:")).unwrap_or(&e).to_owned());
        }
    }
    for (label, key) in PHONES {
        if let Some(n) =
            entry(el, "PhoneNumbers", key).map(|e| e.text.trim().to_owned()).filter(|n| !n.is_empty())
        {
            c.phones.push((label.to_owned(), n));
        }
    }
    for kind in ADDRESSES {
        if let Some(e) = entry(el, "PhysicalAddresses", kind) {
            let p = |n: &str| e.child_text(n).unwrap_or_default().to_owned();
            let a = PostalAddress {
                kind: kind.to_owned(),
                street: p("Street"),
                city: p("City"),
                region: p("State"),
                postal_code: p("PostalCode"),
                country: p("CountryOrRegion"),
            };
            if !(a.street.is_empty()
                && a.city.is_empty()
                && a.region.is_empty()
                && a.postal_code.is_empty()
                && a.country.is_empty())
            {
                c.addresses.push(a);
            }
        }
    }
    if let Some(im) = entry(el, "ImAddresses", "ImAddress1") {
        c.im_address = im.text.trim().to_owned();
    }
    c
}

fn reminder_fields(minutes: Option<u32>) -> [Field; 2] {
    [
        simple("item:ReminderIsSet", "ReminderIsSet", Some(minutes.is_some().to_string())),
        simple(
            "item:ReminderMinutesBeforeStart",
            "ReminderMinutesBeforeStart",
            minutes.map(|m| m.to_string()),
        ),
    ]
}

fn busy(b: BusyStatus) -> &'static str {
    match b {
        BusyStatus::Free => "Free",
        BusyStatus::WorkingElsewhere => "WorkingElsewhere",
        BusyStatus::Tentative => "Tentative",
        BusyStatus::Busy => "Busy",
        BusyStatus::OutOfOffice => "OOF",
    }
}

fn busy_of(s: Option<&str>) -> BusyStatus {
    match s {
        Some("Free") => BusyStatus::Free,
        Some("Tentative") => BusyStatus::Tentative,
        Some("OOF") => BusyStatus::OutOfOffice,
        Some("WorkingElsewhere") => BusyStatus::WorkingElsewhere,
        _ => BusyStatus::Busy,
    }
}

fn attendees_xml(list: &[&Attendee], el: &str) -> Option<String> {
    let inner: String =
        list.iter().map(|a| format!("<t:Attendee>{}</t:Attendee>", mailbox_xml(&a.mailbox))).collect();
    (!inner.is_empty()).then(|| format!("<t:{el}>{inner}</t:{el}>"))
}

fn event_fields(a: &Appointment, tz_offset_min: i32) -> Result<Vec<Field>, NetError> {
    let pick = |r: AttendeeRole| -> Vec<&Attendee> { a.attendees.iter().filter(|x| x.role == r).collect() };
    let recurrence = match &a.recurrence {
        Some(r) if !r.rrule.trim().is_empty() => Some(
            rrule_to_ews(&r.rrule, a.start, tz_offset_min)
                .ok_or_else(|| http(400, "this repeat pattern cannot be stored on an Exchange server"))?,
        ),
        _ => None,
    };
    let [ri, rm] = reminder_fields(a.reminder_min);
    Ok(vec![
        simple("item:Subject", "Subject", Some(a.subject.clone())),
        simple(
            "item:Sensitivity",
            "Sensitivity",
            Some(if a.private { "Private" } else { "Normal" }.to_owned()),
        ),
        simple("item:Body", "Body", Some(a.body.clone())),
        raw("item:Categories", categories_xml(&a.categories)),
        simple("item:Importance", "Importance", Some(importance(a.importance).to_owned())),
        ri,
        rm,
        simple("calendar:Start", "Start", Some(dt(a.start))),
        simple("calendar:End", "End", Some(dt(a.end))),
        simple("calendar:IsAllDayEvent", "IsAllDayEvent", Some(a.all_day.to_string())),
        simple("calendar:LegacyFreeBusyStatus", "LegacyFreeBusyStatus", Some(busy(a.show_as).to_owned())),
        simple("calendar:Location", "Location", Some(a.location.clone())),
        raw("calendar:RequiredAttendees", attendees_xml(&pick(AttendeeRole::Required), "RequiredAttendees")),
        raw("calendar:OptionalAttendees", attendees_xml(&pick(AttendeeRole::Optional), "OptionalAttendees")),
        raw("calendar:Resources", attendees_xml(&pick(AttendeeRole::Resource), "Resources")),
        raw("calendar:Recurrence", recurrence),
    ])
}

fn response_of(s: Option<&str>) -> ResponseStatus {
    match s {
        Some("Accept") => ResponseStatus::Accepted,
        Some("Tentative") => ResponseStatus::Tentative,
        Some("Decline") => ResponseStatus::Declined,
        _ => ResponseStatus::None,
    }
}

fn to_appointment(el: &Element, uid: &str) -> Appointment {
    let time = |n: &str| el.child_text(n).and_then(parse_datetime).unwrap_or_default();
    let mut attendees = Vec::new();
    for (container, role) in [
        ("RequiredAttendees", AttendeeRole::Required),
        ("OptionalAttendees", AttendeeRole::Optional),
        ("Resources", AttendeeRole::Resource),
    ] {
        for at in el.child(container).map(|c| c.all("Attendee").collect::<Vec<_>>()).unwrap_or_default() {
            if let Some(mailbox) = mailbox_of(at) {
                attendees.push(Attendee {
                    mailbox,
                    role,
                    response: response_of(at.child_text("ResponseType")),
                });
            }
        }
    }
    let recurrence = el.child("Recurrence").and_then(ews_to_rrule).map(|rrule| Recurrence {
        rrule,
        exdates: el
            .child("DeletedOccurrences")
            .map(|d| {
                d.all("DeletedOccurrence")
                    .filter_map(|o| o.child_text("Start").and_then(parse_datetime))
                    .collect()
            })
            .unwrap_or_default(),
    });
    let reminder = el.child_text("ReminderIsSet") == Some("true");
    Appointment {
        uid: uid.to_owned(),
        subject: el.child_text("Subject").unwrap_or_default().to_owned(),
        location: el.child_text("Location").unwrap_or_default().to_owned(),
        start: time("Start"),
        end: time("End"),
        all_day: el.child_text("IsAllDayEvent") == Some("true"),
        show_as: busy_of(el.child_text("LegacyFreeBusyStatus")),
        reminder_min: reminder
            .then(|| el.child_text("ReminderMinutesBeforeStart").and_then(|m| m.parse().ok()))
            .flatten(),
        organizer: el.child("Organizer").and_then(mailbox_of),
        is_meeting: !attendees.is_empty() || el.child_text("IsMeeting") == Some("true"),
        attendees,
        recurrence,
        categories: strings(el, "Categories"),
        private: matches!(el.child_text("Sensitivity"), Some("Private" | "Confidential")),
        body: el.child("Body").map(|b| b.text.trim().to_owned()).unwrap_or_default(),
        importance: importance_of(el.child_text("Importance")),
        ..Default::default()
    }
}

fn status(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::NotStarted => "NotStarted",
        TaskStatus::InProgress => "InProgress",
        TaskStatus::Completed => "Completed",
        TaskStatus::WaitingOnSomeoneElse => "WaitingOnOthers",
        TaskStatus::Deferred => "Deferred",
    }
}

fn status_of(s: Option<&str>) -> TaskStatus {
    match s {
        Some("InProgress") => TaskStatus::InProgress,
        Some("Completed") => TaskStatus::Completed,
        Some("WaitingOnOthers") => TaskStatus::WaitingOnSomeoneElse,
        Some("Deferred") => TaskStatus::Deferred,
        _ => TaskStatus::NotStarted,
    }
}

fn task_fields(t: &Task) -> Vec<Field> {
    let prio = match t.priority {
        TaskPriority::Low => Importance::Low,
        TaskPriority::Normal => Importance::Normal,
        TaskPriority::High => Importance::High,
    };
    vec![
        simple("item:Subject", "Subject", Some(t.subject.clone())),
        simple(
            "item:Sensitivity",
            "Sensitivity",
            Some(if t.private { "Private" } else { "Normal" }.to_owned()),
        ),
        simple("item:Body", "Body", Some(t.body.clone())),
        raw("item:Categories", categories_xml(&t.categories)),
        simple("item:Importance", "Importance", Some(importance(prio).to_owned())),
        simple("item:ReminderDueBy", "ReminderDueBy", t.reminder.map(dt)),
        simple("item:ReminderIsSet", "ReminderIsSet", Some(t.reminder.is_some().to_string())),
        simple("task:CompleteDate", "CompleteDate", t.completed.map(dt)),
        simple("task:DueDate", "DueDate", t.due.map(dt)),
        simple("task:PercentComplete", "PercentComplete", Some(t.percent_complete.min(100).to_string())),
        simple("task:StartDate", "StartDate", t.start.map(dt)),
        simple("task:Status", "Status", Some(status(t.status).to_owned())),
    ]
}

fn to_task(el: &Element, uid: &str) -> Task {
    let time = |n: &str| el.child_text(n).and_then(parse_datetime);
    Task {
        uid: uid.to_owned(),
        subject: el.child_text("Subject").unwrap_or_default().to_owned(),
        body: el.child("Body").map(|b| b.text.trim().to_owned()).unwrap_or_default(),
        categories: strings(el, "Categories"),
        private: matches!(el.child_text("Sensitivity"), Some("Private" | "Confidential")),
        priority: match importance_of(el.child_text("Importance")) {
            Importance::Low => TaskPriority::Low,
            Importance::Normal => TaskPriority::Normal,
            Importance::High => TaskPriority::High,
        },
        reminder: if el.child_text("ReminderIsSet") == Some("true") { time("ReminderDueBy") } else { None },
        completed: time("CompleteDate"),
        due: time("DueDate"),
        start: time("StartDate"),
        percent_complete: el
            .child_text("PercentComplete")
            .and_then(|p| p.split('.').next()?.parse::<u8>().ok())
            .unwrap_or(0)
            .min(100),
        status: status_of(el.child_text("Status")),
        ..Default::default()
    }
}

/// The item element for creating an item of `kind` (`el`) from its fields, with the UID property.
fn create_xml(el: &str, fields: &[Field], uid: &str) -> String {
    let mut out = format!("<t:{el}>");
    let uid_xml =
        format!("<t:ExtendedProperty>{ITEM_UID}<t:Value>{}</t:Value></t:ExtendedProperty>", esc(uid));
    let mut uid_done = false;
    let mut i = 0usize;
    while let Some(f) = fields.get(i) {
        // The extended property goes after the ItemType properties (before the first of the item's own).
        if !uid_done && !f.uri.contains("\"item:") {
            out.push_str(&uid_xml);
            uid_done = true;
        }
        match &f.indexed {
            None => {
                if let Some(v) = &f.value {
                    out.push_str(v);
                }
                i += 1;
            }
            Some((container, _, _)) => {
                // Every entry of this container together.
                let mut entries = String::new();
                let mut by_key: Vec<(String, String)> = Vec::new();
                while let Some(g) =
                    fields.get(i).filter(|g| g.indexed.as_ref().is_some_and(|(c, _, _)| c == container))
                {
                    if let (Some((_, key, part)), Some(v)) = (&g.indexed, &g.value) {
                        match part {
                            None => entries.push_str(&format!(r#"<t:Entry Key="{key}">{v}</t:Entry>"#)),
                            Some(_) => match by_key.iter_mut().find(|(k, _)| k == key) {
                                Some((_, acc)) => acc.push_str(v),
                                None => by_key.push((key.clone(), v.clone())),
                            },
                        }
                    }
                    i += 1;
                }
                for (key, parts) in by_key {
                    entries.push_str(&format!(r#"<t:Entry Key="{key}">{parts}</t:Entry>"#));
                }
                if !entries.is_empty() {
                    out.push_str(&format!("<t:{container}>{entries}</t:{container}>"));
                }
            }
        }
    }
    if !uid_done {
        out.push_str(&uid_xml);
    }
    out.push_str(&format!("</t:{el}>"));
    out
}

/// SetItemField / DeleteItemField elements for the fields of `now` that differ from `was`.
fn update_xml(el: &str, now: &[Field], was: &[Field]) -> String {
    let mut out = String::new();
    for f in now {
        let before = was.iter().find(|w| w.uri == f.uri).and_then(|w| w.value.clone());
        if before == f.value {
            continue;
        }
        match (&f.value, &f.indexed) {
            (None, _) => {
                if before.is_some() {
                    out.push_str(&format!("<t:DeleteItemField>{}</t:DeleteItemField>", f.uri));
                }
            }
            (Some(v), None) => out.push_str(&format!("<t:SetItemField>{}<t:{el}>{v}</t:{el}></t:SetItemField>", f.uri)),
            (Some(v), Some((container, key, _))) => out.push_str(&format!(
                r#"<t:SetItemField>{}<t:{el}><t:{container}><t:Entry Key="{key}">{v}</t:Entry></t:{container}></t:{el}></t:SetItemField>"#,
                f.uri
            )),
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------
// Recurrence: EWS patterns ↔ RFC 5545 RRULE
// ---------------------------------------------------------------------------------------------------

const DAYS: [(&str, &str); 7] = [
    ("Sunday", "SU"),
    ("Monday", "MO"),
    ("Tuesday", "TU"),
    ("Wednesday", "WE"),
    ("Thursday", "TH"),
    ("Friday", "FR"),
    ("Saturday", "SA"),
];
const MONTHS: [&str; 12] = [
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
];
const INDEX: [(&str, i32); 5] = [("First", 1), ("Second", 2), ("Third", 3), ("Fourth", 4), ("Last", -1)];

fn days_rule(names: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for n in names.split_whitespace() {
        match n {
            "Day" => out.extend(DAYS.iter().map(|(_, c)| *c)),
            "Weekday" => out.extend(["MO", "TU", "WE", "TH", "FR"]),
            "WeekendDay" => out.extend(["SA", "SU"]),
            n => out.extend(DAYS.iter().filter(|(d, _)| *d == n).map(|(_, c)| *c)),
        }
    }
    out.join(",")
}

/// The RRULE of an EWS `Recurrence` element.
pub(super) fn ews_to_rrule(r: &Element) -> Option<String> {
    let interval =
        |p: &Element| p.child_text("Interval").and_then(|i| i.parse::<u32>().ok()).filter(|i| *i > 1);
    let month =
        |p: &Element| p.child_text("Month").and_then(|m| MONTHS.iter().position(|x| *x == m)).map(|i| i + 1);
    let index = |p: &Element| {
        p.child_text("DayOfWeekIndex").and_then(|x| INDEX.iter().find(|(n, _)| *n == x)).map(|(_, v)| *v)
    };
    let mut parts: Vec<String> = Vec::new();
    let mut found = false;
    for p in &r.children {
        match p.name.as_str() {
            "DailyRecurrence" => parts.push("FREQ=DAILY".into()),
            "WeeklyRecurrence" => {
                parts.push("FREQ=WEEKLY".into());
                let d = days_rule(p.child_text("DaysOfWeek").unwrap_or_default());
                if !d.is_empty() {
                    parts.push(format!("BYDAY={d}"));
                }
            }
            "AbsoluteMonthlyRecurrence" => {
                parts.push("FREQ=MONTHLY".into());
                parts.push(format!("BYMONTHDAY={}", p.child_text("DayOfMonth")?.parse::<u32>().ok()?));
            }
            "RelativeMonthlyRecurrence" => {
                parts.push("FREQ=MONTHLY".into());
                parts.push(format!("BYDAY={}", days_rule(p.child_text("DaysOfWeek")?)));
                parts.push(format!("BYSETPOS={}", index(p)?));
            }
            "AbsoluteYearlyRecurrence" => {
                parts.push("FREQ=YEARLY".into());
                parts.push(format!("BYMONTH={}", month(p)?));
                parts.push(format!("BYMONTHDAY={}", p.child_text("DayOfMonth")?.parse::<u32>().ok()?));
            }
            "RelativeYearlyRecurrence" => {
                parts.push("FREQ=YEARLY".into());
                parts.push(format!("BYMONTH={}", month(p)?));
                parts.push(format!("BYDAY={}", days_rule(p.child_text("DaysOfWeek")?)));
                parts.push(format!("BYSETPOS={}", index(p)?));
            }
            "NumberedRecurrence" => {
                parts.push(format!("COUNT={}", p.child_text("NumberOfOccurrences")?.parse::<u32>().ok()?));
                continue;
            }
            "EndDateRecurrence" => {
                let d = p.child_text("EndDate")?.get(..10)?.replace('-', "");
                parts.push(format!("UNTIL={d}T235959Z"));
                continue;
            }
            _ => continue,
        }
        found = true;
        if let Some(i) = interval(p) {
            parts.push(format!("INTERVAL={i}"));
        }
    }
    found.then(|| parts.join(";"))
}

/// The EWS `Recurrence` element of an RRULE for a series starting at `start` (`None` when the rule has no
/// EWS form: hourly rules, several month days, …).
pub(super) fn rrule_to_ews(rrule: &str, start: DateTime, tz_offset_min: i32) -> Option<String> {
    let mut kv: BTreeMap<String, String> = BTreeMap::new();
    for part in rrule.trim().trim_start_matches("RRULE:").split(';') {
        if let Some((k, v)) = part.split_once('=') {
            kv.insert(k.trim().to_ascii_uppercase(), v.trim().to_ascii_uppercase());
        }
    }
    let interval: u32 = kv.get("INTERVAL").and_then(|i| i.parse().ok()).unwrap_or(1).max(1);
    let civil = start.civil(tz_offset_min);
    let day_name = |code: &str| DAYS.iter().find(|(_, c)| *c == code).map(|(n, _)| *n);
    let weekday =
        DAYS.get(usize::from(civil.weekday.index_from_sunday())).map(|(n, _)| *n).unwrap_or("Monday");
    // BYDAY entries with their position ("1MO", "-1FR", "MO").
    let byday: Vec<(Option<i32>, &str)> = kv
        .get("BYDAY")
        .map(|v| {
            v.split(',')
                .filter_map(|d| {
                    let d = d.trim();
                    let code = d.get(d.len().saturating_sub(2)..)?;
                    let pos = d
                        .get(..d.len().saturating_sub(2))
                        .filter(|p| !p.is_empty())
                        .and_then(|p| p.parse().ok());
                    Some((pos, day_name(code)?))
                })
                .collect()
        })
        .unwrap_or_default();
    let setpos: Option<i32> =
        kv.get("BYSETPOS").and_then(|p| p.parse().ok()).or_else(|| byday.first().and_then(|(p, _)| *p));
    let index_name = |p: i32| INDEX.iter().find(|(_, v)| *v == p).map(|(n, _)| *n);
    let names = |list: &[(Option<i32>, &str)]| -> String {
        let all: Vec<&str> = list.iter().map(|(_, n)| *n).collect();
        let set: std::collections::BTreeSet<&str> = all.iter().copied().collect();
        let weekdays: std::collections::BTreeSet<&str> =
            ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"].into_iter().collect();
        if set.len() == 7 {
            "Day".into()
        } else if set == weekdays {
            "Weekday".into()
        } else if set.len() == 2 && set.contains("Saturday") && set.contains("Sunday") {
            "WeekendDay".into()
        } else {
            all.join(" ")
        }
    };
    let month_name =
        |m: Option<u8>| MONTHS.get(usize::from(m.unwrap_or(civil.month)).saturating_sub(1)).copied();
    let bymonth: Option<u8> = kv.get("BYMONTH").and_then(|m| m.parse().ok());
    let bymonthday: Option<u32> = match kv.get("BYMONTHDAY") {
        Some(v) if v.contains(',') => return None,
        Some(v) => Some(v.parse().ok()?),
        None => None,
    };
    let pattern = match kv.get("FREQ").map(String::as_str)? {
        "DAILY" if byday.is_empty() => {
            format!("<t:DailyRecurrence><t:Interval>{interval}</t:Interval></t:DailyRecurrence>")
        }
        "DAILY" | "WEEKLY" => {
            let days = if byday.is_empty() {
                weekday.to_owned()
            } else {
                byday.iter().map(|(_, n)| *n).collect::<Vec<_>>().join(" ")
            };
            format!(
                "<t:WeeklyRecurrence><t:Interval>{interval}</t:Interval><t:DaysOfWeek>{days}</t:DaysOfWeek></t:WeeklyRecurrence>"
            )
        }
        "MONTHLY" => match (setpos, byday.is_empty()) {
            (Some(p), false) => format!(
                "<t:RelativeMonthlyRecurrence><t:Interval>{interval}</t:Interval><t:DaysOfWeek>{}</t:DaysOfWeek><t:DayOfWeekIndex>{}</t:DayOfWeekIndex></t:RelativeMonthlyRecurrence>",
                names(&byday),
                index_name(p)?
            ),
            (None, false) => return None,
            _ => format!(
                "<t:AbsoluteMonthlyRecurrence><t:Interval>{interval}</t:Interval><t:DayOfMonth>{}</t:DayOfMonth></t:AbsoluteMonthlyRecurrence>",
                bymonthday.unwrap_or(u32::from(civil.day))
            ),
        },
        "YEARLY" if interval == 1 => match (setpos, byday.is_empty()) {
            (Some(p), false) => format!(
                "<t:RelativeYearlyRecurrence><t:DaysOfWeek>{}</t:DaysOfWeek><t:DayOfWeekIndex>{}</t:DayOfWeekIndex><t:Month>{}</t:Month></t:RelativeYearlyRecurrence>",
                names(&byday),
                index_name(p)?,
                month_name(bymonth)?
            ),
            (None, false) => return None,
            _ => format!(
                "<t:AbsoluteYearlyRecurrence><t:DayOfMonth>{}</t:DayOfMonth><t:Month>{}</t:Month></t:AbsoluteYearlyRecurrence>",
                bymonthday.unwrap_or(u32::from(civil.day)),
                month_name(bymonth)?
            ),
        },
        _ => return None,
    };
    let start_date = format!("{:04}-{:02}-{:02}", civil.year, civil.month, civil.day);
    let range = if let Some(n) = kv.get("COUNT").and_then(|c| c.parse::<u32>().ok()) {
        format!(
            "<t:NumberedRecurrence><t:StartDate>{start_date}</t:StartDate><t:NumberOfOccurrences>{n}</t:NumberOfOccurrences></t:NumberedRecurrence>"
        )
    } else if let Some(u) = kv.get("UNTIL") {
        let d = u.get(..8)?;
        let end = format!("{}-{}-{}", d.get(..4)?, d.get(4..6)?, d.get(6..8)?);
        format!(
            "<t:EndDateRecurrence><t:StartDate>{start_date}</t:StartDate><t:EndDate>{end}</t:EndDate></t:EndDateRecurrence>"
        )
    } else {
        format!("<t:NoEndRecurrence><t:StartDate>{start_date}</t:StartDate></t:NoEndRecurrence>")
    };
    Some(format!("<t:Recurrence>{pattern}{range}</t:Recurrence>"))
}

// ---------------------------------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------------------------------

/// One EWS folder's items as DAV resources (see the module notes).
pub(super) struct Store<'a> {
    client: &'a EwsClient,
    kind: PimKind,
    tz_offset_min: i32,
    now: DateTime,
    /// Resource address → (item id, change key) (from listings and creations).
    ids: RefCell<BTreeMap<String, (String, String)>>,
}

impl<'a> Store<'a> {
    pub(super) fn new(client: &'a EwsClient, kind: PimKind, tz_offset_min: i32, now: DateTime) -> Store<'a> {
        Store { client, kind, tz_offset_min, now, ids: RefCell::new(BTreeMap::new()) }
    }

    fn element(&self) -> &'static str {
        match self.kind {
            PimKind::Contacts => "Contact",
            PimKind::Tasks => "Task",
            PimKind::Calendar => "CalendarItem",
        }
    }

    fn full_shape(&self) -> &'static str {
        if self.kind == PimKind::Calendar { FULL_SHAPE_CAL } else { FULL_SHAPE }
    }

    pub(super) fn listing(&self, collection: &str) -> Result<Listing, NetError> {
        let Some(folder) = collection_id(collection, self.kind) else {
            return Err(NetError::Protocol(format!("{collection} is not an Exchange folder")));
        };
        let shape = if self.kind == PimKind::Calendar { LIST_SHAPE_CAL } else { LIST_SHAPE };
        let items = match self.client.find_items(&folder, shape, pim::MAX_ITEMS) {
            Ok(i) => i,
            Err(NetError::TooLarge(_)) => return Err(NetError::Protocol(pim::too_many_error())),
            Err(e) => return Err(e),
        };
        let mut resources = Vec::new();
        let mut taken: BTreeMap<String, String> = BTreeMap::new();
        let mut dropped = 0usize;
        for el in &items {
            if matches!(el.child_text("CalendarItemType"), Some("Occurrence" | "Exception")) {
                continue;
            }
            let Some((id, ck)) = item_id(el) else {
                dropped += 1;
                continue;
            };
            let uid = uid_of(el, &id);
            let mut url = pim::resource_url(collection, &uid, self.kind);
            if taken.get(&url).is_some_and(|o| *o != id) {
                url = pim::resource_url(
                    collection,
                    &format!("ews-{:016x}", pim::fnv(id.as_bytes())),
                    self.kind,
                );
            }
            let url = super::dav::canon(&url);
            taken.insert(url.clone(), id.clone());
            self.ids.borrow_mut().insert(url.clone(), (id, ck.clone()));
            resources.push(Resource {
                url,
                etag: tag(&ck),
                content_type: self.kind.content_type().to_owned(),
            });
        }
        Ok(Listing { collection: collection.to_owned(), resources, dropped })
    }

    /// The items at `urls` as MailCraft's vCard / iCalendar text.
    pub(super) fn multiget(&self, urls: &[String]) -> Result<Vec<Fetched>, NetError> {
        let wanted: Vec<(String, String)> = urls
            .iter()
            .filter_map(|u| self.ids.borrow().get(u).map(|(id, _)| (u.clone(), id.clone())))
            .collect();
        let ids: Vec<String> = wanted.iter().map(|(_, id)| id.clone()).collect();
        let got = self.client.get_items(&ids, self.full_shape())?;
        let mut out = Vec::new();
        for ((url, _), el) in wanted.into_iter().zip(got) {
            let Some(el) = el else { continue };
            let Some((id, ck)) = item_id(&el) else { continue };
            let Some(data) = self.text_of(&el, &id)? else { continue };
            self.ids.borrow_mut().insert(url.clone(), (id, ck.clone()));
            out.push(Fetched { url, etag: tag(&ck), data });
        }
        Ok(out)
    }

    /// The item read into the model, as text; `None` when it holds nothing of this kind.
    fn text_of(&self, el: &Element, id: &str) -> Result<Option<String>, NetError> {
        let uid = uid_of(el, id);
        Ok(match self.kind {
            PimKind::Contacts => Some(pim::contact_card(&to_contact(el), &uid)),
            PimKind::Tasks => Some(pim::task_calendar(&to_task(el, &uid), &uid, self.now)),
            PimKind::Calendar => {
                let items = self.appointments(el, &uid)?;
                let refs: Vec<&Appointment> = items.iter().collect();
                Some(pim::event_calendar(&refs, &uid, self.now))
            }
        })
    }

    /// A calendar item and, for a series, its modified occurrences (as exceptions).
    fn appointments(&self, el: &Element, uid: &str) -> Result<Vec<Appointment>, NetError> {
        let mut items = vec![to_appointment(el, uid)];
        let occurrences: Vec<(String, Option<DateTime>)> = el
            .child("ModifiedOccurrences")
            .map(|m| {
                m.all("Occurrence")
                    .take(pim::MAX_GROUP)
                    .filter_map(|o| {
                        Some((
                            o.child("ItemId")?.attr("Id")?.to_owned(),
                            o.child_text("OriginalStart").and_then(parse_datetime),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if occurrences.is_empty() {
            return Ok(items);
        }
        let ids: Vec<String> = occurrences.iter().map(|(i, _)| i.clone()).collect();
        let got = self.client.get_items(&ids, FULL_SHAPE_CAL)?;
        for ((_, original), occ) in occurrences.into_iter().zip(got) {
            let Some(occ) = occ else { continue };
            let mut a = to_appointment(&occ, uid);
            a.recurrence = None;
            a.recurrence_id = original.or_else(|| occ.child_text("OriginalStart").and_then(parse_datetime));
            if a.recurrence_id.is_some() {
                items.push(a);
            }
        }
        Ok(items)
    }

    /// The fields of `data` (vCard / iCalendar text) and its UID.
    /// (Single occurrences are compared separately: see [`occurrence_signature`].)
    fn fields_of(&self, data: &str) -> Result<(Vec<Field>, String), NetError> {
        let unreadable = || http(400, "the item could not be read");
        match self.kind {
            PimKind::Contacts => {
                let (c, uid) = pim::read_contact(data).ok_or_else(unreadable)?;
                Ok((contact_fields(&c), uid))
            }
            PimKind::Tasks => {
                let (t, uid) = pim::read_task(data, self.tz_offset_min).ok_or_else(unreadable)?;
                Ok((task_fields(&t), uid))
            }
            PimKind::Calendar => {
                let (items, uid) = pim::read_events(data, self.tz_offset_min).ok_or_else(unreadable)?;
                // The series (or the single event); exceptions are not written (see the module notes).
                let lead = items
                    .iter()
                    .find(|a| a.recurrence_id.is_none())
                    .or_else(|| items.first())
                    .ok_or_else(unreadable)?;
                Ok((event_fields(lead, self.tz_offset_min)?, uid))
            }
        }
    }

    fn attrs(&self) -> &'static str {
        match self.kind {
            PimKind::Calendar => r#"SendMeetingInvitations="SendToNone""#,
            _ => "",
        }
    }

    fn update_attrs(&self) -> &'static str {
        match self.kind {
            PimKind::Calendar => r#"SendMeetingInvitationsOrCancellations="SendToNone""#,
            _ => "",
        }
    }

    pub(super) fn put(
        &self,
        collection: &str,
        url: &str,
        data: &[u8],
        pre: &Precondition,
    ) -> Result<Option<String>, NetError> {
        let text = String::from_utf8_lossy(data);
        let (fields, uid) = self.fields_of(&text)?;
        let existing = self.ids.borrow().get(url).cloned();
        match (pre, existing) {
            (Precondition::Create, Some(_)) => Err(http(412, "exists")),
            (Precondition::Match(_), None) => Err(http(412, "gone")),
            (Precondition::Create | Precondition::Any, None) => {
                let Some(folder) = collection_id(collection, self.kind) else {
                    return Err(NetError::Protocol(format!("{collection} is not an Exchange folder")));
                };
                let uid = if uid.trim().is_empty() {
                    format!("mailcraft-{:016x}", pim::fnv(url.as_bytes()))
                } else {
                    uid
                };
                let xml = create_xml(self.element(), &fields, &uid);
                let (id, ck) = self.client.create_item(&folder, &xml, self.attrs()).map_err(|e| match e {
                    NetError::Ews { code, message } => http(400, &format!("{code} {message}")),
                    other => other,
                })?;
                let ck = if ck.is_empty() { self.current_key(&id)?.unwrap_or_default() } else { ck };
                self.ids.borrow_mut().insert(url.to_owned(), (id, ck.clone()));
                Ok(Some(tag(&ck)))
            }
            (Precondition::Match(_) | Precondition::Any, Some((id, _))) => {
                let current = self
                    .client
                    .get_items(std::slice::from_ref(&id), self.full_shape())?
                    .into_iter()
                    .next()
                    .flatten();
                let Some(current) = current else { return Err(http(412, "gone")) };
                let (_, ck_now) = item_id(&current).unwrap_or_default();
                if let Precondition::Match(expected) = pre
                    && tag(&ck_now) != *expected
                {
                    return Err(http(412, "changed"));
                }
                let was_text = self.text_of(&current, &id)?;
                // A removed or changed single occurrence is not written to the server yet: refused (this item
                // only, reported) rather than reported as saved while the server keeps the old occurrence.
                if self.kind == PimKind::Calendar
                    && let (Some(here), Some(there)) = (
                        occurrence_signature(&text, self.tz_offset_min),
                        was_text.as_deref().and_then(|t| occurrence_signature(t, self.tz_offset_min)),
                    )
                    && here != there
                {
                    return Err(http(
                        400,
                        "a change to single occurrences of a series is not saved on the Exchange server yet \
                         (change the whole series instead)",
                    ));
                }
                // Only what this edit changed: the server's item read the way the edited copy was.
                let was = match was_text {
                    Some(t) => self.fields_of(&t).map(|(f, _)| f).unwrap_or_default(),
                    None => Vec::new(),
                };
                let updates = update_xml(self.element(), &fields, &was);
                if updates.is_empty() {
                    return Ok(Some(tag(&ck_now)));
                }
                match self.client.update_item(&id, Some(&ck_now), &updates, self.update_attrs()) {
                    Ok((new_id, ck)) => {
                        let ck =
                            if ck.is_empty() { self.current_key(&new_id)?.unwrap_or_default() } else { ck };
                        self.ids.borrow_mut().insert(url.to_owned(), (new_id, ck.clone()));
                        Ok(Some(tag(&ck)))
                    }
                    Err(e)
                        if is_code(&e, "ErrorIrresolvableConflict")
                            || is_code(&e, "ErrorChangeKeyRequiredForWriteOperations") =>
                    {
                        Err(http(412, "changed"))
                    }
                    Err(e) if is_code(&e, "ErrorItemNotFound") => Err(http(412, "gone")),
                    Err(NetError::Ews { code, message }) => Err(http(400, &format!("{code} {message}"))),
                    Err(e) => Err(e),
                }
            }
        }
    }

    fn current_key(&self, id: &str) -> Result<Option<String>, NetError> {
        let got = self.client.get_items(&[id.to_owned()], "<t:BaseShape>IdOnly</t:BaseShape>")?;
        Ok(got.into_iter().next().flatten().and_then(|e| item_id(&e)).map(|(_, ck)| ck))
    }

    pub(super) fn delete(&self, url: &str, etag: Option<&str>) -> Result<(), NetError> {
        let Some((id, _)) = self.ids.borrow().get(url).cloned() else { return Ok(()) };
        if let Some(expected) = etag {
            match self.current_key(&id)? {
                None => return Ok(()),
                Some(now) if tag(&now) != expected => return Err(http(412, "changed")),
                Some(_) => {}
            }
        }
        self.client.delete_item(&id)?;
        self.ids.borrow_mut().remove(url);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_net::autoconfig::parse_xml_with;

    fn el(xml: &str) -> Element {
        parse_xml_with(xml, 1 << 20, 10_000).unwrap()
    }

    #[test]
    fn collection_addresses() {
        let u = collection_url(PimKind::Contacts, "AAMk/x=");
        assert!(pim::is_ews_url(&u));
        assert_eq!(collection_id(&u, PimKind::Contacts).as_deref(), Some("AAMk/x="));
        assert_eq!(collection_id(&u, PimKind::Calendar), None);
        assert_eq!(collection_id("https://dav.example.com/contacts/x/", PimKind::Contacts), None);
    }

    #[test]
    fn recurrences_both_ways() {
        let start = DateTime::parse_rfc5322("2026-11-03T15:00:00Z").unwrap();
        for (rule, back) in [
            ("FREQ=WEEKLY;BYDAY=TU,TH", "FREQ=WEEKLY;BYDAY=TU,TH"),
            ("FREQ=DAILY;INTERVAL=2;COUNT=5", "FREQ=DAILY;INTERVAL=2;COUNT=5"),
            (
                "FREQ=MONTHLY;BYMONTHDAY=3;UNTIL=20270101T000000Z",
                "FREQ=MONTHLY;BYMONTHDAY=3;UNTIL=20270101T235959Z",
            ),
            ("FREQ=MONTHLY;BYDAY=1TU", "FREQ=MONTHLY;BYDAY=TU;BYSETPOS=1"),
            (
                "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
                "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
            ),
            ("FREQ=YEARLY", "FREQ=YEARLY;BYMONTH=11;BYMONTHDAY=3"),
            ("FREQ=YEARLY;BYMONTH=5;BYDAY=2SU", "FREQ=YEARLY;BYMONTH=5;BYDAY=SU;BYSETPOS=2"),
            ("FREQ=WEEKLY", "FREQ=WEEKLY;BYDAY=TU"),
        ] {
            let xml = rrule_to_ews(rule, start, 0).unwrap_or_else(|| panic!("{rule}"));
            let parsed = el(&xml);
            assert_eq!(ews_to_rrule(&parsed).as_deref(), Some(back), "{rule} → {xml}");
        }
        assert!(rrule_to_ews("FREQ=HOURLY", start, 0).is_none());
        assert!(rrule_to_ews("FREQ=MONTHLY;BYMONTHDAY=1,15", start, 0).is_none());
        assert!(rrule_to_ews("junk", start, 0).is_none());
        // The server's form (a date with a zone letter).
        let live = el(
            "<Recurrence><WeeklyRecurrence><Interval>1</Interval><DaysOfWeek>Tuesday</DaysOfWeek><FirstDayOfWeek>Monday</FirstDayOfWeek></WeeklyRecurrence><NumberedRecurrence><StartDate>2026-11-03Z</StartDate><NumberOfOccurrences>3</NumberOfOccurrences></NumberedRecurrence></Recurrence>",
        );
        assert_eq!(ews_to_rrule(&live).as_deref(), Some("FREQ=WEEKLY;BYDAY=TU;COUNT=3"));
        assert!(ews_to_rrule(&el("<Recurrence><Junk/></Recurrence>")).is_none());
    }

    #[test]
    fn contacts_round_trip_and_updates_name_only_changes() {
        let c = Contact {
            first_name: "Robin".into(),
            last_name: "Example".into(),
            emails: vec!["robin@example.net".into()],
            phones: vec![("Mobile".into(), "+1 555 0100".into())],
            addresses: vec![PostalAddress {
                kind: "Home".into(),
                city: "Springfield".into(),
                ..Default::default()
            }],
            birthday: Some("1990-05-01".into()),
            ..Default::default()
        };
        let xml = create_xml("Contact", &contact_fields(&c), "uid-1");
        assert!(xml.contains(r#"<t:Entry Key="EmailAddress1">robin@example.net</t:Entry>"#), "{xml}");
        assert!(xml.contains(r#"<t:PhysicalAddresses><t:Entry Key="Home"><t:City>Springfield</t:City></t:Entry></t:PhysicalAddresses>"#), "{xml}");
        assert!(xml.find("ExtendedProperty").unwrap() < xml.find("GivenName").unwrap());
        let back = to_contact(&el(&xml.replace("t:", "")));
        assert_eq!(
            (back.first_name.as_str(), back.emails.clone(), back.birthday.clone()),
            ("Robin", c.emails.clone(), c.birthday.clone())
        );
        assert_eq!(back.phones, c.phones);
        assert_eq!(back.addresses[0].city, "Springfield");
        let mut d = c.clone();
        d.job_title = "Tester".into();
        d.emails.clear();
        let up = update_xml("Contact", &contact_fields(&d), &contact_fields(&c));
        assert!(up.contains(r#"<t:FieldURI FieldURI="contacts:JobTitle"/><t:Contact><t:JobTitle>Tester</t:JobTitle></t:Contact>"#), "{up}");
        assert!(up.contains(r#"<t:DeleteItemField><t:IndexedFieldURI FieldURI="contacts:EmailAddress" FieldIndex="EmailAddress1"/></t:DeleteItemField>"#), "{up}");
        assert!(!up.contains("GivenName"), "unchanged fields are left alone: {up}");
    }

    #[test]
    fn events_and_tasks_read_back() {
        let x = el(
            r#"<CalendarItem><ItemId Id="I1" ChangeKey="C"/><Subject>Review</Subject><Start>2026-11-03T15:00:00Z</Start><End>2026-11-03T16:00:00Z</End><LegacyFreeBusyStatus>Tentative</LegacyFreeBusyStatus><ReminderIsSet>true</ReminderIsSet><ReminderMinutesBeforeStart>10</ReminderMinutesBeforeStart><UID>abc</UID><Organizer><Mailbox><Name>Pat</Name><EmailAddress>pat@example.com</EmailAddress></Mailbox></Organizer><RequiredAttendees><Attendee><Mailbox><EmailAddress>bob@example.org</EmailAddress></Mailbox><ResponseType>Accept</ResponseType></Attendee></RequiredAttendees><Recurrence><DailyRecurrence><Interval>1</Interval></DailyRecurrence><NoEndRecurrence><StartDate>2026-11-03</StartDate></NoEndRecurrence></Recurrence><DeletedOccurrences><DeletedOccurrence><Start>2026-11-04T15:00:00Z</Start></DeletedOccurrence></DeletedOccurrences></CalendarItem>"#,
        );
        assert_eq!(uid_of(&x, "I1"), "abc");
        let a = to_appointment(&x, "abc");
        assert_eq!(
            (a.subject.as_str(), a.show_as, a.reminder_min),
            ("Review", BusyStatus::Tentative, Some(10))
        );
        assert!(a.is_meeting && a.attendees[0].response == ResponseStatus::Accepted);
        let r = a.recurrence.clone().unwrap();
        assert_eq!(r.rrule, "FREQ=DAILY");
        assert_eq!(r.exdates.len(), 1);
        let f = event_fields(&a, 0).unwrap();
        let xml = create_xml("CalendarItem", &f, "abc");
        assert!(
            xml.contains("<t:DailyRecurrence>")
                && xml.contains("<t:LegacyFreeBusyStatus>Tentative</t:LegacyFreeBusyStatus>"),
            "{xml}"
        );
        let t = Task {
            subject: "Call".into(),
            due: Some(a.start),
            status: TaskStatus::InProgress,
            percent_complete: 50,
            ..Default::default()
        };
        let tx = create_xml("Task", &task_fields(&t), "t1");
        let back = to_task(&el(&tx.replace("t:", "")), "t1");
        assert_eq!(
            (back.subject.as_str(), back.status, back.percent_complete, back.due),
            ("Call", TaskStatus::InProgress, 50, t.due)
        );
        assert_eq!(uid_of(&el("<Task><ItemId Id=\"X\"/></Task>"), "X").get(..4), Some("ews-"));
    }
}
