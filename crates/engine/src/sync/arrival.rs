//! New mail: what happens to messages once send/receive has filed them in an Inbox, in this order.
//!
//! 1. The Junk Email filter ([`crate::junk::on_new_mail`]); what it moves or deletes goes no further. Voting
//!    responses to my messages are then recorded on the message they answer
//!    ([`crate::cmd::compose::record_vote`]), so the tally keeps them whatever happens to the responses.
//! 2. Focused Inbox sender overrides: mail from a sender marked Always Move to Other is classified Other
//!    ([`crate::cmd::mail::apply_focus_overrides`]).
//! 3. The rules for received messages ([`crate::cmd::rules::exec::on_new_mail`]).
//! 4. Scheduling messages, over what is still outside Deleted Items and Junk Email:
//!    - meeting requests and cancellations go to File › Options › Calendar › Automatic accept or decline
//!      (`calendar.autoRespond`, which does nothing while those options are off), whatever module is
//!      showing;
//!    - requests that are still unanswered go on the calendar as Tentative ([`calendar::place_tentative`]:
//!      the account's calendar, the default reminder) while Options › Mail › Tracking › "Automatically
//!      process meeting requests and responses to meeting requests and polls" is on; answering the request
//!      later updates that copy, declining removes it;
//!    - meeting responses update the meeting's tracking (`calendar.processReply`) while Options › Mail ›
//!      Tracking › "Automatically process meeting requests and responses to meeting requests and polls" is
//!      on;
//!    - task responses and status reports update the assigned task ([`crate::cmd::tasks::process_report`]).
//! 5. Tracking (Options › Mail › Tracking): with "Update tracking information, and then delete responses that
//!    don't contain comments" on, processed meeting and task responses and voting responses to my messages
//!    whose text adds nothing go to Deleted Items (the tally is read from the responses, which Deleted Items
//!    still holds); with "After updating tracking information, move receipt to" on, read receipts and
//!    delivery receipts (RFC 8098 / RFC 3464 reports) about my messages move to that folder. Non-delivery
//!    and delay reports stay where they are.
//!
//! Commands run here are automatic actions ([`Session::run_automatic`]): they record no undo step, and the
//! caller (the send/receive results being applied) journals the mailbox and queues the server changes.

use std::collections::BTreeSet;

use serde_json::json;

use crate::Session;
use crate::cmd::calendar;
use mailcraft_formats::ics::itip::Method;
use mailcraft_model::{AccountId, FolderId, FolderKind, FolderRole, ItemId, MailItem};

/// Most new messages one pass processes beyond the junk filter and the rules (a first download can bring
/// thousands; scheduling and receipts only look at the newest).
pub const MAX_PROCESSED: usize = 5_000;

/// What happened to a batch of new mail.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArrivalReport {
    /// Moved to Junk Email or deleted by the junk filter.
    pub junk: Vec<ItemId>,
    /// Classified Other by a sender override.
    pub focused_other: Vec<ItemId>,
    /// Items that matched a receive rule.
    pub rules_matched: usize,
    /// Meeting requests and cancellations answered automatically (accepted, declined or removed).
    pub meetings_answered: Vec<ItemId>,
    /// Meeting responses recorded on the organizer's meeting.
    pub meeting_responses: Vec<ItemId>,
    /// Task responses and status reports applied to the assigned task.
    pub task_reports: Vec<ItemId>,
    /// Responses without comments sent to Deleted Items.
    pub responses_deleted: Vec<ItemId>,
    /// Read and delivery receipts moved to the receipts folder.
    pub receipts_moved: Vec<ItemId>,
    /// Voting responses recorded on the message of mine they answer.
    pub votes_recorded: Vec<ItemId>,
    /// Meeting requests put on the calendar as Tentative until they are answered.
    pub meetings_placed: Vec<ItemId>,
}

