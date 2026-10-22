//! Rules (plan/reference/rules.md): the quick Create Rule dialog and its Success box, Rules and Alerts (Email
//! Rules list, toolbar, Change Rule menu, Copy to other accounts, description pane, Manage Alerts tab), the
//! Rules Wizard (start from a template or a blank rule for received or sent messages, then condition /
//! action / exception checklists with underlined editable values), Run Rules Now, Rename Rule and Options
//! (`.mcrules` import/export).
//!
//! Also routes the Quick Steps dialogs (`quicksteps.rs`) and the New Item Alert window (`alerts.rs`).

use egui::{Context, RichText, Sense, Ui};
use serde_json::{Value, json};

use super::common::{
    self, Dlg, Out, btn, btn_en, child, folder_path, footer, muted, ok_cancel, primary, take_replies, window,
    with_state,
};
use super::folders::last_error;
use crate::App;
use mailcraft_model::{FolderId, Importance, Rule, RuleAction, RuleCondition, Sensitivity};

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "copyRule" => copy_rule(ctx, app, d),
        "createRule" => create_rule(ctx, app, d),
        "ruleCreated" => rule_created(ctx, app, d),
        "rulesAndAlerts" => rules_and_alerts(ctx, app, d),
        "rulesWizard" => wizard(ctx, app, d),
        "runRulesNow" => run_rules_now(ctx, app, d),
        "renameRule" => rename_rule(ctx, app, d),
        "rulesOptions" => rules_options(ctx, app, d),
        "rulePrompt" => rule_prompt(ctx, app, d),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------------------
// Catalog of conditions and actions

/// Kind of underlined value a checklist entry carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Val {
    None,
    People,
    Words,
    Account,
    Importance,
    Sensitivity,
    Category,
    Size,
    Dates,
    Folder,
    Text,
    Flag,
    /// A form display name (Message, Meeting Request, …).
    Form,
    /// A message class prefix.
    FormType,
    /// Minutes (defer delivery).
    Minutes,
    /// A reply template file (.eml).
    Template,
    /// A sound file.
    Sound,
    /// Reply text (several lines).
    Reply,
}

/// One checklist entry: text before the underlined value, the value placeholder, text after.
struct Def {
    before: &'static str,
    placeholder: &'static str,
    after: &'static str,
    val: Val,
}

const fn def(before: &'static str, placeholder: &'static str, after: &'static str, val: Val) -> Def {
    Def { before, placeholder, after, val }
}

const fn plain(text: &'static str) -> Def {
    Def { before: text, placeholder: "", after: "", val: Val::None }
}

const CONDS: [Def; 30] = [
    def("with ", "specific words", " in the subject", Val::Words),
    def("from ", "people or public group", "", Val::People),
    def("through the ", "specified", " account", Val::Account),
    plain("sent only to me"),
    plain("where my name is in the To box"),
    def("marked as ", "importance", "", Val::Importance),
    def("marked as ", "sensitivity", "", Val::Sensitivity),
    plain("flagged for any action"),
    plain("where my name is in the Cc box"),
    plain("where my name is in the To or Cc box"),
    plain("where my name is not in the To box"),
    def("sent to ", "people or public group", "", Val::People),
    def("with ", "specific words", " in the body", Val::Words),
    def("with ", "specific words", " in the subject or body", Val::Words),
    def("with ", "specific words", " in the message header", Val::Words),
    def("with ", "specific words", " in the recipient's address", Val::Words),
    def("with ", "specific words", " in the sender's address", Val::Words),
    def("assigned to ", "category", " category", Val::Category),
    plain("which has an attachment"),
    def("with a size ", "in a specific range", "", Val::Size),
    def("received ", "in a specific date span", "", Val::Dates),
    plain("which is an automatic reply"),
    plain("which is a meeting invitation or update"),
    def("uses the ", "form name", " form", Val::Form),
    def("of the ", "specific", " form type", Val::FormType),
    def("from RSS Feeds with ", "specified text", " in the title", Val::Words),
    plain("from any RSS Feed"),
    plain("suspected to be junk or from Junk Senders"),
    plain("sender is in my Contacts"),
    plain("on this computer only"),
];

/// Conditions that make sense only for received messages (hidden in a rule for sent messages).
const COND_INCOMING_ONLY: [usize; 11] = [3, 4, 8, 9, 10, 20, 21, 25, 26, 27, 28];

const ACTS: [Def; 25] = [
    def("display ", "a specific message", " in the New Item Alert window", Val::Text),
    def("move it to the ", "specified", " folder", Val::Folder),
    def("assign it to the ", "category", " category", Val::Category),
    plain("delete it"),
    plain("permanently delete it"),
    def("move a copy to the ", "specified", " folder", Val::Folder),
    def("forward it to ", "people or public group", "", Val::People),
    def("forward it to ", "people or public group", " as an attachment", Val::People),
    def("redirect it to ", "people or public group", "", Val::People),
    def("reply using ", "a specific template", "", Val::Template),
    def("flag message for ", "follow up at this time", "", Val::Flag),
    plain("clear the Message Flag"),
    plain("clear message's categories"),
    def("mark it as ", "importance", "", Val::Importance),
    plain("print it"),
    def("play ", "a sound", "", Val::Sound),
    plain("mark it as read"),
    plain("stop processing more rules"),
    plain("display a Desktop Alert"),
    def("defer delivery by ", "a number of", " minutes", Val::Minutes),
    def("Cc the message to ", "people or public group", "", Val::People),
    def("mark it as ", "sensitivity", "", Val::Sensitivity),
    plain("notify me when it is read"),
    plain("notify me when it is delivered"),
    def("have server reply using ", "a specific message", "", Val::Reply),
];

/// Actions for received messages only, and for sent messages only (hidden in the other kind of rule).
const ACT_INCOMING_ONLY: [usize; 13] = [0, 1, 3, 4, 6, 7, 8, 9, 14, 15, 16, 18, 24];
const ACT_OUTGOING_ONLY: [usize; 4] = [19, 20, 22, 23];

/// Whether catalog entry `i` of `sec` is offered in a rule for sent (`outgoing`) or received messages.
fn offered(sec: Sec, i: usize, outgoing: bool) -> bool {
    match sec {
        Sec::Act if outgoing => !ACT_INCOMING_ONLY.contains(&i),
        Sec::Act => !ACT_OUTGOING_ONLY.contains(&i),
        _ => !outgoing || !COND_INCOMING_ONLY.contains(&i),
    }
}

const IMPORTANCE: [&str; 3] = ["low", "normal", "high"];
const SENSITIVITY: [&str; 4] = ["normal", "personal", "private", "confidential"];
const FLAG_LABELS: [&str; 6] = ["Follow up", "Call", "For Your Information", "Read", "Reply", "Review"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Sec {
    Cond,
    Act,
    Exc,
}

/// Checklist entry state.
#[derive(Clone, Debug, Default)]
struct Slot {
    on: bool,
    text: String,
    choice: usize,
    n1: u64,
    n2: u64,
    d1: String,
    d2: String,
    folder: Option<u64>,
}

/// A rule being edited: one slot per catalog entry per section.
#[derive(Clone, Debug)]
struct Draft {
    conds: Vec<Slot>,
    acts: Vec<Slot>,
    excs: Vec<Slot>,
    /// "Apply rule on messages I send".
    outgoing: bool,
}

impl Default for Draft {
    fn default() -> Self {
        let mk = |n: usize| -> Vec<Slot> {
            (0..n).map(|_| Slot { choice: 2, n2: 1024, ..Default::default() }).collect()
        };
        let mut d =
            Draft { conds: mk(CONDS.len()), acts: mk(ACTS.len()), excs: mk(CONDS.len()), outgoing: false };
        // Condition values default to "high" importance / "private" sensitivity; actions to the first choice.
        for s in &mut d.acts {
            s.choice = 0;
        }
        if let Some(s) = d.acts.get_mut(13) {
            s.choice = 2;
        }
        if let Some(s) = d.acts.get_mut(19) {
            s.n1 = 5;
        }
        if let Some(s) = d.acts.get_mut(21) {
            s.choice = 2;
        }
        d
    }
}

fn defs(sec: Sec) -> &'static [Def] {
    match sec {
        Sec::Act => &ACTS,
        _ => &CONDS,
    }
}

impl Draft {
    fn slots(&self, sec: Sec) -> &Vec<Slot> {
        match sec {
            Sec::Cond => &self.conds,
            Sec::Act => &self.acts,
            Sec::Exc => &self.excs,
        }
    }
    fn slots_mut(&mut self, sec: Sec) -> &mut Vec<Slot> {
        match sec {
            Sec::Cond => &mut self.conds,
            Sec::Act => &mut self.acts,
            Sec::Exc => &mut self.excs,
        }
    }
    fn set(&mut self, sec: Sec, i: usize, f: impl FnOnce(&mut Slot)) {
        if let Some(s) = self.slots_mut(sec).get_mut(i) {
            s.on = true;
            f(s);
        }
    }

    fn from_rule(
        app: &App,
        conditions: &[RuleCondition],
        actions: &[RuleAction],
        exceptions: &[RuleCondition],
    ) -> Draft {
        let mut d = Draft::default();
        for (sec, list) in [(Sec::Cond, conditions), (Sec::Exc, exceptions)] {
            for c in list {
                d.load_cond(app, sec, c);
            }
        }
        for a in actions {
            d.load_act(a);
        }
        d
    }

