//! Contacts, appointments, tasks and notes from their MAPI properties: [MS-OXOCNTC], [MS-OXOCAL] (with the
//! recurrence pattern of §2.2.1.44), [MS-OXOTASK] and [MS-OXONOTE]. Named properties are found through the
//! file's name-to-id map ([`Names`]).

use std::collections::HashMap;

use mailcraft_mime::Mailbox;
use mailcraft_model::calendar::{AttendeeRole, ResponseStatus};
use mailcraft_model::contact::PostalAddress;
use mailcraft_model::{
    Appointment, Attendee, BusyStatus, Contact, DateTime, Importance, Note, NoteColor, Recurrence, Task,
    TaskPriority, TaskStatus,
};

use crate::msg::props::{PT_BINARY, PT_BOOLEAN, PT_DOUBLE, PT_SYSTIME, Props, Value, filetime_to_unix};

/// PSETID_Appointment.
pub(crate) const PSETID_APPOINTMENT: [u8; 16] =
    [0x02, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];
/// PSETID_Task.
pub(crate) const PSETID_TASK: [u8; 16] =
    [0x03, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];
/// PSETID_Address.
pub(crate) const PSETID_ADDRESS: [u8; 16] =
    [0x04, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];
/// PSETID_Common.
pub(crate) const PSETID_COMMON: [u8; 16] =
    [0x08, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];
/// PSETID_Note.
pub(crate) const PSETID_NOTE: [u8; 16] =
    [0x0E, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];
/// PSETID_Meeting.
pub(crate) const PSETID_MEETING: [u8; 16] =
    [0x90, 0xDA, 0xD8, 0x6E, 0x0B, 0x45, 0x1B, 0x10, 0x98, 0xDA, 0x00, 0xAA, 0x00, 0x3F, 0x13, 0x05];
