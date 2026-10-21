//! Deterministic sample mailbox for demos, screenshots and tests (synthetic people at example.com/.org/.net).
//!
//! Every message is built as a real RFC 5322 message with [`MessageBuilder`] and parsed back through
//! [`MailItem::from_message`], so bodies, sizes, attachments and previews look like real mail.

use mailcraft_mime::{Address, AttachmentSpec, MessageBuilder};
use mailcraft_model::{
    AccountId, Appointment, Attendee, BusyStatus, Contact, DateTime, FlagState, FocusClass, FolderId,
    FolderRole, MailItem, Mailbox, Note, NoteColor, Recurrence, ResponseState, Task, TaskPriority,
    TaskStatus, thread::topic_key,
};

use crate::Session;

/// The sample account's display name.
pub const SAMPLE_ACCOUNT: &str = "MailCraft Sample";
/// The sample user ("me").
pub const SAMPLE_USER_NAME: &str = "Jordan Lee";
pub const SAMPLE_USER_ADDR: &str = "jordan.lee@example.net";

/// Fixed "now" used by [`sample_session`]: Friday 9 October 2026, 2:30 PM UTC.
pub fn sample_now() -> DateTime {
    mailcraft_model::dates::local_datetime(2026, 10, 9, 14, 30, 0, 0).unwrap_or_default()
}

/// A session over [`sample_mailbox`] with the clock fixed at [`sample_now`] (UTC display).
pub fn sample_session() -> Session {
    let now = sample_now();
    let mut s = Session::new(sample_mailbox(now));
    s.fixed_now = Some(now);
    s.tz_offset_min = 0;
    s
}

type Person = (&'static str, &'static str);