    fn load_cond(&mut self, app: &App, sec: Sec, c: &RuleCondition) {
        let words = |l: &Vec<String>| l.join("; ");
        match c {
            RuleCondition::SubjectContains(l) => self.set(sec, 0, |s| s.text = words(l)),
            RuleCondition::From(l) => self.set(sec, 1, |s| s.text = words(l)),
            RuleCondition::ThroughAccount(a) => self.set(sec, 2, |s| s.text = a.clone()),
            RuleCondition::SentOnlyToMe => self.set(sec, 3, |_| {}),
            RuleCondition::MyNameInTo => self.set(sec, 4, |_| {}),
            RuleCondition::MarkedImportance(i) => self.set(sec, 5, |s| s.choice = imp_idx(*i)),
            RuleCondition::MarkedSensitivity(x) => self.set(sec, 6, |s| s.choice = sens_idx(*x)),
            RuleCondition::FlaggedForAction => self.set(sec, 7, |_| {}),
            RuleCondition::MyNameInCc => self.set(sec, 8, |_| {}),
            RuleCondition::MyNameInToOrCc => self.set(sec, 9, |_| {}),
            RuleCondition::MyNameNotInTo => self.set(sec, 10, |_| {}),
            RuleCondition::SentTo(l) => self.set(sec, 11, |s| s.text = words(l)),
            RuleCondition::BodyContains(l) => self.set(sec, 12, |s| s.text = words(l)),
            RuleCondition::SubjectOrBodyContains(l) => self.set(sec, 13, |s| s.text = words(l)),
            RuleCondition::HeaderContains(l) => self.set(sec, 14, |s| s.text = words(l)),
            RuleCondition::RecipientAddressContains(l) => self.set(sec, 15, |s| s.text = words(l)),
            RuleCondition::SenderAddressContains(l) => self.set(sec, 16, |s| s.text = words(l)),
            RuleCondition::AssignedToCategory(c) => self.set(sec, 17, |s| s.text = c.clone()),
            RuleCondition::HasAttachment => self.set(sec, 18, |_| {}),
            RuleCondition::SizeBetween(a, b) => self.set(sec, 19, |s| {
                s.n1 = *a;
                s.n2 = *b;
            }),
            RuleCondition::ReceivedBetween(a, b) => {
                let (d1, d2) = (common::date_parts(app, *a).0, common::date_parts(app, *b).0);
                self.set(sec, 20, |s| {
                    s.d1 = d1;
                    s.d2 = d2;
                });
            }
            RuleCondition::AutomaticReply => self.set(sec, 21, |_| {}),
            RuleCondition::MeetingInvitationOrUpdate => self.set(sec, 22, |_| {}),
            RuleCondition::UsesForm(f) => self.set(sec, 23, |s| s.text = f.clone()),
            RuleCondition::FormType(t) => self.set(sec, 24, |s| s.text = t.clone()),
            RuleCondition::FromRssFeedWithText(l) => self.set(sec, 25, |s| s.text = words(l)),
            RuleCondition::FromAnyRssFeed => self.set(sec, 26, |_| {}),
            RuleCondition::SuspectedJunk => self.set(sec, 27, |_| {}),
            RuleCondition::SenderInContacts => self.set(sec, 28, |_| {}),
            RuleCondition::OnThisComputerOnly => self.set(sec, 29, |_| {}),
            RuleCondition::AfterSending => {
                if sec == Sec::Cond {
                    self.outgoing = true;
                }
            }
        }
    }

    fn load_act(&mut self, a: &RuleAction) {
        let sec = Sec::Act;
        let words = |l: &Vec<String>| l.join("; ");
        match a {
            RuleAction::NewItemAlert(t) => self.set(sec, 0, |s| s.text = t.clone()),
            RuleAction::MoveTo(f) => self.set(sec, 1, |s| s.folder = Some(f.0)),
            RuleAction::AssignCategory(c) => self.set(sec, 2, |s| s.text = c.clone()),
            RuleAction::Delete => self.set(sec, 3, |_| {}),
            RuleAction::PermanentlyDelete => self.set(sec, 4, |_| {}),
            RuleAction::CopyTo(f) => self.set(sec, 5, |s| s.folder = Some(f.0)),
            RuleAction::ForwardTo(l) => self.set(sec, 6, |s| s.text = words(l)),
            RuleAction::ForwardAsAttachment(l) => self.set(sec, 7, |s| s.text = words(l)),
            RuleAction::RedirectTo(l) => self.set(sec, 8, |s| s.text = words(l)),
            RuleAction::ReplyWithTemplate(t) => self.set(sec, 9, |s| s.text = t.clone()),
            RuleAction::FlagForFollowUp(l, days) => self.set(sec, 10, |s| {
                s.text = l.clone();
                s.n1 = u64::from(*days);
            }),
            RuleAction::ClearFlag => self.set(sec, 11, |_| {}),
            RuleAction::ClearCategories => self.set(sec, 12, |_| {}),
            RuleAction::MarkImportance(i) => self.set(sec, 13, |s| s.choice = imp_idx(*i)),
            RuleAction::Print => self.set(sec, 14, |_| {}),
            RuleAction::PlaySound(p) => self.set(sec, 15, |s| s.text = p.clone()),
            RuleAction::MarkAsRead => self.set(sec, 16, |_| {}),
            RuleAction::StopProcessingMoreRules => self.set(sec, 17, |_| {}),
            RuleAction::DesktopAlert => self.set(sec, 18, |_| {}),
            RuleAction::DeferDelivery(n) => self.set(sec, 19, |s| s.n1 = u64::from(*n)),
            RuleAction::CcTo(l) => self.set(sec, 20, |s| s.text = words(l)),
            RuleAction::MarkSensitivity(x) => self.set(sec, 21, |s| s.choice = sens_idx(*x)),
            RuleAction::NotifyWhenRead => self.set(sec, 22, |_| {}),
            RuleAction::NotifyWhenDelivered => self.set(sec, 23, |_| {}),
            RuleAction::ServerReply(t) => self.set(sec, 24, |s| s.text = t.clone()),
        }
    }

    fn conditions(&self, app: &App, sec: Sec) -> Result<Vec<RuleCondition>, String> {
        let mut out = Vec::new();
        if sec == Sec::Cond && self.outgoing {
            out.push(RuleCondition::AfterSending);
        }
        for (i, s) in self.slots(sec).iter().enumerate() {
            if !s.on || !offered(sec, i, self.outgoing) {
                continue;
            }
            let text = || -> Result<String, String> {
                let t = s.text.trim();
                if t.is_empty() { Err(missing(sec, i)) } else { Ok(t.to_owned()) }
            };
            let list = || -> Result<Vec<String>, String> {
                let l = common::split_list(&s.text);
                if l.is_empty() { Err(missing(sec, i)) } else { Ok(l) }
            };
            out.push(match i {
                0 => RuleCondition::SubjectContains(list()?),
                1 => RuleCondition::From(list()?),
                2 => {
                    if s.text.trim().is_empty() {
                        return Err(missing(sec, i));
                    }
                    RuleCondition::ThroughAccount(s.text.trim().to_owned())
                }
                3 => RuleCondition::SentOnlyToMe,
                4 => RuleCondition::MyNameInTo,
                5 => RuleCondition::MarkedImportance(imp(s.choice)),
                6 => RuleCondition::MarkedSensitivity(sens(s.choice)),
                7 => RuleCondition::FlaggedForAction,
                8 => RuleCondition::MyNameInCc,
                9 => RuleCondition::MyNameInToOrCc,
                10 => RuleCondition::MyNameNotInTo,
                11 => RuleCondition::SentTo(list()?),
                12 => RuleCondition::BodyContains(list()?),
                13 => RuleCondition::SubjectOrBodyContains(list()?),
                14 => RuleCondition::HeaderContains(list()?),
                15 => RuleCondition::RecipientAddressContains(list()?),
                16 => RuleCondition::SenderAddressContains(list()?),
                17 => {
                    if s.text.trim().is_empty() {
                        return Err(missing(sec, i));
                    }
                    RuleCondition::AssignedToCategory(s.text.trim().to_owned())
                }
                18 => RuleCondition::HasAttachment,
                19 => RuleCondition::SizeBetween(s.n1.min(s.n2), s.n1.max(s.n2)),
                20 => {
                    let a = common::parse_date(app, &s.d1, "00:00");
                    let b = common::parse_date(app, &s.d2, "23:59");
                    match (a, b) {
                        (Some(a), Some(b)) => RuleCondition::ReceivedBetween(a, b),
                        _ => return Err(missing(sec, i)),
                    }
                }
                21 => RuleCondition::AutomaticReply,
                22 => RuleCondition::MeetingInvitationOrUpdate,
                23 => RuleCondition::UsesForm(text()?),
                24 => RuleCondition::FormType(text()?),
                25 => RuleCondition::FromRssFeedWithText(list()?),
                26 => RuleCondition::FromAnyRssFeed,
                27 => RuleCondition::SuspectedJunk,
                28 => RuleCondition::SenderInContacts,
                29 => RuleCondition::OnThisComputerOnly,
                _ => continue,
            });
        }
        Ok(out)
    }