/// PS_PUBLIC_STRINGS.
pub(crate) const PS_PUBLIC_STRINGS: [u8; 16] =
    [0x29, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

// Appointment (PSETID_Appointment).
pub(crate) const LID_BUSY_STATUS: u32 = 0x8205;
pub(crate) const LID_LOCATION: u32 = 0x8208;
pub(crate) const LID_START_WHOLE: u32 = 0x820D;
pub(crate) const LID_END_WHOLE: u32 = 0x820E;
pub(crate) const LID_APPOINTMENT_RECUR: u32 = 0x8216;
pub(crate) const LID_STATE_FLAGS: u32 = 0x8217;
pub(crate) const LID_SUB_TYPE: u32 = 0x8215;
pub(crate) const LID_RECURRING: u32 = 0x8223;
// Meeting (PSETID_Meeting): PidLidGlobalObjectId and PidLidCleanGlobalObjectId.
pub(crate) const LID_GLOBAL_OBJECT_ID: u32 = 0x0003;
pub(crate) const LID_CLEAN_GLOBAL_OBJECT_ID: u32 = 0x0023;
// Common.
pub(crate) const LID_REMINDER_DELTA: u32 = 0x8501;
pub(crate) const LID_REMINDER_TIME: u32 = 0x8502;
pub(crate) const LID_REMINDER_SET: u32 = 0x8503;
pub(crate) const LID_REMINDER_SIGNAL_TIME: u32 = 0x8560;
pub(crate) const LID_FLAG_REQUEST: u32 = 0x8530;
// Task (PSETID_Task).
pub(crate) const LID_TASK_STATUS: u32 = 0x8101;
pub(crate) const LID_PERCENT_COMPLETE: u32 = 0x8102;
pub(crate) const LID_TASK_START: u32 = 0x8104;
pub(crate) const LID_TASK_DUE: u32 = 0x8105;
pub(crate) const LID_TASK_DATE_COMPLETED: u32 = 0x810F;
pub(crate) const LID_TASK_COMPLETE: u32 = 0x811C;
// Address (PSETID_Address).
pub(crate) const LID_FILE_UNDER: u32 = 0x8005;
pub(crate) const LID_EMAIL1: u32 = 0x8083;
pub(crate) const LID_EMAIL2: u32 = 0x8093;
pub(crate) const LID_EMAIL3: u32 = 0x80A3;
pub(crate) const LID_IM_ADDRESS: u32 = 0x8062;
pub(crate) const LID_WORK_STREET: u32 = 0x8045;
pub(crate) const LID_WORK_CITY: u32 = 0x8046;
pub(crate) const LID_WORK_STATE: u32 = 0x8047;
pub(crate) const LID_WORK_POSTAL: u32 = 0x8048;
pub(crate) const LID_WORK_COUNTRY: u32 = 0x8049;
// Note (PSETID_Note).
pub(crate) const LID_NOTE_COLOR: u32 = 0x8B00;

/// Every named property the mapping reads: (property set, LID).
pub(crate) const NAMED: &[([u8; 16], u32)] = &[
    (PSETID_APPOINTMENT, LID_BUSY_STATUS),
    (PSETID_APPOINTMENT, LID_LOCATION),
    (PSETID_APPOINTMENT, LID_START_WHOLE),
    (PSETID_APPOINTMENT, LID_END_WHOLE),
    (PSETID_APPOINTMENT, LID_APPOINTMENT_RECUR),
    (PSETID_APPOINTMENT, LID_STATE_FLAGS),
    (PSETID_APPOINTMENT, LID_SUB_TYPE),
    (PSETID_APPOINTMENT, LID_RECURRING),
    (PSETID_MEETING, LID_GLOBAL_OBJECT_ID),
    (PSETID_MEETING, LID_CLEAN_GLOBAL_OBJECT_ID),
    (PSETID_COMMON, LID_REMINDER_DELTA),
    (PSETID_COMMON, LID_REMINDER_TIME),
    (PSETID_COMMON, LID_REMINDER_SET),
    (PSETID_COMMON, LID_REMINDER_SIGNAL_TIME),
    (PSETID_COMMON, LID_FLAG_REQUEST),
    (PSETID_TASK, LID_TASK_STATUS),
    (PSETID_TASK, LID_PERCENT_COMPLETE),
    (PSETID_TASK, LID_TASK_START),
    (PSETID_TASK, LID_TASK_DUE),
    (PSETID_TASK, LID_TASK_DATE_COMPLETED),
    (PSETID_TASK, LID_TASK_COMPLETE),
    (PSETID_ADDRESS, LID_FILE_UNDER),
    (PSETID_ADDRESS, LID_EMAIL1),
    (PSETID_ADDRESS, LID_EMAIL2),
    (PSETID_ADDRESS, LID_EMAIL3),
    (PSETID_ADDRESS, LID_IM_ADDRESS),
    (PSETID_ADDRESS, LID_WORK_STREET),
    (PSETID_ADDRESS, LID_WORK_CITY),
    (PSETID_ADDRESS, LID_WORK_STATE),
    (PSETID_ADDRESS, LID_WORK_POSTAL),
    (PSETID_ADDRESS, LID_WORK_COUNTRY),
    (PSETID_NOTE, LID_NOTE_COLOR),
];

/// Tagged properties of contacts, appointments, tasks and notes (besides the mail ones).
pub(crate) const TAGS: &[u16] = &[
    0x3A05, 0x3A06, 0x3A08, 0x3A09, 0x3A11, 0x3A16, 0x3A17, 0x3A18, 0x3A19, 0x3A1A, 0x3A1B, 0x3A1C, 0x3A1E,
    0x3A1F, 0x3A21, 0x3A24, 0x3A25, 0x3A2F, 0x3A30, 0x3A41, 0x3A42, 0x3A44, 0x3A45, 0x3A46, 0x3A48, 0x3A4E,
    0x3A4F, 0x3A50, 0x3A51, 0x3A59, 0x3A5A, 0x3A5B, 0x3A5C, 0x3A5D, 0x3A5F, 0x3A60, 0x3A61, 0x3A62, 0x3A63,
    0x1090, 0x1091,
];

/// The file's named properties: (property set, LID) and (property set, name) to property id.
#[derive(Debug, Default)]
pub(crate) struct Names {
    pub by_lid: HashMap<([u8; 16], u32), u16>,
    pub by_name: HashMap<([u8; 16], String), u16>,
}

impl Names {
    pub fn lid(&self, set: [u8; 16], lid: u32) -> Option<u16> {
        self.by_lid.get(&(set, lid)).copied()
    }
    pub fn name(&self, set: [u8; 16], name: &str) -> Option<u16> {
        self.by_name.get(&(set, name.to_owned())).copied()
    }

    /// Reads the name-to-id map's streams ([MS-PST] §2.4.7): GUIDs, NAMEID entries and names.
    pub fn parse(guids: &[u8], entries: &[u8], strings: &[u8]) -> Names {
        const PS_MAPI: [u8; 16] =
            [0x28, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];
        let mut names = Names::default();
        for e in entries.as_chunks::<8>().0.iter().take(65_536) {
            let property = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
            let guid_kind = u16::from_le_bytes([e[4], e[5]]);
            let index = u16::from_le_bytes([e[6], e[7]]);
            let Some(id) = 0x8000u16.checked_add(index) else { continue };
            let guid_index = usize::from(guid_kind >> 1);
            let set = match guid_index {
                1 => PS_MAPI,
                2 => PS_PUBLIC_STRINGS,
                n if n >= 3 => {
                    let start = (n - 3).saturating_mul(16);
                    match guids
                        .get(start..start.saturating_add(16))
                        .and_then(|g| <[u8; 16]>::try_from(g).ok())
                    {
                        Some(g) => g,
                        None => continue,
                    }
                }
                _ => continue,
            };
            if guid_kind & 1 == 0 {
                names.by_lid.entry((set, property)).or_insert(id);
            } else {
                let Ok(off) = usize::try_from(property) else { continue };
                let Some(len) = strings
                    .get(off..off.saturating_add(4))
                    .and_then(|b| <[u8; 4]>::try_from(b).ok())
                    .map(u32::from_le_bytes)
                    .and_then(|l| usize::try_from(l).ok())
                else {
                    continue;
                };
                let start = off.saturating_add(4);
                let Some(bytes) = strings.get(start..start.saturating_add(len.min(2048))) else { continue };
                let name = crate::msg::props::from_utf16le(bytes);
                names.by_name.entry((set, name)).or_insert(id);
            }
        }
        names
    }

    /// The ids of every named property the mapping reads, plus `Keywords`.
    pub fn wanted(&self) -> Vec<u16> {
        let mut out: Vec<u16> = NAMED.iter().filter_map(|(s, l)| self.lid(*s, *l)).collect();
        out.extend(self.keywords());
        out
    }

    /// The categories property (`Keywords` in PS_PUBLIC_STRINGS).
    pub fn keywords(&self) -> Option<u16> {
        self.name(PS_PUBLIC_STRINGS, "Keywords")
    }
}

/// Reads named and tagged properties of one item.
pub(crate) struct Item<'a> {
    pub props: &'a Props,
    pub names: &'a Names,
    pub offset_min: i32,
}