/// Processes newly arrived mail `ids` (see the module docs).
pub fn on_new_mail(s: &mut Session, ids: &[ItemId]) -> ArrivalReport {
    let mut report = ArrivalReport::default();
    let mut seen = BTreeSet::new();
    let ids: Vec<ItemId> =
        ids.iter().copied().filter(|i| s.mailbox.mail.contains_key(i) && seen.insert(*i)).collect();
    if ids.is_empty() {
        return report;
    }
    let junk = crate::junk::on_new_mail(s, &ids);
    report.junk = junk.moved.iter().chain(junk.deleted.iter()).copied().collect();
    let gone: BTreeSet<ItemId> = report.junk.iter().copied().collect();
    let ids: Vec<ItemId> = ids.into_iter().filter(|i| !gone.contains(i)).collect();
    // Votes are recorded before anything files the responses away (rules, Deleted Items).
    report.votes_recorded = record_votes(s, &ids);
    report.focused_other = crate::cmd::mail::apply_focus_overrides(s, &ids);
    report.rules_matched = crate::cmd::rules::exec::on_new_mail(s, &ids).matched;
    // What the rules left outside Deleted Items and Junk Email, newest last.
    let mut ids: Vec<ItemId> = ids.into_iter().filter(|i| still_in_mail(s, *i)).collect();
    if ids.len() > MAX_PROCESSED {
        ids.sort_by_key(|i| (s.mailbox.mail.get(i).map_or(0, |m| m.received.unix), *i));
        let excess = ids.len().saturating_sub(MAX_PROCESSED);
        ids.drain(..excess);
    }
    scheduling(s, &ids, &mut report);
    tracking(s, &ids, &mut report);
    // What is left (unread, outside Deleted Items and Junk Email) lights the new-mail envelope.
    crate::new_mail::note_arrivals(s, &ids);
    report
}

/// Voting responses among `ids` that answer a message of mine: each is recorded on that message
/// ([`crate::cmd::compose::record_vote`]), so the tally keeps it after the response is deleted for good.
fn record_votes(s: &mut Session, ids: &[ItemId]) -> Vec<ItemId> {
    let replies: Vec<ItemId> = ids
        .iter()
        .copied()
        .filter(|i| s.mailbox.mail.get(i).is_some_and(|m| !m.in_reply_to.is_empty()))
        .take(MAX_PROCESSED)
        .collect();
    if replies.is_empty() {
        return Vec::new();
    }
    let mine = my_message_ids(s);
    if mine.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for id in replies {
        let to_mine =
            s.mailbox.mail.get(&id).is_some_and(|m| m.in_reply_to.iter().any(|r| mine.contains(&bare_id(r))));
        if to_mine && crate::cmd::compose::record_vote(s, id).is_some() {
            out.push(id);
        }
    }
    out
}

/// True when `id` still exists in a mail folder outside Deleted Items and Junk Email.
fn still_in_mail(s: &Session, id: ItemId) -> bool {
    let Some(m) = s.mailbox.mail.get(&id) else { return false };
    let Some(f) = s.mailbox.folder(m.folder) else { return false };
    if f.kind != FolderKind::Mail {
        return false;
    }
    let Some(account) = s.mailbox.folder_account(m.folder) else { return true };
    ![FolderRole::DeletedItems, FolderRole::JunkEmail].iter().any(|r| {
        s.mailbox.role_folder(account, *r).is_some_and(|root| s.mailbox.is_in_subtree(m.folder, root))
    })
}

/// Meeting requests, cancellations and responses, and task responses (step 4).
fn scheduling(s: &mut Session, ids: &[ItemId], report: &mut ArrivalReport) {
    let auto_process = s.settings.bool("mail.tracking.autoProcessMeetings");
    let delete_plain = s.settings.bool("mail.tracking.deleteWithoutComments");
    let mut requests = Vec::new();
    let mut replies = Vec::new();
    let mut task_replies = Vec::new();
    for id in ids {
        // Cheap test first: only messages carrying calendar data are parsed.
        let carries_calendar = s.mailbox.mail.get(id).is_some_and(|m| {
            m.attachments.iter().any(|a| {
                a.mime_type.to_ascii_lowercase().starts_with("text/calendar")
                    || a.filename.trim().to_ascii_lowercase().ends_with(".ics")
            })
        });
        if !carries_calendar {
            continue;
        }
        if let Some(msg) = calendar::read_itip(s, *id) {
            match msg.method {
                Method::Request | Method::Add | Method::Cancel => requests.push(*id),
                Method::Reply => replies.push(*id),
                _ => {}
            }
        } else if crate::cmd::tasks::read_task_message(s, *id).is_some_and(|t| t.method == "REPLY") {
            task_replies.push(*id);
        }
    }
    // Automatic accept or decline (the command checks the Calendar options itself).
    if calendar::auto_respond_on(s) {
        for chunk in requests.chunks(calendar::MAX_AUTO_SCAN) {
            let ids: Vec<u64> = chunk.iter().map(|i| i.0).collect();
            match s.run_automatic("calendar.autoRespond", &json!({ "ids": ids })) {
                Ok(v) => {
                    let handled = v.get("handled").and_then(|h| h.as_array()).cloned().unwrap_or_default();
                    report.meetings_answered.extend(
                        handled
                            .iter()
                            .filter(|h| h.get("error").is_none())
                            .filter_map(|h| h.get("id").and_then(|i| i.as_u64()))
                            .map(ItemId),
                    );
                }
                Err(e) => log::warn!("automatic meeting responses: {e}"),
            }
        }
    }
    // Requests still unanswered go on the calendar as Tentative until the user answers them.
    if auto_process {
        let answered: BTreeSet<ItemId> = report.meetings_answered.iter().copied().collect();
        for id in requests.iter().filter(|i| !answered.contains(i)) {
            let Some(msg) = calendar::read_itip(s, *id) else { continue };
            match calendar::place_tentative(s, *id, &msg) {
                Ok(Some(_)) => report.meetings_placed.push(*id),
                Ok(None) => {}
                Err(e) => log::debug!("meeting request {}: {e}", id.0),
            }
        }
    }
    let mut plain = Vec::new();
    if auto_process {
        for id in replies {
            match s.run_automatic("calendar.processReply", &json!({ "id": id.0 })) {
                Ok(_) => {
                    report.meeting_responses.push(id);
                    plain.push(id);
                }
                // Not a meeting of mine (or no longer on the calendar): left for the user.
                Err(e) => log::debug!("meeting response {}: {e}", id.0),
            }
        }
    }
    for id in task_replies {
        match crate::cmd::tasks::process_report(s, id) {
            Ok(_) => {
                report.task_reports.push(id);
                plain.push(id);
            }
            Err(e) => log::debug!("task response {}: {e}", id.0),
        }
    }
    if delete_plain {
        let gone: Vec<ItemId> =
            plain.into_iter().filter(|id| s.mailbox.mail.get(id).is_some_and(|m| !has_comments(m))).collect();
        delete(s, &gone, report);
    }
}