    fn actions(&self) -> Result<Vec<RuleAction>, String> {
        let mut out = Vec::new();
        for (i, s) in self.acts.iter().enumerate() {
            if !s.on || !offered(Sec::Act, i, self.outgoing) {
                continue;
            }
            let list = || -> Result<Vec<String>, String> {
                let l = common::split_list(&s.text);
                if l.is_empty() { Err(missing(Sec::Act, i)) } else { Ok(l) }
            };
            let text = || -> Result<String, String> {
                let t = s.text.trim();
                if t.is_empty() { Err(missing(Sec::Act, i)) } else { Ok(t.to_owned()) }
            };
            let folder = || s.folder.map(FolderId).ok_or_else(|| missing(Sec::Act, i));
            out.push(match i {
                0 => RuleAction::NewItemAlert(s.text.trim().to_owned()),
                1 => RuleAction::MoveTo(folder()?),
                2 => RuleAction::AssignCategory(text()?),
                3 => RuleAction::Delete,
                4 => RuleAction::PermanentlyDelete,
                5 => RuleAction::CopyTo(folder()?),
                6 => RuleAction::ForwardTo(list()?),
                7 => RuleAction::ForwardAsAttachment(list()?),
                8 => RuleAction::RedirectTo(list()?),
                9 => RuleAction::ReplyWithTemplate(text()?),
                10 => RuleAction::FlagForFollowUp(
                    if s.text.trim().is_empty() { "Follow up".into() } else { s.text.trim().to_owned() },
                    u32::try_from(s.n1).unwrap_or(0),
                ),
                11 => RuleAction::ClearFlag,
                12 => RuleAction::ClearCategories,
                13 => RuleAction::MarkImportance(imp(s.choice)),
                14 => RuleAction::Print,
                15 => RuleAction::PlaySound(text()?),
                16 => RuleAction::MarkAsRead,
                17 => RuleAction::StopProcessingMoreRules,
                18 => RuleAction::DesktopAlert,
                19 => RuleAction::DeferDelivery(u32::try_from(s.n1.clamp(1, 40_320)).unwrap_or(1)),
                20 => RuleAction::CcTo(list()?),
                21 => RuleAction::MarkSensitivity(sens(s.choice)),
                22 => RuleAction::NotifyWhenRead,
                23 => RuleAction::NotifyWhenDelivered,
                24 => RuleAction::ServerReply(text()?),
                _ => continue,
            });
        }
        // Stop Processing More Rules reads last in the description.
        if let Some(pos) = out.iter().position(|a| *a == RuleAction::StopProcessingMoreRules) {
            let stop = out.remove(pos);
            out.push(stop);
        }
        Ok(out)
    }

    /// Display text of an entry's value (the placeholder until set).
    fn value_text(&self, app: &App, sec: Sec, i: usize) -> Option<String> {
        let def = defs(sec).get(i)?;
        let s = self.slots(sec).get(i)?;
        let set = |t: String| if t.trim().is_empty() { def.placeholder.to_owned() } else { t };
        Some(match def.val {
            Val::None => return None,
            Val::People => set(common::split_list(&s.text).join(" or ")),
            Val::Words => set(common::split_list(&s.text).join(" or ")),
            Val::Account | Val::Category | Val::Text | Val::Form | Val::FormType => {
                set(s.text.trim().to_owned())
            }
            Val::Template | Val::Sound => {
                let t = s.text.trim();
                let name = std::path::Path::new(t).file_name().map(|n| n.to_string_lossy().into_owned());
                set(name.unwrap_or_else(|| t.to_owned()))
            }
            Val::Reply => {
                let first = s.text.trim().lines().next().unwrap_or_default().trim().to_owned();
                set(if first.chars().count() > 40 {
                    format!("\"{}…\"", first.chars().take(40).collect::<String>())
                } else if first.is_empty() {
                    first
                } else {
                    format!("\"{first}\"")
                })
            }
            Val::Minutes => {
                if s.on {
                    s.n1.to_string()
                } else {
                    def.placeholder.to_owned()
                }
            }
            Val::Importance => {
                format!("{} importance", IMPORTANCE.get(s.choice).copied().unwrap_or("normal"))
            }
            Val::Sensitivity => {
                format!("{} sensitivity", SENSITIVITY.get(s.choice).copied().unwrap_or("normal"))
            }
            Val::Size => {
                if s.on || s.n1 > 0 || s.n2 != 1024 {
                    format!("between {} KB and {} KB", s.n1.min(s.n2), s.n1.max(s.n2))
                } else {
                    def.placeholder.to_owned()
                }
            }
            Val::Dates => {
                if s.d1.trim().is_empty() || s.d2.trim().is_empty() {
                    def.placeholder.to_owned()
                } else {
                    format!("between {} and {}", s.d1.trim(), s.d2.trim())
                }
            }
            Val::Folder => match s.folder.and_then(|f| app.s.mailbox.folder(FolderId(f))) {
                Some(f) => f.name.clone(),
                None => def.placeholder.to_owned(),
            },
            Val::Flag => {
                let label = if s.text.trim().is_empty() { "Follow up" } else { s.text.trim() };
                match s.n1 {
                    0 => format!("{label} today"),
                    1 => format!("{label} in 1 day"),
                    n => format!("{label} in {n} days"),
                }
            }
        })
    }

    fn is_client_only(&self) -> bool {
        self.actions().is_ok_and(|a| a.iter().any(RuleAction::is_client_only))
    }

    /// A default name from the first condition's value.
    fn default_name(&self, app: &App) -> String {
        for (i, s) in self.conds.iter().enumerate() {
            if s.on
                && offered(Sec::Cond, i, self.outgoing)
                && let Some(v) = self.value_text(app, Sec::Cond, i)
                && !v.is_empty()
                && CONDS.get(i).is_some_and(|d| d.placeholder != v)
            {
                return v;
            }
        }
        "New rule".to_owned()
    }
}

fn missing(sec: Sec, i: usize) -> String {
    let what = defs(sec).get(i).map_or("", |d| d.placeholder);
    format!("Click the underlined value \"{what}\" and fill it in before continuing.")
}

fn imp(i: usize) -> Importance {
    match i {
        0 => Importance::Low,
        2 => Importance::High,
        _ => Importance::Normal,
    }
}

fn imp_idx(i: Importance) -> usize {
    match i {
        Importance::Low => 0,
        Importance::Normal => 1,
        Importance::High => 2,
    }
}

fn sens(i: usize) -> Sensitivity {
    match i {
        1 => Sensitivity::Personal,
        2 => Sensitivity::Private,
        3 => Sensitivity::Confidential,
        _ => Sensitivity::Normal,
    }
}

fn sens_idx(s: Sensitivity) -> usize {
    match s {
        Sensitivity::Normal => 0,
        Sensitivity::Personal => 1,
        Sensitivity::Private => 2,
        Sensitivity::Confidential => 3,
    }
}

/// "except if …" form of a condition text for the exceptions page.
fn except_prefix(before: &str) -> &'static str {
    if before.starts_with("where") || before.starts_with("which") || before.starts_with("sent only") {
        "except "
    } else {
        "except if "
    }
}

// ---------------------------------------------------------------------------------------------------------
// Description rendering

/// Clicked underlined value.
type Click = Option<(Sec, usize)>;

/// One entry's text with its value underlined; returns a click on the value.
fn entry_line(ui: &mut Ui, app: &App, draft: &Draft, sec: Sec, i: usize, prefix: &str) -> Click {
    let def = defs(sec).get(i)?;
    let mut click = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let before = if sec == Sec::Exc {
            format!("{prefix}{}{}", except_prefix(def.before), def.before)
        } else {
            format!("{prefix}{}", def.before)
        };
        if !before.is_empty() {
            ui.label(before);
        }
        if let Some(v) = draft.value_text(app, sec, i) {
            let r = ui.add(
                egui::Label::new(RichText::new(v).underline().color(app.theme.link)).sense(Sense::click()),
            );
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                click = Some((sec, i));
            }
        }
        if !def.after.is_empty() {
            ui.label(def.after);
        }
    });
    click
}

/// The natural-language description box (Step 2). Returns a clicked value.
fn description(
    ui: &mut Ui,
    app: &App,
    draft: &Draft,
    salt: impl std::hash::Hash + std::fmt::Debug,
    height: f32,
) -> Click {
    let mut click = None;
    let frame = egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, app.theme.separator))
        .fill(app.theme.reading)
        .inner_margin(6.0);
    frame.show(ui, |ui| {
        egui::ScrollArea::vertical()
            .id_salt(salt)
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(if draft.outgoing {
                    "Apply this rule after I send the message"
                } else {
                    "Apply this rule after the message arrives"
                });
                for sec in [Sec::Cond, Sec::Act, Sec::Exc] {
                    let mut first = true;
                    for (i, s) in draft.slots(sec).iter().enumerate() {
                        if !s.on || !offered(sec, i, draft.outgoing) {
                            continue;
                        }
                        let prefix = match (sec, first) {
                            (Sec::Exc, true) => "",
                            (Sec::Exc, false) => "or ",
                            (_, true) => "",
                            (_, false) => "and ",
                        };
                        first = false;
                        if let Some(c) = entry_line(ui, app, draft, sec, i, prefix) {
                            click = Some(c);
                        }
                    }
                }
            });
    });
    click
}