impl Item<'_> {
    fn named(&self, set: [u8; 16], lid: u32) -> Option<u16> {
        self.names.lid(set, lid)
    }
    pub fn text(&self, id: u16) -> String {
        self.props.text(id).unwrap_or_default()
    }
    pub fn named_text(&self, set: [u8; 16], lid: u32) -> String {
        self.named(set, lid).and_then(|id| self.props.text(id)).unwrap_or_default()
    }
    pub fn named_int(&self, set: [u8; 16], lid: u32) -> Option<i64> {
        self.named(set, lid).and_then(|id| self.props.int(id))
    }
    pub fn named_bool(&self, set: [u8; 16], lid: u32) -> Option<bool> {
        self.named(set, lid).and_then(|id| match self.props.get(id, PT_BOOLEAN) {
            Some(Value::Bool(b)) => Some(*b),
            _ => self.props.int(id).map(|v| v != 0),
        })
    }
    pub fn named_double(&self, set: [u8; 16], lid: u32) -> Option<f64> {
        self.named(set, lid).and_then(|id| match self.props.get(id, PT_DOUBLE) {
            Some(Value::Float(f)) if f.is_finite() => Some(*f),
            _ => None,
        })
    }
    pub fn named_binary(&self, set: [u8; 16], lid: u32) -> Option<&[u8]> {
        self.named(set, lid).and_then(|id| match self.props.get(id, PT_BINARY) {
            Some(Value::Bin(b)) => Some(b.as_slice()),
            _ => None,
        })
    }
    fn at(&self, ft: u64) -> Option<DateTime> {
        // FILETIMEs of 4500-08-31 and later are "none" in MAPI.
        (ft != 0 && ft < 0x0CB3_4557_4D96_0000)
            .then(|| DateTime::from_unix(filetime_to_unix(ft), self.offset_min))
    }
    pub fn time(&self, id: u16) -> Option<DateTime> {
        self.props.time(id).and_then(|t| self.at(t))
    }
    pub fn named_time(&self, set: [u8; 16], lid: u32) -> Option<DateTime> {
        self.named(set, lid).and_then(|id| match self.props.get(id, PT_SYSTIME) {
            Some(Value::Time(t)) => self.at(*t),
            _ => None,
        })
    }
    pub fn categories(&self) -> Vec<String> {
        let Some(k) = self.names.keywords() else { return Vec::new() };
        match self.props.get(k, 0x101F).or_else(|| self.props.get(k, 0x101E)) {
            Some(Value::MultiStr(v)) => {
                v.iter().map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).take(256).collect()
            }
            _ => Vec::new(),
        }
    }
    pub fn private(&self) -> bool {
        self.props.int(0x0036) == Some(2)
    }
    pub fn body(&self) -> String {
        self.props.string(0x1000).unwrap_or_default()
    }
}