/// Voting responses and receipts (step 5).
fn tracking(s: &mut Session, ids: &[ItemId], report: &mut ArrivalReport) {
    let delete_plain = s.settings.bool("mail.tracking.deleteWithoutComments");
    let move_receipts = s.settings.bool("mail.tracking.moveReceipts");
    if !delete_plain && !move_receipts {
        return;
    }
    let mine = my_message_ids(s);
    if mine.is_empty() {
        return;
    }
    let refers_to_mine =
        |m: &MailItem| m.in_reply_to.iter().chain(m.references.iter()).any(|r| mine.contains(&bare_id(r)));
    let mut votes = Vec::new();
    let mut receipts = Vec::new();
    for id in ids {
        let Some(m) = s.mailbox.mail.get(id) else { continue };
        let Some(raw) = crate::cmd::compose::stored_raw(s, m) else { continue };
        if delete_plain
            && refers_to_mine(m)
            && crate::cmd::compose::raw_headers(&raw).text(crate::cmd::compose::VOTE_HEADER).is_some()
            && !has_comments(m)
        {
            votes.push(*id);
            continue;
        }
        if move_receipts
            && let Some(original) = receipt_about(&raw)
            && (refers_to_mine(m) || original.as_deref().is_some_and(|o| mine.contains(&bare_id(o))))
        {
            receipts.push(*id);
        }
    }
    delete(s, &votes, report);
    if receipts.is_empty() {
        return;
    }
    let target = s.settings.text("mail.tracking.moveReceiptsTo");
    for id in receipts {
        let account = s.mailbox.mail.get(&id).and_then(|m| s.mailbox.folder_account(m.folder));
        let Some(dest) = receipts_folder(s, account, &target) else {
            log::warn!("the receipts folder \"{target}\" was not found; receipt {} stays", id.0);
            continue;
        };
        if let Ok(moved) = s.mailbox.move_items(&[id], dest) {
            report.receipts_moved.extend(moved);
        }
    }
}

/// Moves `ids` to Deleted Items (removed outright in an account without one).
fn delete(s: &mut Session, ids: &[ItemId], report: &mut ArrivalReport) {
    if ids.is_empty() {
        return;
    }
    match s.mailbox.delete_items(ids) {
        Ok(gone) => report.responses_deleted.extend(gone.iter().map(|m| m.id)),
        Err(e) => log::warn!("deleting answered responses: {e}"),
    }
}

/// Whether a response adds text of its own (the quoted original, from its "From:" header block or "> "
/// lines on, does not count).
fn has_comments(m: &MailItem) -> bool {
    !crate::settings::new_text_of_reply(&m.body_text()).trim().is_empty()
}

fn bare_id(v: &str) -> String {
    v.trim().trim_start_matches('<').trim_end_matches('>').trim().to_ascii_lowercase()
}