/// Small action icons for a rule row.
fn action_icon(a: &RuleAction) -> &'static str {
    match a {
        RuleAction::MoveTo(_) => "move",
        RuleAction::CopyTo(_) => "copy",
        RuleAction::AssignCategory(_) | RuleAction::ClearCategories => "category-tag",
        RuleAction::Delete | RuleAction::PermanentlyDelete => "delete",
        RuleAction::NewItemAlert(_) | RuleAction::DesktopAlert | RuleAction::PlaySound(_) => "bell",
        RuleAction::ForwardTo(_) | RuleAction::ForwardAsAttachment(_) | RuleAction::RedirectTo(_) => {
            "forward"
        }
        RuleAction::ReplyWithTemplate(_) => "reply",
        RuleAction::FlagForFollowUp(..) | RuleAction::ClearFlag => "flag",
        RuleAction::MarkImportance(Importance::Low) => "importance-low",
        RuleAction::MarkImportance(_) => "importance-high",
        RuleAction::Print => "print",
        RuleAction::MarkAsRead => "read",
        RuleAction::StopProcessingMoreRules => "block",
        RuleAction::DeferDelivery(_) => "calendar-reminder",
        RuleAction::CcTo(_) => "forward",
        RuleAction::MarkSensitivity(_) => "private",
        RuleAction::NotifyWhenRead | RuleAction::NotifyWhenDelivered => "read",
        RuleAction::ServerReply(_) => "reply",
    }
}

// ---------------------------------------------------------------------------------------------------------
// Value editor (shared by the wizard)

/// Inline editor for one underlined value. Returns `Some(out)` when it opens a picker; sets `*done` when the
/// user is finished.
fn value_editor(
    ui: &mut Ui,
    app: &mut App,
    uid: u64,
    draft: &mut Draft,
    sec: Sec,
    i: usize,
    done: &mut bool,
) -> Option<Out> {
    let def = defs(sec).get(i)?;
    let cats: Vec<String> = super::categories::category_names(app);
    let accounts: Vec<String> = app.s.mailbox.accounts.iter().map(common::account_title).collect();
    let mut out = None;
    let frame = egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, app.theme.accent))
        .inner_margin(8.0)
        .corner_radius(4.0);
    frame.show(ui, |ui| {
        let Some(s) = draft.slots_mut(sec).get_mut(i) else { return };
        s.on = true;
        ui.label(RichText::new(format!("{}{}{}", def.before, def.placeholder, def.after)).strong());
        match def.val {
            Val::People => {
                ui.label("Addresses or names (separate with ;):");
                ui.add(egui::TextEdit::singleline(&mut s.text).desired_width(f32::INFINITY));
            }
            Val::Words => {
                ui.label("Words or phrases to search for (separate with ;):");
                ui.add(egui::TextEdit::singleline(&mut s.text).desired_width(f32::INFINITY));
            }
            Val::Text => {
                ui.add(
                    egui::TextEdit::singleline(&mut s.text)
                        .hint_text("Message to show")
                        .desired_width(f32::INFINITY),
                );
            }
            Val::Form => {
                let forms: Vec<&str> = mailcraft_model::rule::FORMS.iter().map(|(n, _)| *n).collect();
                if s.text.trim().is_empty() {
                    s.text = "Message".into();
                }
                common::combo(ui, ("rform", uid, i), &mut s.text, &forms, 260.0);
            }
            Val::FormType => {
                ui.label("Message class (for example IPM.Schedule.Meeting):");
                ui.add(
                    egui::TextEdit::singleline(&mut s.text)
                        .hint_text("IPM.Note")
                        .desired_width(f32::INFINITY),
                );
            }
            Val::Minutes => {
                ui.horizontal(|ui| {
                    ui.label("Defer delivery by");
                    ui.add(egui::DragValue::new(&mut s.n1).range(1..=40_320));
                    ui.label("minutes");
                });
            }
            Val::Template | Val::Sound => {
                let (hint, title, filter) = if def.val == Val::Template {
                    (
                        "Reply template (.eml)",
                        "Select a Reply Template",
                        crate::FileFilter::new("Templates", &["eml"]),
                    )
                } else {
                    (
                        "Sound file",
                        "Select a Sound",
                        crate::FileFilter::new("Sounds", &["wav", "ogg", "mp3", "flac"]),
                    )
                };
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut s.text).hint_text(hint).desired_width(300.0));
                    if common::browse_button(ui, app)
                        && let Some(path) = common::browse_open(app, title, &[filter], &s.text)
                    {
                        s.text = path;
                    }
                });
                if def.val == Val::Template {
                    muted(
                        ui,
                        app,
                        "Save a message with File › Save As (.eml) to use it as a reply template.",
                    );
                }
            }
            Val::Reply => {
                ui.label("Text of the reply:");
                ui.add(egui::TextEdit::multiline(&mut s.text).desired_rows(4).desired_width(f32::INFINITY));
            }
            Val::Account => {
                let choices: Vec<&str> = accounts.iter().map(String::as_str).collect();
                common::combo(ui, ("racct", uid, i), &mut s.text, &choices, 260.0);
            }
            Val::Category => {
                let choices: Vec<&str> = cats.iter().map(String::as_str).collect();
                common::combo(ui, ("rcat", uid, i), &mut s.text, &choices, 260.0);
            }
            Val::Importance => {
                common::combo_idx(ui, ("rimp", uid, i), &mut s.choice, &IMPORTANCE, 140.0);
            }
            Val::Sensitivity => {
                common::combo_idx(ui, ("rsens", uid, i), &mut s.choice, &SENSITIVITY, 140.0);
            }
            Val::Size => {
                ui.horizontal(|ui| {
                    ui.label("At least");
                    ui.add(egui::DragValue::new(&mut s.n1).range(0..=10_000_000).suffix(" KB"));
                    ui.label("and at most");
                    ui.add(egui::DragValue::new(&mut s.n2).range(0..=10_000_000).suffix(" KB"));
                });
            }
            Val::Dates => {
                ui.horizontal(|ui| {
                    ui.label("After");
                    ui.add(
                        egui::TextEdit::singleline(&mut s.d1).hint_text("YYYY-MM-DD").desired_width(100.0),
                    );
                    ui.label("Before");
                    ui.add(
                        egui::TextEdit::singleline(&mut s.d2).hint_text("YYYY-MM-DD").desired_width(100.0),
                    );
                });
            }
            Val::Folder => {
                ui.horizontal(|ui| {
                    let p = folder_path(app, s.folder);
                    ui.label(if p.is_empty() { "No folder chosen".to_owned() } else { p });
                    if btn(ui, "Choose Folder...") {
                        let key = format!("folder:{}:{i}", sec_key(sec));
                        out = Some(Out::open("pickFolder", child(uid, &key, json!({"selected": s.folder}))));
                    }
                });
            }
            Val::Flag => {
                ui.horizontal(|ui| {
                    if s.text.is_empty() {
                        s.text = "Follow up".into();
                    }
                    common::combo(ui, ("rflag", uid, i), &mut s.text, &FLAG_LABELS, 160.0);
                    ui.label("due in");
                    ui.add(egui::DragValue::new(&mut s.n1).range(0..=365));
                    ui.label("days");
                });
            }
            Val::None => {}
        }
        if btn(ui, "Done") {
            *done = true;
        }
    });
    out
}

fn sec_key(s: Sec) -> &'static str {
    match s {
        Sec::Cond => "c",
        Sec::Act => "a",
        Sec::Exc => "e",
    }
}

fn apply_folder_reply(draft: &mut Draft, key: &str, v: &Value) {
    let mut parts = key.split(':');
    let (Some("folder"), Some(sec), Some(i)) = (parts.next(), parts.next(), parts.next()) else { return };
    let sec = match sec {
        "c" => Sec::Cond,
        "e" => Sec::Exc,
        _ => Sec::Act,
    };
    let Ok(i) = i.parse::<usize>() else { return };
    let f = v.as_u64();
    draft.set(sec, i, |s| s.folder = f);
}

// ---------------------------------------------------------------------------------------------------------
// Create Rule (quick dialog)

#[derive(Clone, Default)]
struct QuickRule {
    from_on: bool,
    subject_on: bool,
    subject: String,
    sent_on: bool,
    sent_choice: usize,
    alert_on: bool,
    sound_on: bool,
    sound: String,
    move_on: bool,
    folder: Option<u64>,
    error: String,
}