/// `YYYY-MM-DD` of a date property (birthdays and anniversaries are dates without a time).
fn ymd(t: Option<DateTime>) -> Option<String> {
    let t = t?;
    // Birthdays are stored as local midnight in UTC; noon keeps the date on either side.
    let days = t.unix.saturating_add(12 * 3600).div_euclid(86_400);
    let (y, m, d) = mailcraft_model::dates::civil_from_days(days);
    (1..=9999).contains(&y).then(|| format!("{y:04}-{m:02}-{d:02}"))
}

/// A contact ([MS-OXOCNTC]).
pub(crate) fn contact(it: &Item<'_>) -> Contact {
    let mut c = Contact {
        first_name: it.text(0x3A06),
        middle_name: it.text(0x3A44),
        last_name: it.text(0x3A11),
        title: it.text(0x3A45),
        suffix: it.text(0x3A05),
        file_as: it.named_text(PSETID_ADDRESS, LID_FILE_UNDER),
        company: it.text(0x3A16),
        job_title: it.text(0x3A17),
        department: it.text(0x3A18),
        office: it.text(0x3A19),
        profession: it.text(0x3A46),
        manager: it.text(0x3A4E),
        assistant: it.text(0x3A30),
        nickname: it.text(0x3A4F),
        spouse: it.text(0x3A48),
        web_page: {
            let w = it.text(0x3A51);
            if w.is_empty() { it.text(0x3A50) } else { w }
        },
        im_address: it.named_text(PSETID_ADDRESS, LID_IM_ADDRESS),
        birthday: ymd(it.time(0x3A42)),
        anniversary: ymd(it.time(0x3A41)),
        notes: it.body(),
        categories: it.categories(),
        private: it.private(),
        ..Default::default()
    };
    for lid in [LID_EMAIL1, LID_EMAIL2, LID_EMAIL3] {
        let e = it.named_text(PSETID_ADDRESS, lid);
        if !e.is_empty() && !c.emails.iter().any(|x| x.eq_ignore_ascii_case(&e)) {
            c.emails.push(e);
        }
    }
    for (id, label) in [
        (0x3A08u16, "Business"),
        (0x3A1B, "Business 2"),
        (0x3A09, "Home"),
        (0x3A2F, "Home 2"),
        (0x3A1C, "Mobile"),
        (0x3A24, "Business Fax"),
        (0x3A25, "Home Fax"),
        (0x3A21, "Pager"),
        (0x3A1E, "Car"),
        (0x3A1A, "Primary"),
        (0x3A1F, "Other"),
    ] {
        let n = it.text(id);
        if !n.is_empty() {
            c.phones.push((label.to_owned(), n));
        }
    }
    let business = PostalAddress {
        kind: "Business".into(),
        street: it.named_text(PSETID_ADDRESS, LID_WORK_STREET),
        city: it.named_text(PSETID_ADDRESS, LID_WORK_CITY),
        region: it.named_text(PSETID_ADDRESS, LID_WORK_STATE),
        postal_code: it.named_text(PSETID_ADDRESS, LID_WORK_POSTAL),
        country: it.named_text(PSETID_ADDRESS, LID_WORK_COUNTRY),
    };
    let home = PostalAddress {
        kind: "Home".into(),
        street: it.text(0x3A5D),
        city: it.text(0x3A59),
        region: it.text(0x3A5C),
        postal_code: it.text(0x3A5B),
        country: it.text(0x3A5A),
    };
    let other = PostalAddress {
        kind: "Other".into(),
        street: it.text(0x3A63),
        city: it.text(0x3A5F),
        region: it.text(0x3A62),
        postal_code: it.text(0x3A61),
        country: it.text(0x3A60),
    };
    for a in [business, home, other] {
        if [&a.street, &a.city, &a.region, &a.postal_code, &a.country].iter().any(|f| !f.is_empty()) {
            c.addresses.push(a);
        }
    }
    if c.first_name.is_empty() && c.last_name.is_empty() {
        // A contact saved with a display name only.
        let display = it.text(0x3001);
        let display = if display.is_empty() { it.text(0x0037) } else { display };
        match display.rsplit_once(' ') {
            Some((first, last)) => {
                c.first_name = first.trim().to_owned();
                c.last_name = last.trim().to_owned();
            }
            None => c.first_name = display,
        }
    }
    if c.file_as.is_empty() {
        c.file_as = match (c.last_name.is_empty(), c.first_name.is_empty()) {
            (false, false) => format!("{}, {}", c.last_name, c.first_name),
            (false, true) => c.last_name.clone(),
            (true, false) => c.first_name.clone(),
            (true, true) => c.company.clone(),
        };
    }
    c
}