/// Message-IDs (bare, lower-case) of the messages I sent that are still in the mailbox.
fn my_message_ids(s: &Session) -> BTreeSet<String> {
    let me = crate::cmd::mail::my_addresses(s);
    s.mailbox
        .mail
        .values()
        .filter(|m| m.from.as_ref().is_some_and(|f| me.contains(&f.addr.trim().to_lowercase())))
        .filter_map(|m| m.message_id.as_deref().map(bare_id))
        .filter(|i| !i.is_empty())
        .collect()
}

/// When `raw` is a read receipt (RFC 8098 disposition notification) or a delivery receipt (RFC 3464 report
/// whose recipients were all delivered, relayed or expanded): `Some(the original's Message-ID if the report
/// gives it)`. `None` for other mail, non-delivery reports and delays.
fn receipt_about(raw: &[u8]) -> Option<Option<String>> {
    let head = crate::cmd::compose::raw_headers(raw);
    let ctype = head.text("Content-Type")?.to_ascii_lowercase();
    if !ctype.trim_start().starts_with("multipart/report") {
        return None;
    }
    let msg = mailcraft_mime::parse(raw).ok()?;
    let parts = msg.parts();
    // The original's headers returned with the report (RFC 3464 RET=HDRS / a message/rfc822 part).
    let returned = parts.iter().find_map(|p| match (&p.body, p.mime_type()) {
        (mailcraft_mime::PartBody::Message(m), _) => m.message_id(),
        (_, t) if t.eq_ignore_ascii_case("text/rfc822-headers") => {
            p.text().and_then(|t| crate::cmd::compose::raw_headers(t.as_bytes()).text("Message-ID"))
        }
        _ => None,
    });
    for p in &parts {
        let kind = p.mime_type().to_ascii_lowercase();
        let read = kind == "message/disposition-notification";
        if !read && kind != "message/delivery-status" {
            continue;
        }
        let text = p.text().unwrap_or_default();
        let mut original = None;
        let mut actions = 0usize;
        for line in text.lines().take(2_000) {
            let Some((name, value)) = line.split_once(':') else { continue };
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_ascii_lowercase();
            match name.as_str() {
                "original-message-id" if original.is_none() => original = Some(value),
                "action" if !read => {
                    if !matches!(value.as_str(), "delivered" | "relayed" | "expanded") {
                        return None;
                    }
                    actions = actions.saturating_add(1);
                }
                _ => {}
            }
        }
        return (read || actions > 0).then_some(original.or_else(|| returned.clone()));
    }
    None
}