fn create_rule(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let from = d.str("from");
    let from_name = if d.str("fromName").is_empty() { from.clone() } else { d.str("fromName") };
    let sent_to = d.str("sentTo");
    let subject0 = d.str("subject");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| QuickRule { subject: subject0.clone(), ..Default::default() },
        |st, app| {
            for (k, v) in take_replies(ctx, uid) {
                if k == "folder" {
                    st.folder = v.as_u64();
                    st.move_on = st.folder.is_some();
                }
            }
            window(ctx, app, d, "Create Rule", 470.0, |ui, app| {
                common::heading(ui, app, "When I get email with all of the selected conditions");
                ui.add_enabled(
                    !from.is_empty(),
                    egui::Checkbox::new(&mut st.from_on, format!("From {from_name}")),
                );
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.subject_on, "Subject contains");
                    ui.add(egui::TextEdit::singleline(&mut st.subject).desired_width(250.0));
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.sent_on, "Sent to");
                    let mut choices = vec!["me only"];
                    if !sent_to.is_empty() {
                        choices.push(sent_to.as_str());
                    }
                    common::combo_idx(ui, ("sentto", uid), &mut st.sent_choice, &choices, 250.0);
                });
                common::heading(ui, app, "Do the following");
                ui.checkbox(&mut st.alert_on, "Display in the New Item Alert window");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.sound_on, "Play a selected sound:");
                    ui.add(
                        egui::TextEdit::singleline(&mut st.sound)
                            .hint_text("Sound file path")
                            .desired_width(220.0),
                    );
                    if let Some(p) = browse_sound(ui, app, &st.sound) {
                        st.sound = p;
                        st.sound_on = true;
                    }
                });
                let mut out = Out::Keep;
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.move_on, "Move the item to folder:");
                    let p = folder_path(app, st.folder);
                    ui.label(
                        RichText::new(if p.is_empty() { "Select Folder".to_owned() } else { p })
                            .color(app.theme.text_muted),
                    );
                    if btn(ui, "Select Folder...") {
                        out = Out::open("pickFolder", child(uid, "folder", json!({"selected": st.folder})));
                    }
                });
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let any_cond = (st.from_on && !from.is_empty())
                    || (st.subject_on && !st.subject.trim().is_empty())
                    || st.sent_on;
                let any_act = st.alert_on
                    || (st.sound_on && !st.sound.trim().is_empty())
                    || (st.move_on && st.folder.is_some());
                let mut ok = false;
                footer(ui, |ui| {
                    if btn(ui, "Advanced Options...") {
                        let (c, a) = quick_parts(st, &from, &sent_to);
                        out = Out::close_open(
                            "rulesWizard",
                            json!({"prefill": {"conditions": c, "actions": a}, "name": quick_name(st, &from_name)}),
                        );
                    }
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    ok = primary(ui, app, any_cond && any_act, "OK");
                });
                if ok && matches!(out, Out::Keep) {
                    let (c, a) = quick_parts(st, &from, &sent_to);
                    let name = quick_name(st, &from_name);
                    match app.run("rules.create", json!({"name": name, "conditions": c, "actions": a})) {
                        Some(v) => {
                            let id = v.get("id").cloned().unwrap_or(Value::Null);
                            let client =
                                v.pointer("/rule/clientOnly").and_then(Value::as_bool).unwrap_or(false);
                            out = Out::close_open(
                                "ruleCreated",
                                json!({"id": id, "name": name, "clientOnly": client}),
                            );
                        }
                        None => st.error = last_error(app),
                    }
                }
                out
            })
        },
    )
}

fn quick_name(st: &QuickRule, from_name: &str) -> String {
    if st.subject_on && !st.subject.trim().is_empty() {
        st.subject.trim().to_owned()
    } else if !from_name.is_empty() {
        from_name.to_owned()
    } else {
        "New rule".to_owned()
    }
}

fn quick_parts(st: &QuickRule, from: &str, sent_to: &str) -> (Vec<RuleCondition>, Vec<RuleAction>) {
    let mut c = Vec::new();
    if st.from_on && !from.is_empty() {
        c.push(RuleCondition::From(vec![from.to_owned()]));
    }
    if st.subject_on && !st.subject.trim().is_empty() {
        c.push(RuleCondition::SubjectContains(vec![st.subject.trim().to_owned()]));
    }
    if st.sent_on {
        if st.sent_choice == 1 && !sent_to.is_empty() {
            c.push(RuleCondition::SentTo(vec![sent_to.to_owned()]));
        } else {
            c.push(RuleCondition::SentOnlyToMe);
        }
    }
    let mut a = Vec::new();
    if st.alert_on {
        a.push(RuleAction::NewItemAlert(String::new()));
    }
    if st.sound_on && !st.sound.trim().is_empty() {
        a.push(RuleAction::PlaySound(st.sound.trim().to_owned()));
    }
    if st.move_on
        && let Some(f) = st.folder
    {
        a.push(RuleAction::MoveTo(FolderId(f)));
    }
    (c, a)
}

#[derive(Clone, Default)]
struct CreatedState {
    run_now: bool,
}

fn rule_created(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    with_state(
        ctx,
        app,
        uid,
        |_: &App| CreatedState::default(),
        |st, app| {
            window(ctx, app, d, "Success", 400.0, |ui, app| {
                ui.label(format!("The rule \"{}\" has been created.", d.str("name")));
                if d.bool("clientOnly") {
                    muted(
                        ui,
                        app,
                        "It runs only while MailCraft is open, because some of its actions happen in this app.",
                    );
                }
                let folder = app.s.selection.folder;
                let fname = app.s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
                ui.checkbox(&mut st.run_now, format!("Run this rule now on messages already in \"{fname}\""));
                footer(ui, |ui| {
                    if primary(ui, app, true, "OK") {
                        if st.run_now
                            && let Some(id) = d.u64("id")
                        {
                            app.run("rules.runNow", json!({"ids": [id], "folder": folder.0}));
                        }
                        return Out::Close;
                    }
                    Out::Keep
                })
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Rules and Alerts

#[derive(Clone, Default)]
struct RulesState {
    tab: usize,
    selected: Option<u64>,
    /// Selected row of the Manage Alerts tab.
    alert: Option<u64>,
}

fn rules_and_alerts(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init =
        |app: &App| RulesState { tab: 0, selected: app.s.mailbox.rules.first().map(|r| r.id), alert: None };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            let Some(f) = v.as_u64() else { continue };
            let (kind, id) = k.split_once(':').unwrap_or(("", ""));
            if let Ok(id) = id.parse::<u64>() {
                let action = if kind == "copyTo" {
                    RuleAction::CopyTo(FolderId(f))
                } else {
                    RuleAction::MoveTo(FolderId(f))
                };
                add_action(app, id, action);
            }
        }
        if st.selected.is_none_or(|id| !app.s.mailbox.rules.iter().any(|r| r.id == id)) {
            st.selected = app.s.mailbox.rules.first().map(|r| r.id);
        }
        window(ctx, app, d, "Rules and Alerts", 620.0, |ui, app| {
            ui.horizontal(|ui| {
                for (i, t) in ["Email Rules", "Manage Alerts"].iter().enumerate() {
                    if ui.selectable_label(st.tab == i, *t).clicked() {
                        st.tab = i;
                    }
                }
            });
            ui.separator();
            if st.tab == 1 {
                let out = manage_alerts(ui, app, uid, st);
                if !matches!(out, Out::Keep) {
                    return out;
                }
                return ok_cancel(ui, app, true).map_or(Out::Keep, |_| Out::Close);
            }
            let rules = app.s.mailbox.rules.clone();
            let sel = st.selected.and_then(|id| rules.iter().position(|r| r.id == id));
            let sel_rule = sel.and_then(|i| rules.get(i)).cloned();
            let mut out = Out::Keep;
            ui.horizontal_wrapped(|ui| {
                if ui.button("New Rule...").clicked() {
                    app.run("rules.new", json!({}));
                }
                ui.add_enabled_ui(sel_rule.is_some(), |ui| {
                    ui.menu_button("Change Rule", |ui| {
                        if let Some(r) = &sel_rule {
                            out = change_rule_menu(ui, app, uid, r).or(Out::Keep);
                        }
                    });
                });
                if ui.add_enabled(sel_rule.is_some(), egui::Button::new("Copy...")).clicked()
                    && let Some(r) = &sel_rule
                {
                    out = Out::open("copyRule", json!({"id": r.id, "name": r.name}));
                }
                if ui.add_enabled(sel_rule.is_some(), egui::Button::new("Delete")).clicked()
                    && let Some(r) = &sel_rule
                {
                    out = Out::open(
                        "confirm",
                        json!({"title": "Rules and Alerts", "text": format!("Delete rule \"{}\"?", r.name), "command": "rules.delete", "params": {"id": r.id}}),
                    );
                }
                let up = sel.is_some_and(|i| i > 0);
                let down = sel.is_some_and(|i| i + 1 < rules.len());
                if ui.add_enabled(up, egui::Button::new("▲")).on_hover_text("Move Up").clicked()
                    && let Some(r) = &sel_rule
                {
                    app.run("rules.moveUp", json!({"id": r.id}));
                }
                if ui.add_enabled(down, egui::Button::new("▼")).on_hover_text("Move Down").clicked()
                    && let Some(r) = &sel_rule
                {
                    app.run("rules.moveDown", json!({"id": r.id}));
                }
                if ui.add_enabled(!rules.is_empty(), egui::Button::new("Run Rules Now...")).clicked() {
                    app.run("folder.runRulesNow", json!({}));
                }
                if ui.button("Options").clicked() {
                    out = Out::open("rulesOptions", json!({}));
                }
            });
            ui.add_space(4.0);
            // List
            let frame =
                egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0);
            frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(26.0);
                    ui.label(RichText::new("Rule (applied in the order shown)").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(20.0);
                        ui.label(RichText::new("Actions").strong());
                    });
                });
                egui::ScrollArea::vertical()
                    .id_salt(("rules", uid))
                    .max_height(170.0)
                    .min_scrolled_height(170.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if rules.is_empty() {
                            muted(ui, app, "No rules yet. Choose New Rule... to create one.");
                        }
                        for r in &rules {
                            let selected = st.selected == Some(r.id);
                            ui.horizontal(|ui| {
                                let mut on = r.enabled;
                                if ui
                                    .add(egui::Checkbox::without_text(&mut on))
                                    .on_hover_text("Turn this rule on or off")
                                    .changed()
                                {
                                    app.run("rules.enable", json!({"id": r.id, "enabled": on}));
                                }
                                let w = ui.available_width() - 90.0;
                                let resp = ui.add_sized(
                                    [w.max(60.0), 20.0],
                                    egui::Button::selectable(selected, r.list_label()),
                                );
                                if resp.clicked() {
                                    st.selected = Some(r.id);
                                }
                                if resp.double_clicked() {
                                    app.run("rules.edit", json!({"id": r.id}));
                                }
                                for a in r.actions.iter().take(4) {
                                    let (rect, _) =
                                        ui.allocate_exact_size(egui::Vec2::splat(16.0), Sense::hover());
                                    app.icons.paint(ui, rect, action_icon(a), app.theme.text_muted);
                                }
                            });
                        }
                    });
            });
            ui.label("Rule description (click an underlined value to edit):");
            if let Some(r) = &sel_rule {
                let draft = Draft::from_rule(app, &r.conditions, &r.actions, &r.exceptions);
                if description(ui, app, &draft, ("rdesc", uid), 90.0).is_some() {
                    app.run("rules.edit", json!({"id": r.id}));
                }
            } else {
                description(ui, app, &Draft::default(), ("rdesc", uid), 90.0);
            }
            let mut rss = common::pref(ctx, "rules.rss").and_then(|v| v.as_bool()).unwrap_or(false);
            if ui.checkbox(&mut rss, "Enable rules on all messages downloaded from RSS Feeds").changed() {
                common::set_pref(ctx, "rules.rss", json!(rss));
            }
            if !matches!(out, Out::Keep) {
                return out;
            }
            footer(ui, |ui| {
                ui.add_enabled(false, egui::Button::new("Apply").min_size(egui::Vec2::new(80.0, 26.0)))
                    .on_disabled_hover_text("Changes are applied as you make them (each can be undone).");
                if btn(ui, "Cancel") {
                    return Out::Close;
                }
                if primary(ui, app, true, "OK") {
                    return Out::Close;
                }
                Out::Keep
            })
        })
    })
}