/// Longest object id taken as a UID (a longer one is damaged; the caller makes a UID instead).
const MAX_OBJECT_ID: usize = 512;

/// The iCalendar UID of a calendar item from its object id ([MS-OXOCAL] §2.2.1.27 and §2.2.1.28, mapped
/// as [MS-OXCICAL] §2.1.3.1.1.20.26 does): the UID carried inside it ("vCal-Uid" data) when there is one,
/// else the id in upper-case hex with its instance date cleared. `None` when the item has neither id.
pub(crate) fn meeting_uid(it: &Item<'_>) -> Option<String> {
    let (id, clean) = match it.named_binary(PSETID_MEETING, LID_CLEAN_GLOBAL_OBJECT_ID) {
        Some(b) => (b, true),
        None => (it.named_binary(PSETID_MEETING, LID_GLOBAL_OBJECT_ID)?, false),
    };
    // Byte array id (16), instance date (4), creation time (8), reserved (8), data size (4), data.
    if id.len() < 40 || id.len() > MAX_OBJECT_ID {
        return None;
    }
    let size = id.get(36..40).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_le_bytes)?;
    let data = id.get(40..).unwrap_or_default();
    let data = data.get(..usize::try_from(size).ok()?.min(data.len())).unwrap_or(data);
    if let Some(rest) = data.strip_prefix(b"vCal-Uid\x01\x00\x00\x00") {
        let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
        let uid = String::from_utf8_lossy(rest.get(..end).unwrap_or_default()).trim().to_owned();
        if !uid.is_empty() && uid.len() <= 255 && !uid.chars().any(char::is_control) {
            return Some(uid);
        }
    }
    let mut bytes = id.to_vec();
    if !clean && let Some(date) = bytes.get_mut(16..20) {
        date.fill(0);
    }
    Some(bytes.iter().map(|b| format!("{b:02X}")).collect())
}