/// The folder receipts move to: the setting's folder path, looked up in the receipt's account first ("Deleted
/// Items", or nothing, is that account's Deleted Items whatever the server calls it).
fn receipts_folder(s: &Session, account: Option<AccountId>, setting: &str) -> Option<FolderId> {
    let want = setting.trim().trim_matches('\\').trim();
    let deleted = account.and_then(|a| s.mailbox.role_folder(a, FolderRole::DeletedItems));
    if want.is_empty() || want.eq_ignore_ascii_case("Deleted Items") {
        return deleted;
    }
    let lower = want.to_lowercase();
    let is_mail = |f: FolderId| s.mailbox.folder(f).is_some_and(|f| f.kind == FolderKind::Mail);
    account
        .and_then(|a| {
            s.mailbox
                .folders
                .values()
                .find(|f| f.account == a && s.mailbox.folder_path(f.id).to_lowercase() == lower)
                .map(|f| f.id)
        })
        .filter(|f| is_mail(*f))
        .or_else(|| crate::cmd::folder::resolve_folder(s, &json!(want)).filter(|f| is_mail(*f)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::calendar::tests::{deliver, invite, person, role, session};
    use mailcraft_model::FocusClass;
    use serde_json::Value;

    fn set(s: &mut Session, key: &str, v: Value) {
        s.settings.set(key, &v).unwrap_or_else(|e| panic!("{key}: {e}"));
    }

    /// A raw message from `from` to me, filed in `s`'s Inbox like downloaded mail.
    fn arrive(s: &mut Session, raw: &str) -> ItemId {
        let raw = raw.replace('\n', "\r\n");
        let msg = mailcraft_mime::parse(raw.as_bytes()).unwrap();
        let mut item = MailItem::from_message(&msg, raw.as_bytes(), s.now());
        item.folder = role(s, FolderRole::Inbox);
        s.mailbox.add_mail(item)
    }

    /// A message I sent (in Sent Items) with Message-ID `<id>`.
    fn sent(s: &mut Session, id: &str, extra: &str) -> ItemId {
        let raw = format!(
            "From: Me Myself <me@example.com>\nTo: ann@example.com\nSubject: Budget\n\
             Message-ID: <{id}>\n{extra}Content-Type: text/plain\n\nPlease vote.\n"
        );
        let item = arrive(s, &raw);
        let folder = role(s, FolderRole::SentItems);
        s.mailbox.move_items(&[item], folder).unwrap();
        item
    }

    #[test]
    fn junk_then_focus_overrides_then_rules() {
        let mut s = session();
        s.run("mail.junk", &json!({"action": "add", "list": "blockedSenders", "entry": "spam@bad.example"}))
            .unwrap();
        set(&mut s, "mail.focused.otherSenders", json!(["news@example.org"]));
        s.run(
            "rules.create",
            &json!({"name": "Hi", "conditions": [{"SubjectContains": ["hello"]}],
                    "actions": [{"AssignCategory": "Red category"}]}),
        )
        .unwrap();
        let spam = arrive(&mut s, "From: spam@bad.example\nSubject: hello\n\nx\n");
        let news = arrive(&mut s, "From: News <News@Example.org>\nSubject: hello weekly\n\nx\n");
        let ann = arrive(&mut s, "From: ann@example.com\nSubject: lunch\n\nx\n");
        let r = on_new_mail(&mut s, &[spam, news, ann, news, ItemId(u64::MAX)]);
        assert_eq!(r.junk, vec![spam]);
        assert_eq!(r.focused_other, vec![news]);
        assert_eq!(r.rules_matched, 1);
        assert_eq!(s.mailbox.mail[&news].focus, FocusClass::Other);
        assert_eq!(s.mailbox.mail[&news].categories, vec!["Red category".to_string()]);
        assert_eq!(s.mailbox.mail[&ann].focus, FocusClass::Focused);
        assert!(s.mailbox.mail[&spam].categories.is_empty(), "rules skip junk");
        assert_eq!(on_new_mail(&mut s, &[]), ArrivalReport::default());
    }

    #[test]
    fn meeting_requests_are_answered_whatever_the_view() {
        let mut org = session();
        let (_, cid) = invite(&mut org);
        let mut ann = person("ann@example.com", "Ann Lee");
        // Options off: nothing is answered.
        let mail = deliver(&org, cid, &mut ann);
        assert!(on_new_mail(&mut ann, &[mail]).meetings_answered.is_empty());
        let info = ann.run("calendar.meetingInfo", &json!({"id": mail.0})).unwrap();
        assert_ne!(info["myResponse"], "accepted");
        // Automatic accept on: the next request is accepted on arrival, with no undo step.
        set(&mut ann, "calendar.autoAccept", json!(true));
        let undo = ann.undo.undo.len();
        let mail = deliver(&org, cid, &mut ann);
        let r = on_new_mail(&mut ann, &[mail]);
        assert_eq!(r.meetings_answered, vec![mail]);
        let info = ann.run("calendar.meetingInfo", &json!({"id": mail.0})).unwrap();
        assert_eq!(info["myResponse"], "accepted", "{info}");
        assert_eq!(ann.undo.undo.len(), undo, "automatic, not undoable");
    }

    #[test]
    fn meeting_responses_update_tracking_and_plain_ones_are_deleted() {
        let mut org = session();
        let (meeting, cid) = invite(&mut org);
        let mut ann = person("ann@example.com", "Ann Lee");
        let req = deliver(&org, cid, &mut ann);
        let accept = |ann: &mut Session, comment: &str| {
            let r = ann
                .run(
                    "calendar.respond",
                    &json!({"id": req.0, "response": "accept", "send": "edit", "comment": comment}),
                )
                .unwrap();
            r["compose"].as_u64().unwrap()
        };
        let tracked = |org: &mut Session| {
            org.run("calendar.tracking", &json!({"id": meeting.0})).unwrap()["accepted"] == 1
        };
        // Processing off: the response waits for the user.
        set(&mut org, "mail.tracking.autoProcessMeetings", json!(false));
        let plain = accept(&mut ann, "");
        let reply = deliver(&ann, plain, &mut org);
        assert!(on_new_mail(&mut org, &[reply]).meeting_responses.is_empty());
        assert!(!tracked(&mut org));
        // On (the default): tracked; with "delete responses without comments" a plain one goes to Deleted
        // Items and one with a comment stays.
        set(&mut org, "mail.tracking.autoProcessMeetings", json!(true));
        set(&mut org, "mail.tracking.deleteWithoutComments", json!(true));
        let reply = deliver(&ann, plain, &mut org);
        let commented = accept(&mut ann, "See you there");
        let noted = deliver(&ann, commented, &mut org);
        let r = on_new_mail(&mut org, &[reply, noted]);
        assert_eq!(r.meeting_responses, vec![reply, noted]);
        assert!(tracked(&mut org));
        assert_eq!(r.responses_deleted, vec![reply]);
        assert_eq!(org.mailbox.mail[&reply].folder, role(&org, FolderRole::DeletedItems));
        assert_eq!(org.mailbox.mail[&noted].folder, role(&org, FolderRole::Inbox));
    }

    #[test]
    fn task_responses_update_the_assigned_task() {
        let mut org = session();
        let r = org
            .run(
                "tasks.assign",
                &json!({"to": "Ann Lee <ann@example.com>", "subject": "Budget", "due": "2026-10-20",
                        "send": false}),
            )
            .unwrap();
        let copy = ItemId(r["task"].as_u64().unwrap());
        let mut ann = person("ann@example.com", "Ann Lee");
        let req = deliver(&org, r["compose"].as_u64().unwrap(), &mut ann);
        let r = ann.run("tasks.acceptRequest", &json!({"id": req.0, "send": "edit"})).unwrap();
        let answer = deliver(&ann, r["compose"].as_u64().unwrap(), &mut org);
        let r = on_new_mail(&mut org, &[answer]);
        assert_eq!(r.task_reports, vec![answer]);
        assert_eq!(org.mailbox.tasks[&copy].request_state, mailcraft_model::task::TaskRequestState::Accepted);
        // The request itself (no REPLY) is left for its reader.
        assert!(on_new_mail(&mut ann, &[req]).task_reports.is_empty());
    }

    #[test]
    fn plain_votes_go_to_deleted_items_when_asked() {
        let mut s = session();
        sent(&mut s, "vote-1@example.com", "X-MailCraft-Voting: Approve;Reject\n");
        let plain = "From: ann@example.com\nSubject: Approve: Budget\nIn-Reply-To: <vote-1@example.com>\n\
                     X-MailCraft-Vote: Approve\n\n";
        let a = arrive(&mut s, plain);
        assert!(on_new_mail(&mut s, &[a]).responses_deleted.is_empty(), "option off");
        set(&mut s, "mail.tracking.deleteWithoutComments", json!(true));
        let b = arrive(&mut s, plain);
        let c = arrive(
            &mut s,
            "From: bo@example.org\nSubject: Reject: Budget\nIn-Reply-To: <vote-1@example.com>\n\
             X-MailCraft-Vote: Reject\n\nToo high.\n",
        );
        // A vote about someone else's message is not mine to file away.
        let d = arrive(
            &mut s,
            "From: cy@example.net\nSubject: Approve: Other\nIn-Reply-To: <other@example.net>\n\
             X-MailCraft-Vote: Approve\n\n",
        );
        let r = on_new_mail(&mut s, &[b, c, d]);
        assert_eq!(r.responses_deleted, vec![b]);
        assert_eq!(s.mailbox.mail[&b].folder, role(&s, FolderRole::DeletedItems));
        assert_eq!(s.mailbox.mail[&c].folder, role(&s, FolderRole::Inbox));
        assert_eq!(s.mailbox.mail[&d].folder, role(&s, FolderRole::Inbox));
    }

    fn mdn(about: &str) -> String {
        format!(
            "From: ann@example.com\nSubject: Read: Budget\nIn-Reply-To: <{about}>\nMIME-Version: 1.0\n\
             Content-Type: multipart/report; report-type=disposition-notification; boundary=\"b\"\n\n\
             --b\nContent-Type: text/plain\n\nRead.\n--b\nContent-Type: message/disposition-notification\n\n\
             Final-Recipient: rfc822;ann@example.com\nOriginal-Message-ID: <{about}>\n\
             Disposition: manual-action/MDN-sent-manually; displayed\n--b--\n"
        )
    }

    fn dsn(about: &str, action: &str) -> String {
        format!(
            "From: mailer-daemon@example.org\nSubject: Delivery report\nMIME-Version: 1.0\n\
             Content-Type: multipart/report; report-type=delivery-status; boundary=\"c\"\n\n\
             --c\nContent-Type: text/plain\n\nReport.\n--c\nContent-Type: message/delivery-status\n\n\
             Reporting-MTA: dns;mx.example.org\nOriginal-Envelope-Id: x\n\n\
             Final-Recipient: rfc822;ann@example.com\nAction: {action}\nStatus: 2.0.0\n\
             --c\nContent-Type: text/rfc822-headers\n\nMessage-ID: <{about}>\n--c--\n"
        )
    }

    #[test]
    fn receipts_move_to_the_chosen_folder() {
        let mut s = session();
        sent(&mut s, "r-1@example.com", "");
        let read = arrive(&mut s, &mdn("r-1@example.com"));
        assert!(on_new_mail(&mut s, &[read]).receipts_moved.is_empty(), "option off");
        set(&mut s, "mail.tracking.moveReceipts", json!(true));
        // Default target: Deleted Items.
        let read = arrive(&mut s, &mdn("r-1@example.com"));
        let other = arrive(&mut s, &mdn("not-mine@example.net"));
        let r = on_new_mail(&mut s, &[read, other]);
        assert_eq!(r.receipts_moved, vec![read]);
        assert_eq!(s.mailbox.mail[&read].folder, role(&s, FolderRole::DeletedItems));
        assert_eq!(s.mailbox.mail[&other].folder, role(&s, FolderRole::Inbox));
        // A folder of my own; a delivery receipt goes there, a failure report stays in the Inbox.
        let inbox = role(&s, FolderRole::Inbox);
        let receipts = s.mailbox.create_folder(inbox, "Receipts").unwrap();
        set(&mut s, "mail.tracking.moveReceiptsTo", json!("Inbox\\Receipts"));
        let delivered = arrive(&mut s, &dsn("r-1@example.com", "delivered"));
        let failed = arrive(&mut s, &dsn("r-1@example.com", "failed"));
        let r = on_new_mail(&mut s, &[delivered, failed]);
        assert_eq!(r.receipts_moved, vec![delivered]);
        assert_eq!(s.mailbox.mail[&delivered].folder, receipts);
        assert_eq!(s.mailbox.mail[&failed].folder, inbox);
        // A folder that does not exist: the receipt stays.
        set(&mut s, "mail.tracking.moveReceiptsTo", json!("Nowhere\\At all"));
        let lost = arrive(&mut s, &mdn("r-1@example.com"));
        assert!(on_new_mail(&mut s, &[lost]).receipts_moved.is_empty());
        assert_eq!(s.mailbox.mail[&lost].folder, inbox);
    }

    #[test]
    fn receipt_detection_rejects_junk() {
        assert_eq!(receipt_about(mdn("a@b").replace('\n', "\r\n").as_bytes()), Some(Some("<a@b>".into())));
        assert!(receipt_about(dsn("a@b", "delayed").as_bytes()).is_none());
        assert!(receipt_about(b"Subject: hi\r\n\r\nbody").is_none());
        assert!(receipt_about(b"Content-Type: multipart/report\r\n\r\n").is_none());
        assert!(receipt_about(&[0xff, 0xfe, b':', b'\n']).is_none());
        assert!(receipt_about(b"").is_none());
    }

    fn copies(s: &Session, subject: &str) -> Vec<ItemId> {
        s.mailbox.appointments.values().filter(|a| a.subject == subject).map(|a| a.id).collect()
    }

    #[test]
    fn meeting_requests_wait_on_the_calendar_as_tentative() {
        use mailcraft_model::BusyStatus;
        let mut org = session();
        let (_, cid) = invite(&mut org);
        let mut ann = person("ann@example.com", "Ann Lee");
        // Processing off: the request only waits in the Inbox.
        set(&mut ann, "mail.tracking.autoProcessMeetings", json!(false));
        let first = deliver(&org, cid, &mut ann);
        assert!(on_new_mail(&mut ann, &[first]).meetings_placed.is_empty());
        assert!(copies(&ann, "Design review").is_empty());
        // On (the default): it goes on the calendar as Tentative, unanswered, with the default reminder; the
        // message stays unread.
        set(&mut ann, "mail.tracking.autoProcessMeetings", json!(true));
        set(&mut ann, "calendar.reminderMinutes", json!(30));
        let req = deliver(&org, cid, &mut ann);
        let undo = ann.undo.undo.len();
        assert_eq!(on_new_mail(&mut ann, &[req]).meetings_placed, vec![req]);
        assert_eq!(ann.undo.undo.len(), undo, "automatic, not undoable");
        let [id] = copies(&ann, "Design review")[..] else { panic!("one copy") };
        let a = &ann.mailbox.appointments[&id];
        assert_eq!(a.show_as, BusyStatus::Tentative);
        assert_eq!(a.reminder_min, Some(30));
        assert_eq!(a.folder, role(&ann, FolderRole::Calendar));
        assert!(!ann.mailbox.mail[&req].read);
        let info = ann.run("calendar.meetingInfo", &json!({"id": req.0})).unwrap();
        assert_eq!((info["myResponse"].as_str(), info["appointment"].as_u64()), (Some("none"), Some(id.0)));
        // The same meeting again is not placed twice.
        assert!(on_new_mail(&mut ann, &[first]).meetings_placed.is_empty());
        assert_eq!(copies(&ann, "Design review").len(), 1);
        // Answering updates that copy: Accept shows the time as Busy.
        ann.run("calendar.respond", &json!({"id": req.0, "response": "accept", "send": "none"})).unwrap();
        assert_eq!(copies(&ann, "Design review"), vec![id]);
        assert_eq!(ann.mailbox.appointments[&id].show_as, BusyStatus::Busy);
        assert_eq!(ann.mailbox.appointments[&id].reminder_min, Some(30), "local reminder kept");
        // Declining removes it; without default reminders (Options › Calendar) the copy has none.
        let mut bob = person("bob@example.org", "Bob");
        set(&mut bob, "calendar.defaultReminders", json!(false));
        let req = deliver(&org, cid, &mut bob);
        assert_eq!(on_new_mail(&mut bob, &[req]).meetings_placed, vec![req]);
        let [b] = copies(&bob, "Design review")[..] else { panic!("one copy") };
        assert_eq!(bob.mailbox.appointments[&b].reminder_min, None);
        bob.run("calendar.respond", &json!({"id": req.0, "response": "decline", "send": "none"})).unwrap();
        assert!(
            copies(&bob, "Design review")
                .iter()
                .all(|i| bob.mailbox.appointments[i].folder != role(&bob, FolderRole::Calendar))
        );
        // Automatic accept answers it instead (no tentative step).
        let mut cy = person("cy@example.net", "Cy");
        set(&mut cy, "calendar.autoAccept", json!(true));
        let req = deliver(&org, cid, &mut cy);
        let r = on_new_mail(&mut cy, &[req]);
        assert_eq!((r.meetings_answered.clone(), r.meetings_placed.clone()), (vec![req], vec![]));
        // My own meeting request is not placed again.
        let mut me = person("me@example.com", "Me Myself");
        let mine = deliver(&org, cid, &mut me);
        assert!(on_new_mail(&mut me, &[mine]).meetings_placed.is_empty());
    }

    #[test]
    fn votes_keep_counting_after_deleted_items_is_emptied() {
        let mut s = session();
        let original = sent(&mut s, "vote-2@example.com", "X-MailCraft-Voting: Approve;Reject\n");
        set(&mut s, "mail.tracking.deleteWithoutComments", json!(true));
        let plain = |from: &str, choice: &str| {
            format!(
                "From: {from}\nSubject: {choice}: Budget\nIn-Reply-To: <vote-2@example.com>\n\
                 X-MailCraft-Vote: {choice}\n\n"
            )
        };
        let a = arrive(&mut s, &plain("ann@example.com", "Approve"));
        let b = arrive(&mut s, &plain("bo@example.org", "Reject"));
        let r = on_new_mail(&mut s, &[a, b]);
        assert_eq!(r.votes_recorded, vec![a, b]);
        assert_eq!(r.responses_deleted, vec![a, b]);
        let tally = |s: &Session| crate::cmd::compose::tracking(s, original).unwrap().tally;
        assert_eq!(tally(&s), vec![("Approve".to_owned(), 1), ("Reject".to_owned(), 1)]);
        // Deleted Items emptied: the responses are gone, the tally is not.
        s.run("folder.emptyDeleted", &json!({"confirm": true})).unwrap();
        assert!(!s.mailbox.mail.contains_key(&a) && !s.mailbox.mail.contains_key(&b));
        assert_eq!(tally(&s), vec![("Approve".to_owned(), 1), ("Reject".to_owned(), 1)]);
        assert_eq!(s.mailbox.mail[&original].votes.len(), 2);
        // A later vote from the same person replaces theirs; a vote from someone else's thread is not mine.
        let c = arrive(&mut s, &plain("ann@example.com", "Reject"));
        let d = arrive(
            &mut s,
            "From: cy@example.net\nSubject: Approve: X\nIn-Reply-To: <other@example.net>\nX-MailCraft-Vote: Approve\n\n",
        );
        assert_eq!(on_new_mail(&mut s, &[c, d]).votes_recorded, vec![c]);
        assert_eq!(tally(&s), vec![("Approve".to_owned(), 0), ("Reject".to_owned(), 2)]);
        // The stored record and the live response are one vote.
        assert_eq!(crate::cmd::compose::tracking(&s, original).unwrap().responses.len(), 2);
        // With "Automatically update original sent item with receipt information" off, none show.
        set(&mut s, "mail.tracking.updateSentItem", json!(false));
        assert_eq!(tally(&s), vec![("Approve".to_owned(), 0), ("Reject".to_owned(), 0)]);
    }
}