/// A Browse... button for a sound file (shown when the app has file dialogs); the chosen path.
fn browse_sound(ui: &mut Ui, app: &App, current: &str) -> Option<String> {
    if !common::browse_button(ui, app) {
        return None;
    }
    let filter = crate::FileFilter::new("Sounds", &["wav", "ogg", "mp3", "flac"]);
    common::browse_open(app, "Select a Sound", &[filter], current)
}

/// Change Rule quick action: adds `action` (replacing one of the same kind).
fn add_action(app: &mut App, id: u64, action: RuleAction) {
    app.run("rules.addAction", json!({"id": id, "action": action}));
}

/// Manage Alerts tab: the rules that show alerts (New Item Alert window, Desktop Alert, sound), with New
/// Alert (a rule from the alert template) and Alert Rules (back to the rules list on that rule).
fn manage_alerts(ui: &mut Ui, app: &mut App, uid: u64, st: &mut RulesState) -> Out {
    let mut out = Out::Keep;
    ui.horizontal(|ui| {
        if ui.button("New Alert...").clicked() {
            out = Out::open("rulesWizard", json!({"template": TEMPLATE_ALERT}));
        }
        let alert_rules: Vec<u64> = app.s.mailbox.rules.iter().filter(|r| r.has_alert()).map(|r| r.id).collect();
        if ui.add_enabled(!alert_rules.is_empty(), egui::Button::new("Alert Rules...")).clicked() {
            st.tab = 0;
            st.selected = st.alert.filter(|id| alert_rules.contains(id)).or(alert_rules.first().copied());
        }
        if ui.add_enabled(st.alert.is_some(), egui::Button::new("Delete")).clicked()
            && let Some(id) = st.alert
            && let Some(r) = app.s.mailbox.rules.iter().find(|r| r.id == id)
        {
            out = Out::open(
                "confirm",
                json!({"title": "Rules and Alerts", "text": format!("Delete rule \"{}\"?", r.name), "command": "rules.delete", "params": {"id": id}}),
            );
        }
    });
    ui.add_space(4.0);
    let frame = egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0);
    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Alert source").strong());
            ui.add_space(160.0);
            ui.label(RichText::new("Alert shown").strong());
        });
        egui::ScrollArea::vertical()
            .id_salt(("alerts", uid))
            .max_height(220.0)
            .min_scrolled_height(220.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let rules: Vec<Rule> = app.s.mailbox.rules.iter().filter(|r| r.has_alert()).cloned().collect();
                if rules.is_empty() {
                    muted(
                        ui,
                        app,
                        "No alerts. Choose New Alert... to show messages from someone in the New Item Alert window.",
                    );
                }
                for r in &rules {
                    let shown: Vec<&str> = r
                        .actions
                        .iter()
                        .filter_map(|a| match a {
                            RuleAction::NewItemAlert(_) => Some("New Item Alert window"),
                            RuleAction::DesktopAlert => Some("Desktop Alert"),
                            RuleAction::PlaySound(_) => Some("Sound"),
                            _ => None,
                        })
                        .collect();
                    let label = format!("{}{}  —  {}", r.name, if r.enabled { "" } else { " (off)" }, shown.join(", "));
                    if ui.selectable_label(st.alert == Some(r.id), label).clicked() {
                        st.alert = Some(r.id);
                    }
                }
            });
    });
    muted(ui, app, "Alerts come from rules; RSS and web alerts are not available.");
    out
}

// ---------------------------------------------------------------------------------------------------------
// Copy rule (to other accounts)

#[derive(Clone, Default)]
struct CopyState {
    picked: Vec<u64>,
    error: String,
}

fn copy_rule(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let id = d.u64("id");
    let name = d.str("name");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| CopyState::default(),
        |st, app| {
            window(ctx, app, d, "Copy Rule", 420.0, |ui, app| {
                ui.label(format!("Copy the rule \"{name}\" to:"));
                let accounts: Vec<(u64, String, bool)> = app
                    .s
                    .mailbox
                    .accounts
                    .iter()
                    .map(|a| (u64::from(a.id.0), common::account_title(a), !a.address.trim().is_empty()))
                    .collect();
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, app.theme.separator))
                    .inner_margin(4.0)
                    .show(ui, |ui| {
                        ui.set_min_height(90.0);
                        for (aid, title, has_addr) in &accounts {
                            let mut on = st.picked.contains(aid);
                            if ui.add_enabled(*has_addr, egui::Checkbox::new(&mut on, title)).changed() {
                                if on {
                                    st.picked.push(*aid);
                                } else {
                                    st.picked.retain(|x| x != aid);
                                }
                            }
                        }
                    });
                muted(ui, app, "Each copy runs only on mail of its account; folders are matched by name.");
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    if primary(ui, app, !st.picked.is_empty(), "OK") {
                        match app.run("rules.copyToAccounts", json!({"id": id, "accounts": st.picked})) {
                            Some(_) => out = Out::Close,
                            None => st.error = last_error(app),
                        }
                    }
                    if btn_en(ui, id.is_some(), "Duplicate Here") {
                        match app.run("rules.copy", json!({"id": id})) {
                            Some(_) => out = Out::Close,
                            None => st.error = last_error(app),
                        }
                    }
                });
                out
            })
        },
    )
}

fn change_rule_menu(ui: &mut Ui, app: &mut App, uid: u64, r: &Rule) -> Out {
    let mut out = Out::Keep;
    if ui.button("Edit Rule Settings...").clicked() {
        app.run("rules.edit", json!({"id": r.id}));
        ui.close();
    }
    if ui.button("Rename Rule").clicked() {
        out = Out::open("renameRule", json!({"id": r.id, "name": r.name}));
        ui.close();
    }
    ui.separator();
    let quick: [(&str, Option<RuleAction>); 7] = [
        ("Display in the New Item Alert window", Some(RuleAction::NewItemAlert(String::new()))),
        ("Play a Sound...", None),
        ("Move to Folder...", None),
        ("Copy to Folder...", None),
        ("Mark as High Priority", Some(RuleAction::MarkImportance(Importance::High))),
        ("Mark as Low Priority", Some(RuleAction::MarkImportance(Importance::Low))),
        ("Delete Message", Some(RuleAction::Delete)),
    ];
    for (label, action) in quick {
        if ui.button(label).clicked() {
            match (label, action) {
                (_, Some(a)) => add_action(app, r.id, a),
                ("Play a Sound...", None) => {
                    out = Out::open("rulePrompt", json!({"id": r.id, "kind": "sound"}))
                }
                ("Move to Folder...", None) => {
                    out = Out::open("pickFolder", child(uid, &format!("moveTo:{}", r.id), json!({})));
                }
                _ => out = Out::open("pickFolder", child(uid, &format!("copyTo:{}", r.id), json!({}))),
            }
            ui.close();
        }
    }
    out
}

#[derive(Clone, Default)]
struct TextState {
    text: String,
    error: String,
}