/// An appointment ([MS-OXOCAL]); the second value is a note when its recurrence could not be read.
pub(crate) fn appointment(
    it: &Item<'_>,
    organizer: Option<Mailbox>,
    recipients: &[Props],
    uid: String,
) -> (Appointment, Option<String>) {
    let start = it.named_time(PSETID_APPOINTMENT, LID_START_WHOLE).or_else(|| it.time(0x0060));
    let end = it.named_time(PSETID_APPOINTMENT, LID_END_WHOLE).or_else(|| it.time(0x0061));
    let start = start.or(end).unwrap_or_else(|| DateTime::from_unix(0, it.offset_min));
    let end = end.filter(|e| e.unix >= start.unix).unwrap_or(start);
    let show_as = match it.named_int(PSETID_APPOINTMENT, LID_BUSY_STATUS) {
        Some(0) => BusyStatus::Free,
        Some(1) => BusyStatus::Tentative,
        Some(3) => BusyStatus::OutOfOffice,
        Some(4) => BusyStatus::WorkingElsewhere,
        _ => BusyStatus::Busy,
    };
    let reminder_min = if it.named_bool(PSETID_COMMON, LID_REMINDER_SET).unwrap_or(false) {
        it.named_int(PSETID_COMMON, LID_REMINDER_DELTA).and_then(|m| u32::try_from(m).ok()).or(Some(15))
    } else {
        None
    };
    let is_meeting = it.named_int(PSETID_APPOINTMENT, LID_STATE_FLAGS).is_some_and(|f| f & 1 != 0);
    let mut attendees = Vec::new();
    for r in recipients.iter().take(crate::msg::MAX_RECIPIENTS) {
        // recipOrganizer (0x0002) in PidTagRecipientFlags marks the organizer's own row.
        if r.int(0x5FFD).is_some_and(|f| f & 0x2 != 0) {
            continue;
        }
        let addr = r.text(0x39FE).or_else(|| r.text(0x3003).filter(|e| e.contains('@')));
        let Some(addr) = addr else { continue };
        let name = r.text(0x3001).filter(|n| !n.eq_ignore_ascii_case(&addr));
        let role = match r.int(0x0C15).map(|t| t & 0xF) {
            Some(2) => AttendeeRole::Optional,
            Some(3) => AttendeeRole::Resource,
            _ => AttendeeRole::Required,
        };
        let response = match r.int(0x5FFF) {
            Some(3) => ResponseStatus::Accepted,
            Some(2) => ResponseStatus::Tentative,
            Some(4) => ResponseStatus::Declined,
            _ => ResponseStatus::None,
        };
        attendees.push(Attendee { mailbox: Mailbox::new(name.as_deref(), &addr), role, response });
    }
    let mut note = None;
    let mut recurrence = None;
    let recurring = it.named_bool(PSETID_APPOINTMENT, LID_RECURRING).unwrap_or(false);
    if let Some(blob) = it.named_binary(PSETID_APPOINTMENT, LID_APPOINTMENT_RECUR) {
        match recurrence_rule(blob, it.offset_min) {
            Ok(r) => recurrence = Some(r),
            Err(why) => note = Some(format!("repeats, but {why}; imported as a single occurrence")),
        }
    } else if recurring {
        note = Some("repeats, but has no recurrence pattern; imported as a single occurrence".into());
    }
    let all_day = it.named_bool(PSETID_APPOINTMENT, LID_SUB_TYPE).unwrap_or(false);
    let importance = match it.props.int(0x0017) {
        Some(0) => Importance::Low,
        Some(2) => Importance::High,
        _ => Importance::Normal,
    };
    let html = it.props.binary(0x1013).map(|b| String::from_utf8_lossy(b).trim_end_matches('\0').to_owned());
    let a = Appointment {
        uid,
        subject: it.text(0x0037),
        location: it.named_text(PSETID_APPOINTMENT, LID_LOCATION),
        start,
        end,
        all_day,
        show_as,
        reminder_min,
        organizer: if is_meeting { organizer } else { None },
        attendees: if is_meeting { attendees } else { Vec::new() },
        recurrence,
        categories: it.categories(),
        private: it.private(),
        body: it.body(),
        is_meeting,
        importance,
        body_html: html.filter(|h| !h.trim().is_empty()),
        ..Default::default()
    };
    (a, note)
}

/// A task ([MS-OXOTASK]).
pub(crate) fn task(it: &Item<'_>) -> Task {
    let status = match it.named_int(PSETID_TASK, LID_TASK_STATUS) {
        Some(1) => TaskStatus::InProgress,
        Some(2) => TaskStatus::Completed,
        Some(3) => TaskStatus::WaitingOnSomeoneElse,
        Some(4) => TaskStatus::Deferred,
        _ if it.named_bool(PSETID_TASK, LID_TASK_COMPLETE) == Some(true) => TaskStatus::Completed,
        _ => TaskStatus::NotStarted,
    };
    let percent = it
        .named_double(PSETID_TASK, LID_PERCENT_COMPLETE)
        .map(|p| (p.clamp(0.0, 1.0) * 100.0).round() as u8)
        .unwrap_or(if status == TaskStatus::Completed { 100 } else { 0 });
    let reminder = if it.named_bool(PSETID_COMMON, LID_REMINDER_SET).unwrap_or(false) {
        it.named_time(PSETID_COMMON, LID_REMINDER_TIME)
            .or_else(|| it.named_time(PSETID_COMMON, LID_REMINDER_SIGNAL_TIME))
    } else {
        None
    };
    Task {
        subject: it.text(0x0037),
        start: it.named_time(PSETID_TASK, LID_TASK_START),
        due: it.named_time(PSETID_TASK, LID_TASK_DUE),
        status,
        priority: match it.props.int(0x0017) {
            Some(0) => TaskPriority::Low,
            Some(2) => TaskPriority::High,
            _ => TaskPriority::Normal,
        },
        percent_complete: percent,
        reminder,
        body: it.body(),
        categories: it.categories(),
        completed: if status == TaskStatus::Completed {
            it.named_time(PSETID_TASK, LID_TASK_DATE_COMPLETED)
        } else {
            None
        },
        private: it.private(),
        modified: it.time(0x3008),
        ..Default::default()
    }
}