const ME: Person = (SAMPLE_USER_NAME, SAMPLE_USER_ADDR);
const ALICE: Person = ("Alice Rivera", "alice@example.com");
const BOB: Person = ("Bob Chen", "bob.chen@example.org");
const CAROL: Person = ("Carol Okafor", "carol@example.com");
const DEV: Person = ("Dev Patel", "dev.patel@example.net");
const ERIN: Person = ("Erin Walsh", "erin@example.org");
const FRANK: Person = ("Frank Moreau", "frank@example.com");
const GRACE: Person = ("Grace Kim", "grace.kim@example.org");
const HIRO: Person = ("Hiro Tanaka", "hiro@example.net");
const WEEKLY: Person = ("Example Weekly", "news@example.org");
const TRAIL: Person = ("Trail Gear Outfitters", "deals@example.com");
const STORE: Person = ("Example Store", "orders@example.com");
const IT: Person = ("IT Support", "it-support@example.com");
const PRIZE: Person = ("Prize Center", "winner@example.net");
const PHARMA: Person = ("Online Deals", "offers@example.net");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Role(FolderRole),
    Projects,
    Receipts,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flag {
    None,
    /// Label and due date in days from today.
    FollowUp(&'static str, i64),
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Resp {
    None,
    Replied,
    Forwarded,
}

struct Msg {
    place: Place,
    from: Person,
    to: Vec<Person>,
    cc: Vec<Person>,
    subject: &'static str,
    text: String,
    html: Option<String>,
    days: i64,
    hour: i64,
    minute: i64,
    importance: Option<&'static str>,
    read: bool,
    flag: Flag,
    categories: Vec<&'static str>,
    attachments: Vec<(&'static str, &'static str, usize)>,
    other: bool,
    draft: bool,
    /// Index of the message this one replies to (threading headers).
    reply_of: Option<usize>,
    response: Resp,
}

fn msg(
    place: Place,
    from: Person,
    subject: &'static str,
    days: i64,
    hour: i64,
    minute: i64,
    text: &str,
) -> Msg {
    Msg {
        place,
        from,
        to: vec![ME],
        cc: Vec::new(),
        subject,
        text: text.to_owned(),
        html: None,
        days,
        hour,
        minute,
        importance: None,
        read: true,
        flag: Flag::None,
        categories: Vec::new(),
        attachments: Vec::new(),
        other: false,
        draft: false,
        reply_of: None,
        response: Resp::None,
    }
}

impl Msg {
    fn unread(mut self) -> Self {
        self.read = false;
        self
    }
    fn to(mut self, to: &[Person]) -> Self {
        self.to = to.to_vec();
        self
    }
    fn cc(mut self, cc: &[Person]) -> Self {
        self.cc = cc.to_vec();
        self
    }
    fn html(mut self, html: String) -> Self {
        self.html = Some(html);
        self
    }
    fn high(mut self) -> Self {
        self.importance = Some("high");
        self
    }
    fn low(mut self) -> Self {
        self.importance = Some("low");
        self
    }
    fn flag(mut self, f: Flag) -> Self {
        self.flag = f;
        self
    }
    fn cats(mut self, c: &[&'static str]) -> Self {
        self.categories = c.to_vec();
        self
    }
    fn attach(mut self, name: &'static str, mime: &'static str, size: usize) -> Self {
        self.attachments.push((name, mime, size));
        self
    }
    fn other(mut self) -> Self {
        self.other = true;
        self
    }
    fn draft(mut self) -> Self {
        self.draft = true;
        self
    }
    fn reply_of(mut self, i: usize) -> Self {
        self.reply_of = Some(i);
        self
    }
    fn responded(mut self, r: Resp) -> Self {
        self.response = r;
        self
    }
}

/// Deterministic filler bytes for attachments (a recognisable header, then a repeating pattern).
fn filler(name: &str, size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(format!("MailCraft sample attachment: {name}\n").as_bytes());
    let mut x: u32 = 0x1234_5678;
    while out.len() < size {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        out.push(
            b"abcdefghijklmnopqrstuvwxyz0123456789 \n".get((x >> 16) as usize % 38).copied().unwrap_or(b' '),
        );
    }
    out.truncate(size);
    out
}

fn mailbox(p: Person) -> mailcraft_mime::Mailbox {
    mailcraft_mime::Mailbox::new(Some(p.0), p.1)
}

fn addresses(list: &[Person]) -> Vec<Address> {
    list.iter().map(|p| Address::Mailbox(mailbox(*p))).collect()
}

fn newsletter_html(title: &str, intro: &str, items: &[(&str, &str)]) -> String {
    let mut rows = String::new();
    for (head, body) in items {
        rows.push_str(&format!(
            "<tr><td style=\"padding:8px 0;border-bottom:1px solid #dddddd\"><h3 style=\"margin:0;color:#1a5fb4\">{head}</h3><p style=\"margin:4px 0 0 0\">{body}</p></td></tr>"
        ));
    }
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{title}</title></head><body style=\"font-family:Arial,sans-serif;color:#222222\">\
<table width=\"600\" cellpadding=\"0\" cellspacing=\"0\" align=\"center\"><tr><td style=\"background:#1a5fb4;color:#ffffff;padding:16px\"><h1 style=\"margin:0\">{title}</h1></td></tr>\
<tr><td style=\"padding:12px 0\"><p>{intro}</p></td></tr>{rows}\
<tr><td style=\"padding:16px 0;font-size:11px;color:#777777\">You are receiving this sample newsletter because you subscribed at example.org. <a href=\"https://example.org/unsubscribe\">Unsubscribe</a></td></tr></table></body></html>"
    )
}

fn plain_of(items: &[(&str, &str)], intro: &str) -> String {
    let mut s = format!("{intro}\n\n");
    for (h, b) in items {
        s.push_str(&format!("* {h}\n  {b}\n\n"));
    }
    s
}

/// The messages, oldest references first so replies can point back by index.
fn messages() -> Vec<Msg> {
    use FolderRole::*;
    use Place::Role;
    let weekly_items = [
        ("Ten tips for a tidy inbox", "Folders, rules and categories that keep mail under control."),
        ("Keyboard shortcuts worth learning", "Ctrl+R to reply, Ctrl+Shift+V to move, Insert to flag."),
        (
            "Reader question",
            "How do I find that attachment from last month? Try searching for hasattachments:yes.",
        ),
    ];
    let weekly_items_2 = [
        ("Calendar habits", "Block focus time before the week fills up."),
        ("Out of office etiquette", "Short, clear, and with a backup contact."),
    ];
    let trail_items = [
        ("Autumn sale", "Up to 30% off rain shells and insulated jackets this weekend only."),
        ("New arrivals", "Lightweight trekking poles and a two-person tent."),
    ];
    vec![
        // 0: thread root (Projects folder later replies)
        msg(Role(Inbox), ALICE, "Project kickoff", 9, 9, 15,
            "Hi Jordan,\n\nLet's kick off the website refresh next Tuesday at 10. I'll bring the draft timeline.\nCan you book the small meeting room?\n\nThanks,\nAlice")
            .cc(&[BOB]).cats(&["Blue category"]).responded(Resp::Replied),
        // 1: my reply (Sent Items)
        msg(Role(SentItems), ME, "RE: Project kickoff", 9, 10, 2,
            "Booked the small room for Tuesday 10:00.\n\n-----Original Message-----\nFrom: Alice Rivera <alice@example.com>\nSubject: Project kickoff\n\nLet's kick off the website refresh next Tuesday at 10.")
            .to(&[ALICE]).cc(&[BOB]).reply_of(0),
        // 2: Bob's reply to my reply
        msg(Role(Inbox), BOB, "RE: Project kickoff", 8, 8, 40,
            "Thanks both. I'll share the analytics summary before the meeting.\n\nBob")
            .to(&[ME, ALICE]).reply_of(1).cats(&["Blue category"]),
        // 3: today, unread, high importance
        msg(Role(Inbox), CAROL, "URGENT: server room access", 0, 7, 30,
            "The badge reader on the server room is down. Please use the side door today and sign the paper log.\n\nCarol")
            .unread().high().cc(&[DEV]),
        // 4: today, unread, flagged with attachment
        msg(Role(Inbox), DEV, "Quarterly report draft", 0, 9, 10,
            "Hi Jordan,\n\nAttached is the first draft of the quarterly report. Could you review sections 2 and 3 by Monday?\n\nDev")
            .unread().flag(Flag::FollowUp("Follow up", 3)).attach("Quarterly report draft.pdf", "application/pdf", 140_000),
        // 5: today, mention
        msg(Role(Inbox), GRACE, "Slides for Thursday", 0, 11, 45,
            "Great work on the slides, @Jordan Lee! I added two charts at the end. Let me know if the colours work.\n\nGrace")
            .unread().cats(&["Green category"]),
        // 6: yesterday, newsletter HTML (Other)
        msg(Role(Inbox), WEEKLY, "Example Weekly: inbox tips", 1, 6, 0,
            &plain_of(&weekly_items, "This week in Example Weekly"))
            .html(newsletter_html("Example Weekly", "This week in Example Weekly", &weekly_items)).other().unread(),
        // 7: yesterday, low importance
        msg(Role(Inbox), FRANK, "Lunch on Friday?", 1, 12, 5,
            "Anyone up for the noodle place on Friday? No pressure.\n\nFrank")
            .low().to(&[ME, ALICE, BOB]),
        // 8: two days ago, completed flag
        msg(Role(Inbox), ERIN, "Expense report approved", 2, 15, 20,
            "Your September expense report has been approved and will be paid with the next payroll.\n\nErin")
            .flag(Flag::Done).cats(&["Yellow category"]),
        // 9: this week, attachments (image)
        msg(Role(Inbox), HIRO, "Photos from the offsite", 3, 17, 0,
            "A few photos from the team offsite. More on the shared drive.\n\nHiro")
            .attach("offsite-1.jpg", "image/jpeg", 30_000).attach("agenda.docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document", 12_000),
        // 10: promo (Other)
        msg(Role(Inbox), TRAIL, "Autumn sale starts now", 3, 8, 0, &plain_of(&trail_items, "Trail Gear Outfitters"))
            .html(newsletter_html("Trail Gear Outfitters", "Gear up for autumn.", &trail_items)).other(),
        // 11: last week, flagged for reply, red + orange categories
        msg(Role(Inbox), ALICE, "Budget questions", 6, 10, 30,
            "Jordan, two questions on the Q4 budget:\n1. Is the travel line final?\n2. Can we move the training spend to January?\n\nAlice")
            .flag(Flag::FollowUp("Reply", 1)).cats(&["Red category", "Orange category"]),
        // 12: last week, forwarded
        msg(Role(Inbox), IT, "Password expires in 7 days", 7, 9, 0,
            "Your password expires in 7 days. Change it from the account portal at https://example.com/account before it expires.")
            .responded(Resp::Forwarded),
        // 13: last week
        msg(Role(Inbox), BOB, "Analytics summary", 8, 16, 45,
            "Summary of last month's site analytics:\n- Visits up 12%\n- Bounce rate down 3 points\n- Mobile now 61% of traffic\n\nBob")
            .attach("analytics.csv", "text/csv", 4_000),
        // 14: two weeks ago newsletter
        msg(Role(Inbox), WEEKLY, "Example Weekly: calendar habits", 12, 6, 0,
            &plain_of(&weekly_items_2, "This week in Example Weekly"))
            .html(newsletter_html("Example Weekly", "This week in Example Weekly", &weekly_items_2)).other(),
        // 15: two weeks ago
        msg(Role(Inbox), CAROL, "Desk move on the 20th", 13, 11, 0,
            "Facilities will move our desks to the third floor on the 20th. Please pack personal items the day before.\n\nCarol")
            .cats(&["Purple category"]),
        // 16: three weeks ago
        msg(Role(Inbox), DEV, "Code review checklist", 18, 14, 0,
            "Here is the checklist we agreed on for code reviews. Feedback welcome.\n\nDev")
            .attach("checklist.txt", "text/plain", 2_500),
        // 17: three weeks ago, unread
        msg(Role(Inbox), GRACE, "Team survey results", 20, 9, 30,
            "The team survey results are in. Highlights: more focus time, fewer recurring meetings.\n\nGrace")
            .unread(),
        // 18: older
        msg(Role(Inbox), HIRO, "Welcome to the team", 30, 9, 0,
            "Welcome aboard, Jordan! Your laptop is ready at the IT desk.\n\nHiro"),
        // 19: older, flagged no due
        msg(Role(Inbox), ERIN, "Benefits enrollment", 33, 13, 0,
            "Open enrollment for benefits closes at the end of the month. The forms are on the intranet.\n\nErin")
            .flag(Flag::FollowUp("Follow up", 0)),
        // 20-22: Projects folder
        msg(Place::Projects, ALICE, "Website refresh: timeline", 5, 10, 0,
            "Draft timeline:\n- Week 1: discovery\n- Week 2-3: design\n- Week 4-6: build\n- Week 7: launch\n\nAlice")
            .cats(&["Blue category"]),
        msg(Place::Projects, BOB, "Website refresh: hosting options", 4, 15, 30,
            "Compared three hosting options; the spreadsheet is attached.\n\nBob")
            .attach("hosting-options.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", 9_000)
            .unread(),
        msg(Place::Projects, CAROL, "Website refresh: accessibility audit", 11, 11, 0,
            "The accessibility audit found 14 issues, mostly colour contrast and missing labels.\n\nCarol")
            .flag(Flag::FollowUp("Review", 5)),
        // 23-24: Receipts
        msg(Place::Receipts, STORE, "Your order has shipped", 10, 8, 20,
            "Order 1042 has shipped and should arrive in 3-5 business days.\n\nThank you for shopping with Example Store.")
            .html("<html><body><h2>Your order has shipped</h2><p>Order <b>1042</b> should arrive in 3&ndash;5 business days.</p><table border=\"1\" cellpadding=\"4\"><tr><th>Item</th><th>Qty</th><th>Price</th></tr><tr><td>USB-C cable</td><td>2</td><td>$12.00</td></tr><tr><td>Laptop stand</td><td>1</td><td>$39.00</td></tr></table></body></html>".to_owned()),
        msg(Place::Receipts, STORE, "Receipt for order 1042", 10, 8, 5,
            "Receipt for order 1042. Total: $63.00.")
            .attach("receipt-1042.pdf", "application/pdf", 6_000),
        // 25-28: Sent Items
        msg(Role(SentItems), ME, "Re: Lunch on Friday?", 1, 12, 30, "Count me in!").to(&[FRANK]),
        msg(Role(SentItems), ME, "FW: Password expires in 7 days", 7, 9, 20,
            "FYI, the portal link is below.\n\n-----Original Message-----\nFrom: IT Support <it-support@example.com>\nSubject: Password expires in 7 days")
            .to(&[ERIN]),
        msg(Role(SentItems), ME, "Holiday schedule", 15, 16, 0,
            "Hi all, please add your holiday plans to the shared calendar by Friday.\n\nJordan")
            .to(&[ALICE, BOB, CAROL, DEV]),
        msg(Role(SentItems), ME, "Re: Code review checklist", 18, 15, 10, "Looks good to me. Thanks, Dev.").to(&[DEV]),
        // 29-30: Drafts
        msg(Role(Drafts), ME, "Status update", 0, 8, 0,
            "Hi team,\n\nThis week:\n- Finished the analytics review\n- Started the budget questions\n").to(&[ALICE]).draft(),
        msg(Role(Drafts), ME, "", 2, 18, 0, "Notes for the retro: ").to(&[]).draft(),
        // 31-32: Junk Email
        msg(Role(JunkEmail), PRIZE, "You have WON a prize!!!", 2, 3, 0,
            "Claim your prize now by replying with your bank details. This is a sample junk message."),
        msg(Role(JunkEmail), PHARMA, "Limited offer just for you", 5, 4, 0,
            "Huge discounts on everything. This is a sample junk message."),
        // 33-34: Deleted Items
        msg(Role(DeletedItems), TRAIL, "Last chance: summer clearance", 25, 7, 0, "Summer clearance ends tonight."),
        msg(Role(DeletedItems), FRANK, "Parking lot repaving", 16, 10, 0,
            "The parking lot will be repaved Saturday. Please park on the street.").unread(),
        // 35: Archive
        msg(Role(Archive), ALICE, "Q2 retrospective notes", 34, 15, 0,
            "Notes from the Q2 retrospective are attached.").attach("q2-retro.txt", "text/plain", 3_000),
        // 36-39: more Inbox mail spread through the weeks
        msg(Role(Inbox), BOB, "RE: Analytics summary", 8, 17, 30,
            "One more data point: newsletter sign-ups doubled.\n\nBob").reply_of(13).unread(),
        msg(Role(Inbox), FRANK, "Book club: October pick", 4, 19, 0,
            "October's book club pick is a short mystery novel. We meet on the 28th.\n\nFrank").cats(&["Green category"]),
        msg(Role(Inbox), IT, "Scheduled maintenance this weekend", 2, 10, 0,
            "Email may be unavailable for up to an hour on Saturday night during scheduled maintenance.")
            .low(),
        msg(Role(Inbox), CAROL, "Re: Desk move on the 20th", 13, 15, 0,
            "Update: the move is now on the 21st.\n\nCarol").reply_of(15).flag(Flag::Done),
    ]
}

/// The sample: local account "MailCraft Sample", ~40 messages over the last five weeks relative to `now`
/// (unread, flagged, completed, high/low importance, categories, attachments, HTML newsletters, a reply
/// thread, drafts, sent items, junk), plus a few appointments, contacts, tasks and notes.
pub fn sample_mailbox(now: DateTime) -> Mailbox {
    let mut mb = Mailbox::new_local(SAMPLE_ACCOUNT);
    let account = mb.default_account().map(|a| a.id).unwrap_or(AccountId(1));
    if let Some(a) = mb.accounts.iter_mut().find(|a| a.id == account) {
        a.address = SAMPLE_USER_ADDR.to_owned();
        a.sender_name = SAMPLE_USER_NAME.to_owned();
    }
    let role = |mb: &Mailbox, r: FolderRole| mb.role_folder(account, r).unwrap_or_default();
    let inbox = role(&mb, FolderRole::Inbox);
    let projects = mb.create_folder(inbox, "Projects").unwrap_or(inbox);
    let receipts = mb.create_folder(inbox, "Receipts").unwrap_or(inbox);

    let day_start = now.unix.div_euclid(86_400).saturating_mul(86_400);
    let specs = messages();
    let mut ids: Vec<String> = Vec::with_capacity(specs.len());
    for (i, m) in specs.iter().enumerate() {
        let mut t = day_start
            .saturating_sub(m.days.saturating_mul(86_400))
            .saturating_add(m.hour.saturating_mul(3600))
            .saturating_add(m.minute.saturating_mul(60));
        if t > now.unix {
            // Keep "today" items in the past whatever time `now` is.
            t = now.unix.saturating_sub(60 * (i64::try_from(i).unwrap_or(0) + 1));
        }
        let date = DateTime::from_unix(t, 0);
        let message_id =
            format!("sample-{i:02}.mailcraft@{}", m.from.1.rsplit('@').next().unwrap_or("example.com"));
        let parent = m.reply_of.and_then(|p| ids.get(p).cloned());
        let mut b = MessageBuilder::new();
        b.from = Some(mailbox(m.from));
        b.to = addresses(&m.to);
        b.cc = addresses(&m.cc);
        b.subject = m.subject.to_owned();
        b.date = Some(date);
        b.message_id = Some(message_id.clone());
        if let Some(p) = &parent {
            b.in_reply_to = vec![p.clone()];
            b.references = vec![p.clone()];
        }
        b.importance = m.importance.map(str::to_owned);
        b.text = Some(m.text.clone());
        b.html = m.html.clone();
        b.attachments = m
            .attachments
            .iter()
            .map(|(name, mime, size)| AttachmentSpec {
                filename: (*name).to_owned(),
                mime_type: (*mime).to_owned(),
                data: filler(name, *size),
                content_id: None,
            })
            .collect();
        if !m.categories.is_empty() {
            b.extra_headers.push(("Keywords".into(), m.categories.join(", ")));
        }
        if m.draft {
            b.extra_headers.push(("X-Unsent".into(), "1".into()));
        }
        let raw = b.build();
        ids.push(message_id);
        let mut item = match mailcraft_mime::parse(&raw) {
            Ok(parsed) => MailItem::from_message(&parsed, &raw, now),
            Err(_) => MailItem { subject: m.subject.to_owned(), received: date, ..Default::default() },
        };
        item.folder = match m.place {
            Place::Role(r) => role(&mb, r),
            Place::Projects => projects,
            Place::Receipts => receipts,
        };
        item.received = date;
        item.read = m.read || m.place == Place::Role(FolderRole::SentItems) || m.draft;
        item.is_draft = m.draft;
        if m.draft {
            item.sent = None;
        }
        item.focus = if m.other { FocusClass::Other } else { FocusClass::Focused };
        item.conversation = Some(topic_key(&item.subject));
        item.flag = match m.flag {
            Flag::None => FlagState::None,
            Flag::FollowUp(label, due_days) => FlagState::Flagged {
                label: label.to_owned(),
                start: Some(DateTime::from_unix(day_start, 0)),
                due: Some(DateTime::from_unix(day_start.saturating_add(due_days.saturating_mul(86_400)), 0)),
                reminder: None,
            },
            Flag::Done => {
                FlagState::Complete { completed: Some(DateTime::from_unix(t.saturating_add(3_600), 0)) }
            }
        };
        item.response = match m.response {
            Resp::None => ResponseState::None,
            Resp::Replied => ResponseState::Replied(DateTime::from_unix(t.saturating_add(2_820), 0)),
            Resp::Forwarded => ResponseState::Forwarded(DateTime::from_unix(t.saturating_add(1_200), 0)),
        };
        mb.add_mail(item);
    }

    add_calendar(&mut mb, account, day_start);
    add_people(&mut mb, account);
    add_tasks(&mut mb, account, day_start);
    add_notes(&mut mb, account, day_start);
    mb
}

fn add_calendar(mb: &mut Mailbox, account: AccountId, day_start: i64) {
    let folder = mb.role_folder(account, FolderRole::Calendar).unwrap_or_default();
    let at = |days: i64, hour: i64, minute: i64| {
        DateTime::from_unix(
            day_start
                .saturating_add(days.saturating_mul(86_400))
                .saturating_add(hour.saturating_mul(3600))
                .saturating_add(minute.saturating_mul(60)),
            0,
        )
    };
    let attendee = |p: Person| Attendee { mailbox: mailbox(p), ..Default::default() };
    let appts = [
        Appointment {
            uid: "sample-standup@example.net".into(),
            subject: "Team stand-up".into(),
            location: "Room 2B".into(),
            start: at(0, 9, 0),
            end: at(0, 9, 15),
            reminder_min: Some(5),
            recurrence: Some(Recurrence {
                rrule: "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into(),
                exdates: Vec::new(),
            }),
            organizer: Some(mailbox(ME)),
            attendees: vec![attendee(ALICE), attendee(BOB), attendee(DEV)],
            is_meeting: true,
            ..Default::default()
        },
        Appointment {
            uid: "sample-kickoff@example.net".into(),
            subject: "Website refresh kickoff".into(),
            location: "Small meeting room".into(),
            start: at(4, 10, 0),
            end: at(4, 11, 0),
            reminder_min: Some(15),
            organizer: Some(mailbox(ALICE)),
            attendees: vec![attendee(ME), attendee(BOB)],
            is_meeting: true,
            categories: vec!["Blue category".into()],
            body: "Agenda: goals, timeline, roles.".into(),
            ..Default::default()
        },
        Appointment {
            uid: "sample-lunch@example.net".into(),
            subject: "Lunch with Frank".into(),
            location: "Noodle place".into(),
            start: at(0, 12, 0),
            end: at(0, 13, 0),
            show_as: BusyStatus::Tentative,
            reminder_min: Some(15),
            ..Default::default()
        },
        Appointment {
            uid: "sample-dentist@example.net".into(),
            subject: "Dentist".into(),
            start: at(6, 8, 30),
            end: at(6, 9, 30),
            private: true,
            show_as: BusyStatus::OutOfOffice,
            reminder_min: Some(60),
            ..Default::default()
        },
        Appointment {
            uid: "sample-holiday@example.net".into(),
            subject: "Company holiday".into(),
            start: at(10, 0, 0),
            end: at(11, 0, 0),
            all_day: true,
            show_as: BusyStatus::Free,
            ..Default::default()
        },
    ];
    for mut a in appts {
        let id = mailcraft_model::ItemId(mb.alloc_id());
        a.id = id;
        a.folder = folder;
        mb.appointments.insert(id, a);
    }
}

fn add_people(mb: &mut Mailbox, account: AccountId) {
    let folder = mb.role_folder(account, FolderRole::Contacts).unwrap_or_default();
    let people = [
        ("Alice", "Rivera", ALICE.1, "Example Co.", "Project Manager", "+1 555 0101"),
        ("Bob", "Chen", BOB.1, "Example Org", "Data Analyst", "+1 555 0102"),
        ("Carol", "Okafor", CAROL.1, "Example Co.", "Facilities Lead", "+1 555 0103"),
        ("Dev", "Patel", DEV.1, "Example Net", "Software Engineer", "+1 555 0104"),
        ("Erin", "Walsh", ERIN.1, "Example Org", "Payroll Specialist", "+1 555 0105"),
        ("Grace", "Kim", GRACE.1, "Example Org", "Designer", "+1 555 0106"),
    ];
    for (first, last, email, company, job, phone) in people {
        let id = mailcraft_model::ItemId(mb.alloc_id());
        mb.contacts.insert(
            id,
            Contact {
                id,
                folder,
                first_name: first.into(),
                last_name: last.into(),
                file_as: format!("{last}, {first}"),
                company: company.into(),
                job_title: job.into(),
                emails: vec![email.into()],
                phones: vec![("Business".into(), phone.into())],
                ..Default::default()
            },
        );
    }
}

fn add_tasks(mb: &mut Mailbox, account: AccountId, day_start: i64) {
    let folder = mb.role_folder(account, FolderRole::Tasks).unwrap_or_default();
    let day = |d: i64| Some(DateTime::from_unix(day_start.saturating_add(d.saturating_mul(86_400)), 0));
    let tasks = [
        Task {
            subject: "Review quarterly report".into(),
            due: day(3),
            start: day(0),
            priority: TaskPriority::High,
            status: TaskStatus::InProgress,
            percent_complete: 25,
            ..Default::default()
        },
        Task {
            subject: "Book meeting room for kickoff".into(),
            due: day(-8),
            status: TaskStatus::Completed,
            percent_complete: 100,
            completed: day(-9),
            ..Default::default()
        },
        Task { subject: "Submit benefits forms".into(), due: day(14), ..Default::default() },
    ];
    for mut t in tasks {
        let id = mailcraft_model::ItemId(mb.alloc_id());
        t.id = id;
        t.folder = folder;
        mb.tasks.insert(id, t);
    }
}

fn add_notes(mb: &mut Mailbox, account: AccountId, day_start: i64) {
    let folder: FolderId = mb.role_folder(account, FolderRole::Notes).unwrap_or_default();
    let notes = [
        ("Wi-Fi for guests\nNetwork: Example-Guest\nAsk reception for today's code.", NoteColor::Yellow, -20),
        ("Retro ideas\n- fewer meetings\n- shared glossary", NoteColor::Blue, -3),
    ];
    for (text, color, d) in notes {
        let id = mailcraft_model::ItemId(mb.alloc_id());
        let d: i64 = d;
        let when = DateTime::from_unix(
            day_start.saturating_add(d.saturating_mul(86_400)).saturating_add(10 * 3600),
            0,
        );
        mb.notes.insert(
            id,
            Note {
                id,
                folder,
                text: text.into(),
                color,
                created: when,
                modified: when,
                ..Default::default()
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_is_valid_and_deterministic() {
        let now = sample_now();
        let a = sample_mailbox(now);
        let b = sample_mailbox(now);
        assert_eq!(a, b);
        assert!(a.validate().is_empty(), "{:?}", a.validate());
        assert!((36..=45).contains(&a.mail.len()), "{}", a.mail.len());
        assert!(
            !a.appointments.is_empty()
                && !a.contacts.is_empty()
                && !a.tasks.is_empty()
                && !a.notes.is_empty()
        );
    }

    #[test]
    fn sample_covers_the_interesting_states() {
        let mb = sample_mailbox(sample_now());
        let mail: Vec<&MailItem> = mb.mail.values().collect();
        assert!(mail.iter().any(|m| !m.read));
        assert!(mail.iter().any(|m| m.is_flagged()));
        assert!(mail.iter().any(|m| matches!(m.flag, FlagState::Complete { .. })));
        assert!(mail.iter().any(|m| m.importance == mailcraft_model::Importance::High));
        assert!(mail.iter().any(|m| m.importance == mailcraft_model::Importance::Low));
        assert!(mail.iter().any(|m| !m.categories.is_empty()));
        assert!(mail.iter().any(|m| m.has_attachments()));
        assert!(mail.iter().any(|m| m.body.html.is_some()));
        assert!(mail.iter().any(|m| !m.in_reply_to.is_empty()));
        assert!(mail.iter().any(|m| m.is_draft));
        assert!(mail.iter().any(|m| m.focus == FocusClass::Other));
        assert!(mail.iter().all(|m| m.received.unix <= sample_now().unix));
        assert!(mail.iter().all(|m| m.raw.is_some()));
        let addrs: Vec<&str> = mail
            .iter()
            .flat_map(|m| m.from.iter().chain(m.to.iter()).chain(m.cc.iter()))
            .map(|a| a.addr.as_str())
            .collect();
        assert!(addrs.iter().all(|a| a.ends_with("@example.com")
            || a.ends_with("@example.org")
            || a.ends_with("@example.net")));
    }

    #[test]
    fn sample_session_selects_inbox() {
        let s = sample_session();
        let inbox = s.mailbox.default_account().and_then(|a| s.mailbox.role_folder(a.id, FolderRole::Inbox));
        assert_eq!(Some(s.selection.folder), inbox);
        assert_eq!(s.me, vec![SAMPLE_USER_ADDR.to_owned()]);
    }
}