fn rename_rule(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let old = d.str("name");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| TextState { text: old.clone(), ..Default::default() },
        |st, app| {
            window(ctx, app, d, &format!("Rename {old} rule"), 360.0, |ui, app| {
                ui.label("New name of rule:");
                ui.add(egui::TextEdit::singleline(&mut st.text).desired_width(f32::INFINITY));
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                match ok_cancel(ui, app, !st.text.trim().is_empty()) {
                    Some(true) => {
                        if app
                            .run("rules.rename", json!({"id": d.u64("id"), "name": st.text.trim()}))
                            .is_some()
                        {
                            Out::Close
                        } else {
                            st.error = last_error(app);
                            Out::Keep
                        }
                    }
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

fn rule_prompt(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    with_state(
        ctx,
        app,
        uid,
        |_: &App| TextState::default(),
        |st, app| {
            window(ctx, app, d, "Play a Sound", 380.0, |ui, app| {
                ui.label("Sound file to play when the rule runs:");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut st.text)
                            .hint_text("/path/to/sound.wav")
                            .desired_width(300.0),
                    );
                    if let Some(p) = browse_sound(ui, app, &st.text) {
                        st.text = p;
                    }
                });
                match ok_cancel(ui, app, !st.text.trim().is_empty()) {
                    Some(true) => {
                        if let Some(id) = d.u64("id") {
                            add_action(app, id, RuleAction::PlaySound(st.text.trim().to_owned()));
                        }
                        Out::Close
                    }
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

#[derive(Clone, Default)]
struct OptState {
    path: String,
    msg: String,
    replace: bool,
}

fn rules_options(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    with_state(
        ctx,
        app,
        uid,
        |_: &App| OptState::default(),
        |st, app| {
            window(ctx, app, d, "Options", 440.0, |ui, app| {
                common::heading(ui, app, "Import and export");
                ui.label("Rules file (.mcrules):");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut st.path)
                            .hint_text("/path/to/my-rules.mcrules")
                            .desired_width(330.0),
                    );
                    let filter = crate::FileFilter::new("MailCraft rules", &["mcrules", "json"]);
                    if common::browse_button(ui, app) {
                        let picked = if app.s.mailbox.rules.is_empty() {
                            common::browse_open(app, "Import Rules", &[filter], &st.path)
                        } else {
                            common::browse_save(app, "Rules File", &[filter], &st.path)
                        };
                        if let Some(p) = picked {
                            st.path = p;
                        }
                    }
                });
                ui.checkbox(&mut st.replace, "Replace my rules when importing");
                ui.horizontal(|ui| {
                    let has = !st.path.trim().is_empty();
                    if btn_en(ui, has && !app.s.mailbox.rules.is_empty(), "Export Rules...") {
                        st.msg = match app.run("rules.export", json!({"path": st.path.trim()})) {
                            Some(v) => format!(
                                "Exported {} rule(s).",
                                v.get("exported").and_then(Value::as_u64).unwrap_or(0)
                            ),
                            None => last_error(app),
                        };
                    }
                    if btn_en(ui, has, "Import Rules...") {
                        st.msg = match app
                            .run("rules.import", json!({"path": st.path.trim(), "replace": st.replace}))
                        {
                            Some(v) => format!(
                                "Imported {} rule(s).",
                                v.get("imported").and_then(Value::as_u64).unwrap_or(0)
                            ),
                            None => last_error(app),
                        };
                    }
                });
                if !st.msg.is_empty() {
                    muted(ui, app, &st.msg);
                }
                common::heading(ui, app, "Rules upgrade");
                ui.add_enabled(false, egui::Button::new("Upgrade Now"))
                    .on_disabled_hover_text("Rules are stored with your mailbox; nothing to upgrade.");
                ok_cancel(ui, app, true).map_or(Out::Keep, |_| Out::Close)
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Run Rules Now

#[derive(Clone, Default)]
struct RunState {
    picked: Vec<u64>,
    selected: Option<u64>,
    folder: Option<u64>,
    subfolders: bool,
    which: usize,
    msg: String,
}

fn run_rules_now(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |app: &App| RunState {
        folder: d.u64("folder").or(Some(app.s.selection.folder.0)),
        ..Default::default()
    };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if k == "folder" {
                st.folder = v.as_u64().or(st.folder);
            }
        }
        window(ctx, app, d, "Run Rules Now", 480.0, |ui, app| {
            ui.label("Select rules to run:");
            let rules = app.s.mailbox.rules.clone();
            egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(
                ui,
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(("rrn", uid))
                        .max_height(150.0)
                        .min_scrolled_height(150.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for r in &rules {
                                ui.horizontal(|ui| {
                                    let mut on = st.picked.contains(&r.id);
                                    if ui.add(egui::Checkbox::without_text(&mut on)).changed() {
                                        if on {
                                            st.picked.push(r.id);
                                        } else {
                                            st.picked.retain(|x| *x != r.id);
                                        }
                                    }
                                    if ui.selectable_label(st.selected == Some(r.id), &r.name).clicked() {
                                        st.selected = Some(r.id);
                                    }
                                });
                            }
                        });
                },
            );
            ui.label("Rule Description");
            if let Some(r) = st.selected.and_then(|id| rules.iter().find(|r| r.id == id)) {
                let draft = Draft::from_rule(app, &r.conditions, &r.actions, &r.exceptions);
                description(ui, app, &draft, ("rrd", uid), 70.0);
            } else {
                description(ui, app, &Draft::default(), ("rrd", uid), 70.0);
            }
            let mut out = Out::Keep;
            ui.horizontal(|ui| {
                ui.label("Run in Folder:");
                ui.label(RichText::new(folder_path(app, st.folder)).strong());
                if btn(ui, "Browse...") {
                    out = Out::open("pickFolder", child(uid, "folder", json!({"selected": st.folder})));
                }
            });
            ui.checkbox(&mut st.subfolders, "Include subfolders");
            ui.horizontal(|ui| {
                ui.label("Apply rules to:");
                common::combo_idx(
                    ui,
                    ("which", uid),
                    &mut st.which,
                    &["All Messages", "Read Messages", "Unread Messages"],
                    160.0,
                );
            });
            if !st.msg.is_empty() {
                muted(ui, app, &st.msg);
            }
            let close = footer(ui, |ui| {
                if btn(ui, "Close") {
                    return true;
                }
                if btn_en(ui, !st.picked.is_empty(), "Run Now") {
                    let which = ["all", "read", "unread"].get(st.which).copied().unwrap_or("all");
                    let res = app.run(
                        "folder.runRulesNow",
                        json!({"rules": st.picked, "folder": st.folder, "subfolders": st.subfolders, "which": which}),
                    );
                    st.msg = match res {
                        Some(v) => {
                            let n = v
                                .get("applied")
                                .or_else(|| v.get("matched"))
                                .and_then(Value::as_u64)
                                .unwrap_or(0);
                            format!("Done. The rules acted on {n} message(s).")
                        }
                        None => last_error(app),
                    };
                }
                if btn(ui, "Select All") {
                    st.picked = rules.iter().map(|r| r.id).collect();
                }
                false
            });
            if close { Out::Close } else { out }
        })
    })
}

// ---------------------------------------------------------------------------------------------------------
// Rules Wizard

#[derive(Clone)]
struct Wizard {
    page: usize,
    template: usize,
    draft: Draft,
    rule_id: Option<u64>,
    name: String,
    run_now: bool,
    enabled: bool,
    editing: Option<(Sec, usize)>,
    error: String,
}

/// Templates on the first page: (group, label, conditions ticked, actions ticked, for sent messages).
const TEMPLATES: [(&str, &str, &[usize], &[usize], bool); 10] = [
    ("Stay Organized", "Move messages from someone to a folder", &[1], &[1], false),
    ("Stay Organized", "Move messages with specific words in the subject to a folder", &[0], &[1], false),
    ("Stay Organized", "Move messages sent to a public group to a folder", &[11], &[1], false),
    ("Stay Organized", "Flag messages from someone for follow-up", &[1], &[10], false),
    ("Stay Organized", "Move RSS items from a specific RSS Feed to a folder", &[25], &[1], false),
    ("Stay Up to Date", "Display mail from someone in the New Item Alert window", &[1], &[0], false),
    ("Stay Up to Date", "Play a sound when I get messages from someone", &[1], &[15], false),
    (
        "Stay Up to Date",
        "Send an alert to my mobile device when I get messages from someone",
        &[1],
        &[],
        false,
    ),
    ("Start from a blank rule", "Apply rule on messages I receive", &[], &[], false),
    ("Start from a blank rule", "Apply rule on messages I send", &[], &[], true),
];

/// The New Item Alert template (Manage Alerts › New Alert).
const TEMPLATE_ALERT: usize = 5;
/// Not available here (no text messaging service).
const TEMPLATE_MOBILE: usize = 7;

fn template_draft(t: usize) -> Draft {
    let mut d = Draft::default();
    if let Some((_, _, c, a, outgoing)) = TEMPLATES.get(t) {
        d.outgoing = *outgoing;
        for i in c.iter() {
            d.set(Sec::Cond, *i, |_| {});
        }
        for i in a.iter() {
            d.set(Sec::Act, *i, |_| {});
        }
    }
    d
}

fn wizard(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |app: &App| {
        let parse_c = |v: Option<&Value>| -> Vec<RuleCondition> {
            v.cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
        };
        let parse_a = |v: Option<&Value>| -> Vec<RuleAction> {
            v.cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
        };
        if let Some(r) = d.get("rule") {
            let draft = Draft::from_rule(
                app,
                &parse_c(r.get("conditions")),
                &parse_a(r.get("actions")),
                &parse_c(r.get("exceptions")),
            );
            Wizard {
                page: 1,
                template: 0,
                draft,
                rule_id: r.get("id").and_then(Value::as_u64),
                name: r.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                run_now: false,
                enabled: r.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                editing: None,
                error: String::new(),
            }
        } else if let Some(p) = d.get("prefill") {
            let draft = Draft::from_rule(app, &parse_c(p.get("conditions")), &parse_a(p.get("actions")), &[]);
            Wizard {
                page: 1,
                template: 0,
                draft,
                rule_id: None,
                name: d.str("name"),
                run_now: false,
                enabled: true,
                editing: None,
                error: String::new(),
            }
        } else {
            let template = d
                .u64("template")
                .and_then(|t| usize::try_from(t).ok())
                .filter(|t| *t < TEMPLATES.len() && *t != TEMPLATE_MOBILE)
                .unwrap_or(0);
            Wizard {
                page: 0,
                template,
                draft: template_draft(template),
                rule_id: None,
                name: String::new(),
                run_now: false,
                enabled: true,
                editing: None,
                error: String::new(),
            }
        }
    };
    with_state(ctx, app, uid, init, |w, app| {
        for (k, v) in take_replies(ctx, uid) {
            apply_folder_reply(&mut w.draft, &k, &v);
        }
        window(ctx, app, d, "Rules Wizard", 520.0, |ui, app| wizard_body(ctx, ui, app, uid, w))
    })
}

fn wizard_body(ctx: &Context, ui: &mut Ui, app: &mut App, uid: u64, w: &mut Wizard) -> Out {
    let mut out = Out::Keep;
    let mut click: Click = None;
    match w.page {
        0 => {
            ui.label(RichText::new("Start from a template or from a blank rule").strong());
            ui.label("Step 1: Select a template");
            egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(
                ui,
                |ui| {
                    ui.set_min_height(200.0);
                    // As wide as the Step 2 box below it (sized to its content, it ended short of it).
                    ui.set_min_width(ui.available_width());
                    let mut group = "";
                    for (i, (g, label, _, _, _)) in TEMPLATES.iter().enumerate() {
                        if *g != group {
                            group = g;
                            ui.label(RichText::new(*g).color(app.theme.text_muted));
                        }
                        if i == TEMPLATE_MOBILE {
                            ui.add_enabled(false, egui::Button::selectable(false, *label)).on_disabled_hover_text(
                                "Alerts to a mobile device need a text messaging service, which MailCraft does not have.",
                            );
                            continue;
                        }
                        if ui.selectable_label(w.template == i, *label).clicked() {
                            w.template = i;
                            w.draft = template_draft(i);
                        }
                    }
                },
            );
            ui.label("Step 2: Edit the rule description (click an underlined value)");
            click = description(ui, app, &w.draft, ("wd0", uid), 110.0);
        }
        1..=3 => {
            let (sec, title) = match w.page {
                1 => (Sec::Cond, "Which condition(s) do you want to check?"),
                2 => (Sec::Act, "What do you want to do with the message?"),
                _ => (Sec::Exc, "Are there any exceptions?"),
            };
            ui.label(RichText::new(title).strong());
            ui.label("Step 1: Select condition(s)".replace(
                "condition(s)",
                match sec {
                    Sec::Act => "action(s)",
                    Sec::Exc => "exception(s) (if necessary)",
                    Sec::Cond => "condition(s)",
                },
            ));
            egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(
                ui,
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(("wl", uid, w.page))
                        .max_height(190.0)
                        .min_scrolled_height(190.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for i in 0..defs(sec).len() {
                                if !offered(sec, i, w.draft.outgoing) {
                                    continue;
                                }
                                ui.horizontal(|ui| {
                                    let mut on = w.draft.slots(sec).get(i).is_some_and(|s| s.on);
                                    if ui.add(egui::Checkbox::without_text(&mut on)).changed()
                                        && let Some(s) = w.draft.slots_mut(sec).get_mut(i)
                                    {
                                        s.on = on;
                                    }
                                    if let Some(c) = entry_line(ui, app, &w.draft, sec, i, "") {
                                        click = Some(c);
                                    }
                                });
                            }
                        });
                },
            );
            ui.label("Step 2: Edit the rule description (click an underlined value)");
            if let Some(c) = description(ui, app, &w.draft, ("wd", uid), 100.0) {
                click = Some(c);
            }
        }
        _ => {
            ui.label(RichText::new("Finish rule setup.").strong());
            ui.label("Step 1: Specify a name for this rule");
            if w.name.is_empty() {
                w.name = w.draft.default_name(app);
            }
            ui.add(egui::TextEdit::singleline(&mut w.name).desired_width(f32::INFINITY));
            ui.label("Step 2: Setup rule options");
            let fname =
                app.s.mailbox.folder(app.s.selection.folder).map(|f| f.name.clone()).unwrap_or_default();
            ui.checkbox(&mut w.run_now, format!("Run this rule now on messages already in \"{fname}\""));
            ui.checkbox(&mut w.enabled, "Turn on this rule");
            ui.add_enabled(false, egui::Checkbox::new(&mut false, "Create this rule on all accounts"))
                .on_disabled_hover_text(
                    "Rules apply to every account unless limited with \"through the specified account\"; use Copy... to make a copy for another account.",
                );
            ui.label("Step 3: Review rule description (click an underlined value to edit)");
            click = description(ui, app, &w.draft, ("wd4", uid), 120.0);
        }
    }
    if let Some((sec, i)) = click {
        let page = match sec {
            Sec::Cond => 1,
            Sec::Act => 2,
            Sec::Exc => 3,
        };
        if w.page != 0 && w.page != 4 {
            w.page = page;
        }
        if defs(sec).get(i).is_some_and(|def| def.val != Val::None) {
            w.editing = Some((sec, i));
        }
    }
    if let Some((sec, i)) = w.editing {
        let mut done = false;
        if let Some(o) = value_editor(ui, app, uid, &mut w.draft, sec, i, &mut done) {
            out = o;
        }
        if done {
            w.editing = None;
        }
    }
    if !w.error.is_empty() {
        ui.colored_label(app.theme.danger, &w.error);
    }
    let mut finish = false;
    footer(ui, |ui| {
        if btn(ui, "Cancel") {
            out = Out::Close;
        }
        finish = primary(ui, app, true, "Finish");
        if btn_en(ui, w.page < 4, "Next >") {
            w.error = match validate_page(app, w) {
                Ok(()) => {
                    w.page += 1;
                    w.editing = None;
                    String::new()
                }
                Err(e) => e,
            };
        }
        if btn_en(ui, w.page > usize::from(w.rule_id.is_some() || w.page == 1), "< Back") {
            w.page = w.page.saturating_sub(1);
            w.editing = None;
            w.error.clear();
        }
    });
    if finish && matches!(out, Out::Keep) {
        out = finish_wizard(ctx, app, w);
    }
    out
}

fn validate_page(app: &App, w: &Wizard) -> Result<(), String> {
    match w.page {
        1 => w.draft.conditions(app, Sec::Cond).map(|_| ()),
        2 => {
            let a = w.draft.actions()?;
            if a.is_empty() { Err("Select at least one action.".into()) } else { Ok(()) }
        }
        3 => w.draft.conditions(app, Sec::Exc).map(|_| ()),
        _ => Ok(()),
    }
}

fn finish_wizard(ctx: &Context, app: &mut App, w: &mut Wizard) -> Out {
    let built = (|| -> Result<(Vec<RuleCondition>, Vec<RuleAction>, Vec<RuleCondition>), String> {
        let c = w.draft.conditions(app, Sec::Cond)?;
        let a = w.draft.actions()?;
        let e = w.draft.conditions(app, Sec::Exc)?;
        if a.is_empty() {
            return Err("Select at least one action.".into());
        }
        Ok((c, a, e))
    })();
    let (c, a, e) = match built {
        Ok(v) => v,
        Err(msg) => {
            w.error = msg;
            return Out::Keep;
        }
    };
    let name = if w.name.trim().is_empty() { w.draft.default_name(app) } else { w.name.trim().to_owned() };
    let folder = app.s.selection.folder.0;
    let res = match w.rule_id {
        Some(id) => {
            let r = app.run("rules.edit", json!({"id": id, "name": name, "conditions": c, "actions": a, "exceptions": e, "enabled": w.enabled}));
            if r.is_some() && w.run_now {
                app.run("rules.runNow", json!({"ids": [id], "folder": folder}));
            }
            r
        }
        None => app.run(
            "rules.create",
            json!({"name": name, "conditions": c, "actions": a, "exceptions": e, "enabled": w.enabled, "runNow": w.run_now}),
        ),
    };
    if res.is_none() {
        w.error = last_error(app);
        return Out::Keep;
    }
    let _ = ctx;
    if w.draft.is_client_only() || w.draft.conds.get(29).is_some_and(|s| s.on) {
        Out::close_open(
            "message",
            json!({"title": "Rules Wizard", "text": "MailCraft carries out this rule itself, so it acts only on mail that arrives while MailCraft is running."}),
        )
    } else {
        Out::Close
    }
}