/// A note ([MS-OXONOTE]).
pub(crate) fn note(it: &Item<'_>, now: DateTime) -> Note {
    let color = match it.named_int(PSETID_NOTE, LID_NOTE_COLOR) {
        Some(0) => NoteColor::Blue,
        Some(1) => NoteColor::Green,
        Some(2) => NoteColor::Pink,
        Some(4) => NoteColor::White,
        _ => NoteColor::Yellow,
    };
    let mut text = it.body();
    if text.trim().is_empty() {
        text = it.text(0x0037);
    }
    let created = it.time(0x3007).unwrap_or(now);
    Note {
        text,
        color,
        created,
        modified: it.time(0x3008).unwrap_or(created),
        categories: it.categories(),
        ..Default::default()
    }
}

/// Minutes since 1601-01-01 to Unix seconds.
fn minutes_to_unix(m: u32) -> i64 {
    i64::from(m).saturating_mul(60).saturating_sub(11_644_473_600)
}

/// Little-endian reader over a recurrence blob.
struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn u16(&mut self) -> Result<u16, String> {
        let b = self.data.get(self.at..self.at.saturating_add(2)).ok_or("the pattern is cut short")?;
        self.at = self.at.saturating_add(2);
        Ok(u16::from_le_bytes([*b.first().ok_or("short")?, *b.get(1).ok_or("short")?]))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.data.get(self.at..self.at.saturating_add(4)).ok_or("the pattern is cut short")?;
        self.at = self.at.saturating_add(4);
        let a: [u8; 4] = b.try_into().map_err(|_| "the pattern is cut short")?;
        Ok(u32::from_le_bytes(a))
    }
    fn list(&mut self, max: u32) -> Result<Vec<u32>, String> {
        let n = self.u32()?;
        if n > max {
            return Err(format!("the pattern lists {n} dates"));
        }
        (0..n).map(|_| self.u32()).collect()
    }
}

/// Most deleted occurrences kept per series.
const MAX_EXDATES: usize = 10_000;

const DAYS: [&str; 7] = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];

fn by_day(mask: u32) -> Result<String, String> {
    let days: Vec<&str> =
        DAYS.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, d)| *d).collect();
    if days.is_empty() { Err("the pattern names no weekday".into()) } else { Ok(days.join(",")) }
}

/// An RFC 5545 rule (and the deleted occurrences) from an appointment recurrence blob
/// (PidLidAppointmentRecur, [MS-OXOCAL] §2.2.1.44.1 and §2.2.1.44.5). Times in the blob are local; they are
/// given the UTC offset `offset_min`. Changed occurrences (exceptions) keep the series' pattern.
pub fn recurrence_rule(blob: &[u8], offset_min: i32) -> Result<Recurrence, String> {
    let mut c = Cursor { data: blob, at: 0 };
    let reader = c.u16()?;
    let writer = c.u16()?;
    if reader != 0x3004 || writer != 0x3004 {
        return Err("the pattern has an unknown version".into());
    }
    let frequency = c.u16()?;
    let pattern = c.u16()?;
    let calendar = c.u16()?;
    if calendar != 0 && calendar != 1 {
        return Err("the pattern uses a calendar other than the Gregorian one".into());
    }
    let _first = c.u32()?;
    let period = c.u32()?;
    let _sliding = c.u32()?;
    let (mask, day, nth) = match pattern {
        0x0000 => (0, 0, 0),
        0x0001 => (c.u32()?, 0, 0),
        0x0002 | 0x0004 => (0, c.u32()?, 0),
        0x0003 => {
            let m = c.u32()?;
            (m, 0, c.u32()?)
        }
        _ => return Err("the pattern uses a lunar calendar".into()),
    };
    let end_type = c.u32()?;
    let count = c.u32()?;
    let first_dow = c.u32()?;
    let deleted = c.list(100_000)?;
    let modified = c.list(100_000)?;
    let start_date = c.u32()?;
    let end_date = c.u32()?;
    // AppointmentRecurrencePattern: the occurrences' start time of day.
    let start_offset = match (c.u32(), c.u32(), c.u32()) {
        (Ok(_reader2), Ok(_writer2), Ok(off)) => off,
        _ => 0,
    };

    let mut rule = match (frequency, pattern) {
        (0x200A, 0x0000) => {
            if period == 0 || period % 1440 != 0 {
                return Err("the daily interval is not a whole number of days".into());
            }
            format!("FREQ=DAILY;INTERVAL={}", period / 1440)
        }
        (0x200B | 0x200A, 0x0001) => {
            format!("FREQ=WEEKLY;INTERVAL={};BYDAY={}", period.max(1), by_day(mask)?)
        }
        (0x200C, 0x0002) => {
            format!("FREQ=MONTHLY;INTERVAL={};BYMONTHDAY={}", period.max(1), day.clamp(1, 31))
        }
        (0x200C, 0x0004) => format!("FREQ=MONTHLY;INTERVAL={};BYMONTHDAY=-1", period.max(1)),
        (0x200C, 0x0003) => format!(
            "FREQ=MONTHLY;INTERVAL={};BYDAY={};BYSETPOS={}",
            period.max(1),
            by_day(mask)?,
            if nth >= 5 { -1 } else { i64::from(nth.max(1)) }
        ),
        (0x200D, 0x0002..=0x0004) => {
            let (_, month, _) =
                mailcraft_model::dates::civil_from_days(minutes_to_unix(start_date).div_euclid(86_400));
            let years = (period / 12).max(1);
            match pattern {
                0x0002 => {
                    format!("FREQ=YEARLY;INTERVAL={years};BYMONTH={month};BYMONTHDAY={}", day.clamp(1, 31))
                }
                0x0004 => format!("FREQ=YEARLY;INTERVAL={years};BYMONTH={month};BYMONTHDAY=-1"),
                _ => format!(
                    "FREQ=YEARLY;INTERVAL={years};BYMONTH={month};BYDAY={};BYSETPOS={}",
                    by_day(mask)?,
                    if nth >= 5 { -1 } else { i64::from(nth.max(1)) }
                ),
            }
        }
        _ => return Err("the pattern's frequency is not understood".into()),
    };
    if let Some(d) = DAYS.get(usize::try_from(first_dow).unwrap_or(0))
        && first_dow != 0
    {
        rule.push_str(&format!(";WKST={d}"));
    }
    let local = |minutes: u32| minutes_to_unix(minutes).saturating_sub(i64::from(offset_min) * 60);
    match end_type {
        0x2021 => {
            // The last occurrence's date, local midnight: UNTIL at the end of that day, in UTC.
            let until = local(end_date).saturating_add(86_399);
            let t = DateTime::from_unix(until, 0);
            rule.push_str(&format!(";UNTIL={}", utc_stamp(t)));
        }
        0x2022 => {
            if count == 0 {
                return Err("the pattern ends after no occurrences".into());
            }
            rule.push_str(&format!(";COUNT={count}"));
        }
        _ => {}
    }
    // A set: both lists may be long, and a scan per date would take quadratic time on a hostile pattern.
    let modified: std::collections::HashSet<u32> = modified.into_iter().collect();
    let exdates = deleted
        .iter()
        .filter(|d| !modified.contains(d))
        .take(MAX_EXDATES)
        .map(|d| DateTime::from_unix(local(*d).saturating_add(i64::from(start_offset) * 60), offset_min))
        .collect();
    Ok(Recurrence { rrule: rule, exdates })
}

/// `YYYYMMDDTHHMMSSZ`.
fn utc_stamp(t: DateTime) -> String {
    let days = t.unix.div_euclid(86_400);
    let secs = t.unix.rem_euclid(86_400);
    let (y, m, d) = mailcraft_model::dates::civil_from_days(days);
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", secs / 3600, (secs / 60) % 60, secs % 60)
}
