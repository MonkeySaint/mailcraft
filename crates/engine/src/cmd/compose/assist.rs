//! Writing aids around the compose body, and the Message format options that reshape text:
//!
//! - `compose.insertMention`: an @mention (Options › Mail › Send messages › "Suggest names to mention when I
//!   use the @ symbol in a message"). The typed `@query` becomes a `mailto:` link reading "@Name", and the
//!   person is added to To when no recipient field has them yet. The UI offers the names (the Auto-Complete
//!   suggestions of `compose.suggestRecipients`) only while the option is on; with it off the command is
//!   disabled.
//! - `compose.livePreview`: General › "Enable Live Preview". While the pointer rests on a gallery choice
//!   (styles, font and highlight colours, fonts, sizes, bullet and numbering libraries), the compose holds
//!   the body as that choice would leave it ([`ComposeState::preview`]); the editor paints it instead of the
//!   body until the pointer moves on (`{"clear": true}`). Nothing is applied: the command runs on a copy and
//!   the compose is put back as it was (body, selection, editor undo).
//! - [`reduce_formatting`]: Message format › "Reduce message size by removing format information not
//!   necessary to display the message", applied to the HTML of sent messages.
//! - [`remove_extra_line_breaks`]: Message format › "Remove extra line breaks in plain text messages",
//!   applied when a plain-text message is shown.

use serde_json::{Value, json};

use super::{compose_id, compose_mut, field_mailbox, has_compose, spec, valid_addr};
use crate::cmd::util::{opt_bool, opt_str, opt_u64, req_str};
use crate::compose::{
    BodySelection, ClipEntry, ComposeAttachment, ComposeState, DocPos, EditSnapshot, InlineImage,
};
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_model::{Block, BodyFormat, Doc, Inline, TextStyle};

// =======================================================================================================
// @mentions
// =======================================================================================================

/// Longest display name written into a mention (chars).
pub const MAX_MENTION_NAME_CHARS: usize = 256;
/// Most typed chars an @mention replaces (the `@query` before the caret).
pub const MAX_MENTION_QUERY_CHARS: usize = 256;

/// Why mentions are off.
pub const MENTIONS_OFF: &str = "@mentions are turned off (File › Options › Mail › Send messages)";

/// Enabled: a compose is open and Options › Mail › "Suggest names to mention…" is on.
pub fn mentions_enabled(s: &Session) -> Option<&'static str> {
    has_compose(s).or_else(|| (!s.settings.bool("mail.send.mentions")).then_some(MENTIONS_OFF))
}

/// The name a mention shows for `addr`: a contact's name, else the address's local part.
fn mention_name(s: &Session, addr: &str) -> String {
    let contact =
        s.mailbox.contacts.values().find(|c| c.emails.iter().any(|e| e.trim().eq_ignore_ascii_case(addr)));
    let name = contact
        .map(|c| {
            let full = c.full_name();
            if full.trim().is_empty() { c.file_as.clone() } else { full }
        })
        .filter(|n| !n.trim().is_empty());
    name.unwrap_or_else(|| addr.split('@').next().unwrap_or(addr).to_owned())
}

fn insert_mention(s: &mut Session, p: &Value) -> CmdResult {
    let addr = req_str(p, "address")?.trim().to_owned();
    if !valid_addr(&addr) || addr.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | ';' | '"')) {
        return Err(CmdError::Params(format!("`{addr}` is not an e-mail address")));
    }
    let given = opt_str(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned);
    let name: String = given
        .unwrap_or_else(|| mention_name(s, &addr))
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_MENTION_NAME_CHARS)
        .collect();
    let replace =
        opt_u64(p, "replace").and_then(|n| usize::try_from(n).ok()).unwrap_or(0).min(MAX_MENTION_QUERY_CHARS);
    let c = compose_mut(s, p)?;
    c.checkpoint();
    if replace > 0 && c.selection.is_collapsed() {
        // The typed "@query" (in the caret's paragraph only).
        let focus = c.selection.focus.clone();
        let from = focus.offset.saturating_sub(replace);
        c.selection = BodySelection { anchor: DocPos::new(focus.path.clone(), from), focus };
    }
    let shown = format!("@{name}");
    if c.format == BodyFormat::PlainText {
        c.insert_text(&shown);
    } else {
        c.set_link(Some(&format!("mailto:{addr}")), Some(&shown));
        let end = c.selection.end().clone();
        c.selection = BodySelection::caret(end);
        // Typing on after the mention is not part of the link.
        c.pending_style = Some(TextStyle::default());
    }
    c.mark_edited();
    let already = c.recipients().iter().any(|(_, a)| a.eq_ignore_ascii_case(&addr));
    if !already {
        let entry = field_mailbox(&mailcraft_mime::Mailbox::new(Some(&name), &addr));
        let to = c.to.trim().trim_end_matches(';').trim_end();
        c.to = if to.is_empty() { entry } else { format!("{to}; {entry}") };
    }
    c.dirty = true;
    Ok(json!({"mention": shown, "address": addr, "addedToTo": !already, "to": c.to}))
}

// =======================================================================================================
// Live Preview
// =======================================================================================================

/// Gallery commands Live Preview can show (they change only the compose body).
pub const PREVIEW_COMMANDS: [&str; 8] = [
    "format.style",
    "format.color",
    "format.highlight",
    "format.shading",
    "format.font",
    "format.fontSize",
    "format.bullets",
    "format.numbering",
];

/// Why Live Preview is off.
pub const LIVE_PREVIEW_OFF: &str = "Live Preview is turned off (File › Options › General)";

/// Whether the UI should preview gallery choices (General › "Enable Live Preview").
pub fn live_preview_on(s: &Session) -> bool {
    s.settings.bool("general.livePreview")
}

fn clear_preview(c: &mut ComposeState) -> bool {
    let had = c.preview.is_some();
    c.preview = None;
    c.preview_key = None;
    had
}

/// The parts of a compose Live Preview moves aside while a gallery command runs on it.
struct Heavy {
    attachments: Vec<ComposeAttachment>,
    inline_images: Vec<InlineImage>,
    edit_undo: Vec<EditSnapshot>,
    edit_redo: Vec<EditSnapshot>,
    clipboard: Vec<ClipEntry>,
    original_body: Option<Doc>,
}

impl Heavy {
    fn take(c: &mut ComposeState) -> Self {
        Heavy {
            attachments: std::mem::take(&mut c.attachments),
            inline_images: std::mem::take(&mut c.inline_images),
            edit_undo: std::mem::take(&mut c.edit_undo),
            edit_redo: std::mem::take(&mut c.edit_redo),
            clipboard: std::mem::take(&mut c.clipboard),
            original_body: c.original_body.take(),
        }
    }

    fn restore(self, c: &mut ComposeState) {
        c.attachments = self.attachments;
        c.inline_images = self.inline_images;
        c.edit_undo = self.edit_undo;
        c.edit_redo = self.edit_redo;
        c.clipboard = self.clipboard;
        c.original_body = self.original_body;
    }
}

fn live_preview(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let command = opt_str(p, "command").map(str::to_owned);
    let Some(command) = command.filter(|_| opt_bool(p, "clear") != Some(true)) else {
        let cleared = s.composes.get_mut(&cid).is_some_and(clear_preview);
        return Ok(json!({"compose": cid, "preview": false, "cleared": cleared}));
    };
    if !live_preview_on(s) {
        if let Some(c) = s.composes.get_mut(&cid) {
            clear_preview(c);
        }
        return Err(CmdError::Disabled(LIVE_PREVIEW_OFF.into()));
    }
    if !PREVIEW_COMMANDS.contains(&command.as_str()) {
        return Err(CmdError::Params(format!(
            "`{command}` has no Live Preview (one of {})",
            PREVIEW_COMMANDS.join(", ")
        )));
    }
    let mut params = match p.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err(CmdError::Params("`params` must be an object".into())),
    };
    if let Some(o) = params.as_object_mut() {
        o.insert("compose".into(), json!(cid));
    }
    let Some(current) = s.composes.get(&cid) else {
        return Err(CmdError::Disabled("no message is being composed".into()));
    };
    // The same choice over the same body (asked again every frame the pointer rests on it): already
    // previewed.
    let key = format!("{} {:?} {command} {params}", current.edit_rev, current.selection);
    if current.preview_key.as_deref() == Some(key.as_str()) {
        return Ok(json!({"compose": cid, "preview": true, "command": command}));
    }
    // The copy to put back leaves out what no gallery command touches and can be large (attachment and
    // picture bytes, the editor undo history, the clipboard): it is moved aside instead of copied for every
    // choice the pointer rests on.
    let Some(live) = s.composes.get_mut(&cid) else {
        return Err(CmdError::Disabled("no message is being composed".into()));
    };
    let heavy = Heavy::take(live);
    let mut saved = live.clone();
    saved.preview = None;
    saved.preview_key = None;
    let notices = s.notices.len();
    let result = s.run(&command, &params);
    let changed = s.composes.get(&cid).map(|c| c.body.clone());
    // Put everything back: only the preview stays.
    s.notices.truncate(notices);
    heavy.restore(&mut saved);
    s.composes.insert(cid, saved);
    let Some(c) = s.composes.get_mut(&cid) else {
        return Err(CmdError::Disabled("no message is being composed".into()));
    };
    match (result, changed) {
        (Ok(_), Some(body)) => {
            c.preview = Some(body);
            c.preview_key = Some(key);
            Ok(json!({"compose": cid, "preview": true, "command": command}))
        }
        (Err(e), _) => {
            clear_preview(c);
            Err(e)
        }
        (Ok(_), None) => {
            clear_preview(c);
            Err(CmdError::Failed("the message closed during the preview".into()))
        }
    }
}

// =======================================================================================================
// Message format: reduce message size
// =======================================================================================================

/// Deepest block nesting walked.
const MAX_DEPTH: usize = 64;

/// The body without format information a reader does not need to display it: character formatting that
/// repeats what the text inherits anyway (the message's base font, size and colour; a style switched off
/// where nothing switches it on) is dropped, empty runs go, and neighbouring runs that look the same are
/// merged into one (one `<span>` instead of several). The text and how it looks do not change. Headings keep
/// their sizes and weights, links their colour and underline.
pub fn reduce_formatting(doc: &Doc) -> Doc {
    let mut out = doc.clone();
    reduce_blocks(&mut out.blocks, &doc.base, 0);
    out
}

fn reduce_blocks(blocks: &mut [Block], base: &TextStyle, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let next = depth.saturating_add(1);
    for b in blocks {
        match b {
            Block::Paragraph { inlines, .. } => reduce_inlines(inlines, base, false, next),
            Block::Heading { inlines, .. } => reduce_inlines(inlines, base, true, next),
            Block::List { items, .. } => {
                for item in items {
                    reduce_blocks(item, base, next);
                }
            }
            Block::Quote { blocks } | Block::Container { blocks, .. } => reduce_blocks(blocks, base, next),
            Block::Table { rows, .. } => {
                for cell in rows.iter_mut().flatten() {
                    reduce_blocks(&mut cell.blocks, base, next);
                }
            }
            Block::Pre { .. } | Block::Rule => {}
        }
    }
}

/// Where a run sits, for what it inherits besides the base style.
#[derive(Clone, Copy)]
struct Context {
    heading: bool,
    link: bool,
}

fn reduce_inlines(inlines: &mut Vec<Inline>, base: &TextStyle, heading: bool, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for i in inlines.iter_mut() {
        match i {
            Inline::Text { style, .. } => reduce_style(style, base, Context { heading, link: false }),
            Inline::Link { children, .. } => {
                for ch in children.iter_mut() {
                    if let Inline::Text { style, .. } = ch {
                        reduce_style(style, base, Context { heading, link: true });
                    }
                }
                merge_runs(children);
            }
            Inline::LineBreak | Inline::Image { .. } => {}
        }
    }
    merge_runs(inlines);
}

fn reduce_style(st: &mut TextStyle, base: &TextStyle, cx: Context) {
    if let (Some(f), Some(b)) = (&st.font_family, &base.font_family)
        && f.trim().eq_ignore_ascii_case(b.trim())
    {
        st.font_family = None;
    }
    // Headings have their own size; links their own colour.
    if !cx.heading && st.size_pt.is_some() && st.size_pt == base.size_pt {
        st.size_pt = None;
    }
    if !cx.link && st.color.is_some() && st.color == base.color {
        st.color = None;
    }
    let off = |flag: &mut Option<bool>, inherited: Option<bool>, keep: bool| {
        if *flag == Some(false) && inherited != Some(true) && !keep {
            *flag = None;
        }
    };
    off(&mut st.bold, base.bold, cx.heading);
    off(&mut st.italic, base.italic, false);
    off(&mut st.underline, base.underline, cx.link);
    off(&mut st.strike, base.strike, false);
    off(&mut st.superscript, base.superscript, false);
    off(&mut st.subscript, base.subscript, false);
    off(&mut st.monospace, base.monospace, false);
}

/// Drops empty text runs and joins neighbours with the same style.
fn merge_runs(inlines: &mut Vec<Inline>) {
    let mut out: Vec<Inline> = Vec::with_capacity(inlines.len());
    for i in inlines.drain(..) {
        if let Inline::Text { text, style } = &i {
            if text.is_empty() {
                continue;
            }
            if let Some(Inline::Text { text: prev, style: prev_style }) = out.last_mut()
                && prev_style == style
            {
                prev.push_str(text);
                continue;
            }
        }
        out.push(i);
    }
    *inlines = out;
}

// =======================================================================================================
// Message format: remove extra line breaks
// =======================================================================================================

/// Lines this long or longer are considered when measuring the wrap width.
const MIN_WRAP: usize = 30;
/// Longest line considered a wrapped line (RFC 5322 caps lines at 998).
const MAX_WRAP: usize = 998;
/// How far below the wrap width a line may end and still count as filled to the margin.
const WRAP_SLACK: usize = 12;

/// The width a plain text was wrapped at: its longest line, when at least one other line also reaches
/// within [`WRAP_SLACK`] of it (one long line alone is no evidence of wrapping).
fn wrap_width(lines: &[&str]) -> Option<usize> {
    let lens: Vec<usize> = lines
        .iter()
        .map(|l| l.trim_end().chars().count())
        .filter(|n| (MIN_WRAP..=MAX_WRAP).contains(n))
        .collect();
    let width = lens.iter().copied().max()?;
    let near = lens.iter().filter(|&&n| n.saturating_add(WRAP_SLACK) >= width).count();
    (near >= 2).then_some(width)
}

/// Whether `line` starts something that is laid out on its own line: an indent, a list marker, a quote, a
/// signature separator or a header-like "Key: value".
fn starts_own_line(line: &str) -> bool {
    if line.starts_with([' ', '\t', '>', '|']) || line == "-- " || line.trim_end() == "--" {
        return true;
    }
    let t = line.trim_start();
    if t.starts_with(['-', '*', '•', '·', '+', '#']) && t.chars().nth(1).is_some_and(char::is_whitespace) {
        return true;
    }
    // "1." / "2)" / "a)" numbering.
    let marker: String = t.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    let after = t.chars().nth(marker.chars().count());
    if !marker.is_empty()
        && marker.chars().count() <= 3
        && (marker.chars().all(|c| c.is_ascii_digit()) || marker.chars().count() == 1)
        && matches!(after, Some('.' | ')'))
        && t.chars().nth(marker.chars().count().saturating_add(1)).is_some_and(char::is_whitespace)
    {
        return true;
    }
    header_like(t)
}

/// Longest key of a header-like "Key: value" line (chars).
const MAX_HEADER_KEY: usize = 30;

/// Whether `t` reads as a header line: a key of letters, digits and hyphens that starts with a letter (`To`,
/// `Reply-To`, `Subject`), then a colon and a space or the end of the line, as in a forwarded header block.
fn header_like(t: &str) -> bool {
    let Some((key, rest)) = t.split_once(':') else {
        return false;
    };
    let mut chars = key.chars();
    key.chars().count() <= MAX_HEADER_KEY
        && chars.next().is_some_and(char::is_alphabetic)
        && chars.all(|c| c.is_alphanumeric() || c == '-')
        && rest.chars().next().is_none_or(char::is_whitespace)
}

/// Joins the lines of a plain-text message that were broken only because the sender's program wrapped them
/// at a fixed width: a line that reaches at least half the message's wrap width is joined to the next when
/// the next line's first word would not have fitted after it (so the break was the wrapper's, not the
/// writer's). Blank lines, quoted lines (`>`), indented lines, list items, header-like "Key: value" lines
/// (a forwarded `To:` / `Cc:` / `Subject:` block), short lines ("Thanks,") and the signature separator keep
/// their breaks (`format=flowed` soft breaks are already joined by the MIME decoder). Returns `None` when
/// nothing changed.
pub fn remove_extra_line_breaks(text: &str) -> Option<String> {
    let text =
        if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text.to_owned() };
    let lines: Vec<&str> = text.split('\n').collect();
    let width = wrap_width(&lines)?;
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    for (n, line) in lines.iter().enumerate() {
        out.push_str(line);
        let Some(next) = lines.get(n.saturating_add(1)) else {
            break;
        };
        let len = line.trim_end().chars().count();
        let next_word = next.split_whitespace().next().map_or(0, |w| w.chars().count());
        let wrapped =
            len.saturating_mul(2) >= width && len.saturating_add(1).saturating_add(next_word) > width;
        let join = wrapped
            && !next.trim().is_empty()
            && !line.starts_with('>')
            && *line != "-- "
            && !starts_own_line(next);
        if join {
            out.truncate(out.trim_end().len());
            out.push(' ');
            changed = true;
        } else {
            out.push('\n');
        }
    }
    changed.then_some(out)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "compose.insertMention",
            "Mention",
            "Compose › Body (@)",
            "",
            r#"{"address":string,"name"?:string,"replace"?:number}"#,
            false,
            mentions_enabled,
            insert_mention,
        ),
        spec(
            "compose.livePreview",
            "Live Preview",
            "Compose › galleries (pointer over a choice)",
            "",
            r#"{"command"?:string,"params"?:object,"clear"?:bool}"#,
            false,
            has_compose,
            live_preview,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::tests::{open, session};
    use super::*;
    use mailcraft_geom::Color;
    use mailcraft_model::{Contact, ParaStyle};

    fn set(s: &mut Session, key: &str, v: Value) {
        s.run("options.set", &json!({"key": key, "value": v})).unwrap();
    }

    fn links(doc: &Doc) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for b in &doc.blocks {
            if let Block::Paragraph { inlines, .. } = b {
                for i in inlines {
                    if let Inline::Link { href, children } = i {
                        let text: String = children
                            .iter()
                            .filter_map(|c| match c {
                                Inline::Text { text, .. } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect();
                        out.push((href.clone(), text));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn mention_links_the_name_and_adds_the_person_to_to() {
        let mut s = session();
        let contact = mailcraft_model::ItemId(s.mailbox.alloc_id());
        s.mailbox.contacts.insert(
            contact,
            Contact {
                id: contact,
                first_name: "Ann".into(),
                last_name: "Lee".into(),
                emails: vec!["ann@example.org".into()],
                ..Default::default()
            },
        );
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "Thanks @an"})).unwrap();
        let r = s.run("compose.insertMention", &json!({"address": "ann@example.org", "replace": 3})).unwrap();
        assert_eq!(r["mention"], "@Ann Lee");
        assert_eq!(r["addedToTo"], true);
        let c = &s.composes[&id];
        assert_eq!(c.body.to_plain_text(), "Thanks @Ann Lee");
        assert_eq!(links(&c.body), [("mailto:ann@example.org".to_owned(), "@Ann Lee".to_owned())]);
        assert_eq!(c.to, "Ann Lee <ann@example.org>");
        // Typing on is not part of the link.
        s.run("compose.insertText", &json!({"text": " for this"})).unwrap();
        let c = &s.composes[&id];
        assert_eq!(links(&c.body)[0].1, "@Ann Lee");
        assert_eq!(c.body.to_plain_text(), "Thanks @Ann Lee for this");
        // Mentioned again, or already a recipient: To is left alone.
        s.run("compose.setCc", &json!({"value": "bo@example.net"})).unwrap();
        let r = s.run("compose.insertMention", &json!({"address": "BO@example.net", "name": "Bo"})).unwrap();
        assert_eq!(r["addedToTo"], false);
        let r = s.run("compose.insertMention", &json!({"address": "ann@example.org"})).unwrap();
        assert_eq!(r["addedToTo"], false);
        assert_eq!(s.composes[&id].to, "Ann Lee <ann@example.org>");
        // A third person is appended to To; undo takes the mention out of the body.
        s.run("compose.insertMention", &json!({"address": "cy@example.com"})).unwrap();
        assert_eq!(s.composes[&id].to, "Ann Lee <ann@example.org>; cy <cy@example.com>");
        s.run("compose.editUndo", &json!({})).unwrap();
        assert!(!s.composes[&id].body.to_plain_text().contains("@cy"));
        // Hostile input is refused.
        for bad in ["not an address", "a@b>; x", ""] {
            assert!(s.run("compose.insertMention", &json!({"address": bad})).is_err(), "{bad}");
        }
        // A replace count past the paragraph start stops at the start.
        s.run("compose.insertMention", &json!({"address": "dee@example.com", "replace": 100_000})).unwrap();
    }

    #[test]
    fn mentions_follow_the_option() {
        let mut s = session();
        let id = open(&mut s);
        set(&mut s, "mail.send.mentions", json!(false));
        let r = s.run("compose.insertMention", &json!({"address": "ann@example.org"}));
        assert!(matches!(r, Err(CmdError::Disabled(ref why)) if why == MENTIONS_OFF), "{r:?}");
        assert!(s.composes[&id].to.is_empty());
        set(&mut s, "mail.send.mentions", json!(true));
        assert!(s.run("compose.insertMention", &json!({"address": "ann@example.org"})).is_ok());
        assert_eq!(s.composes[&id].to, "ann <ann@example.org>");
    }

    #[test]
    fn mention_in_plain_text_is_text() {
        let mut s = session();
        let id = open(&mut s);
        s.composes.get_mut(&id).unwrap().format = BodyFormat::PlainText;
        s.run("compose.insertText", &json!({"text": "@a"})).unwrap();
        s.run("compose.insertMention", &json!({"address": "al@example.com", "name": "Al", "replace": 2}))
            .unwrap();
        let c = &s.composes[&id];
        assert_eq!(c.body.to_plain_text(), "@Al");
        assert!(links(&c.body).is_empty());
    }

    #[test]
    fn live_preview_shows_a_choice_without_applying_it() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "Preview me"})).unwrap();
        s.run("compose.select", &json!({"all": true})).unwrap();
        let before = s.composes[&id].clone();
        let r = s
            .run("compose.livePreview", &json!({"command": "format.color", "params": {"color": "#c00000"}}))
            .unwrap();
        assert_eq!(r["preview"], true);
        let c = &s.composes[&id];
        assert_eq!(c.body, before.body, "nothing applied");
        assert_eq!(c.selection, before.selection);
        assert_eq!(c.edit_undo.len(), before.edit_undo.len(), "no undo step");
        let preview = c.preview.clone().unwrap();
        assert_ne!(preview, c.body);
        // Pointer off the gallery.
        let r = s.run("compose.livePreview", &json!({"clear": true})).unwrap();
        assert_eq!(r["cleared"], true);
        assert!(s.composes[&id].preview.is_none());
        // Only gallery commands, with object params.
        for bad in [json!({"command": "compose.send"}), json!({"command": "format.color", "params": 3})] {
            assert!(matches!(s.run("compose.livePreview", &bad), Err(CmdError::Params(_))), "{bad}");
        }
        // A failing choice previews nothing.
        assert!(
            s.run("compose.livePreview", &json!({"command": "format.style", "params": {"name": " "}}))
                .is_err()
        );
        assert!(s.composes[&id].preview.is_none());
        assert_eq!(s.composes[&id].body, before.body);
    }

    #[test]
    fn live_preview_follows_the_option() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "Hi"})).unwrap();
        s.run("compose.select", &json!({"all": true})).unwrap();
        let p = json!({"command": "format.highlight", "params": {"color": "#ffff00"}});
        s.run("compose.livePreview", &p).unwrap();
        assert!(s.composes[&id].preview.is_some());
        set(&mut s, "general.livePreview", json!(false));
        assert!(!live_preview_on(&s));
        let r = s.run("compose.livePreview", &p);
        assert!(matches!(r, Err(CmdError::Disabled(ref why)) if why == LIVE_PREVIEW_OFF), "{r:?}");
        assert!(s.composes[&id].preview.is_none(), "turning it off drops the preview");
    }

    #[test]
    fn live_preview_puts_attachments_pictures_and_undo_back_untouched() {
        let mut s = session();
        let id = open(&mut s);
        for word in ["one ", "two ", "three"] {
            s.run("compose.insertText", &json!({"text": word})).unwrap();
        }
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("format.bold", &json!({})).unwrap();
        s.run("compose.editUndo", &json!({})).unwrap();
        {
            let c = s.composes.get_mut(&id).unwrap();
            c.attachments.push(ComposeAttachment {
                filename: "plan.pdf".into(),
                mime_type: "application/pdf".into(),
                data: vec![7; 4096],
            });
            c.inline_images.push(InlineImage {
                content_id: "pic1@example.com".into(),
                filename: "pic.png".into(),
                mime_type: "image/png".into(),
                data: vec![9; 2048],
            });
            c.original_body = Some(Doc::from_plain_text("before translation"));
        }
        let before = s.composes[&id].clone();
        assert!(!before.edit_undo.is_empty() && !before.edit_redo.is_empty(), "the test needs history");
        for color in ["#c00000", "#00b050", "#0070c0"] {
            let p = json!({"command": "format.color", "params": {"color": color}});
            s.run("compose.livePreview", &p).unwrap();
            let mut c = s.composes[&id].clone();
            assert!(c.preview.is_some());
            c.preview = None;
            c.preview_key = None;
            assert_eq!(c, before, "only the preview differs");
        }
        // Undo and Redo still step through the history the compose had.
        s.run("compose.livePreview", &json!({"clear": true})).unwrap();
        s.run("compose.editRedo", &json!({})).unwrap();
        assert!(s.composes[&id].edit_redo.len() < before.edit_redo.len());
    }

    fn run(text: &str, style: TextStyle) -> Inline {
        Inline::Text { text: text.into(), style }
    }

    #[test]
    fn reduce_formatting_drops_what_is_inherited() {
        let base = TextStyle { font_family: Some("Aptos".into()), size_pt: Some(12.0), ..Default::default() };
        let same = TextStyle {
            font_family: Some("aptos".into()),
            size_pt: Some(12.0),
            bold: Some(false),
            ..Default::default()
        };
        let red = TextStyle { color: Some(Color::rgb(200, 0, 0)), ..same.clone() };
        let doc = Doc {
            blocks: vec![
                Block::Paragraph {
                    style: ParaStyle::default(),
                    inlines: vec![
                        run("One ", same.clone()),
                        run("", red.clone()),
                        run("two ", same.clone()),
                        run("red", red.clone()),
                        Inline::Link {
                            href: "https://example.com".into(),
                            children: vec![run("link", TextStyle { underline: Some(false), ..same.clone() })],
                        },
                    ],
                },
                Block::Heading {
                    level: 1,
                    style: ParaStyle::default(),
                    inlines: vec![run("Plain heading", same.clone())],
                },
            ],
            base,
            background: None,
        };
        let small = reduce_formatting(&doc);
        assert_eq!(small.to_plain_text(), doc.to_plain_text());
        let Block::Paragraph { inlines, .. } = &small.blocks[0] else { panic!() };
        assert_eq!(inlines.len(), 3, "{inlines:?}");
        assert_eq!(inlines[0], run("One two ", TextStyle::default()));
        assert_eq!(
            inlines[1],
            run("red", TextStyle { color: Some(Color::rgb(200, 0, 0)), ..Default::default() })
        );
        // A link keeps "no underline"; a heading keeps its size and "not bold".
        assert!(format!("{:?}", inlines[2]).contains("underline: Some(false)"));
        let Block::Heading { inlines, .. } = &small.blocks[1] else { panic!() };
        assert_eq!(
            inlines[0],
            run("Plain heading", TextStyle { size_pt: Some(12.0), bold: Some(false), ..Default::default() })
        );
        let (a, b) = (mailcraft_html::doc_to_html(&doc), mailcraft_html::doc_to_html(&small));
        assert!(b.len() < a.len(), "{b}\n{a}");
        assert!(b.matches("<span").count() < a.matches("<span").count());
    }

    #[test]
    fn reduce_message_size_option_changes_the_sent_html() {
        let mut s = session();
        let id = open(&mut s);
        let c = s.composes.get_mut(&id).unwrap();
        c.body = Doc {
            blocks: vec![Block::Paragraph {
                style: ParaStyle::default(),
                inlines: ["a", "b", "c"]
                    .iter()
                    .map(|t| run(t, TextStyle { font_family: Some("Aptos".into()), ..Default::default() }))
                    .collect(),
            }],
            base: TextStyle { font_family: Some("Aptos".into()), ..Default::default() },
            background: None,
        };
        let html = |s: &Session| {
            super::super::build_message(s, &s.composes[&id], None, false).html.unwrap_or_default()
        };
        let reduced = html(&s);
        assert_eq!(reduced.matches("<span").count(), 0, "{reduced}");
        assert!(reduced.contains(">abc</p>"), "{reduced}");
        set(&mut s, "mail.format.reduceSize", json!(false));
        let full = html(&s);
        assert_eq!(full.matches("<span").count(), 3, "{full}");
        assert!(full.len() > reduced.len());
    }

    #[test]
    fn extra_line_breaks_in_wrapped_text_are_removed() {
        let wrapped = "This is a long paragraph that the sender's program wrapped at a\n\
                       fixed width, so the reader sees ragged lines on a wide screen\n\
                       that end in the middle of a sentence.\n\
                       \n\
                       Thanks,\n\
                       Ann\n\
                       -- \n\
                       Ann Lee\n\
                       > quoted text that was wrapped by the original sender at some\n\
                       > fixed width stays as it is\n\
                       - first item that is long enough to have been wrapped by a mailer\n\
                       - second item";
        let out = remove_extra_line_breaks(wrapped).unwrap();
        assert_eq!(
            out,
            "This is a long paragraph that the sender's program wrapped at a fixed width, so the reader sees \
             ragged lines on a wide screen that end in the middle of a sentence.\n\
             \n\
             Thanks,\n\
             Ann\n\
             -- \n\
             Ann Lee\n\
             > quoted text that was wrapped by the original sender at some\n\
             > fixed width stays as it is\n\
             - first item that is long enough to have been wrapped by a mailer\n\
             - second item"
        );
        // One long line is no evidence of wrapping.
        assert_eq!(remove_extra_line_breaks("A line that is long enough to measure a wrap width\nOK"), None);
        // A break the next word would have fitted before was the writer's own.
        let own = "A line that is long enough to measure a wrap width\n\
                   and a second line that is nearly as long\n\
                   Short line\n\
                   Then this one";
        assert_eq!(
            remove_extra_line_breaks(own).as_deref(),
            Some(
                "A line that is long enough to measure a wrap width and a second line that is nearly as long\n\
                 Short line\nThen this one"
            )
        );
        assert_eq!(
            remove_extra_line_breaks(
                "CRLF line that is long enough to be a wrapped\r\nline and this one is nearly as long too\r\nend"
            )
            .as_deref(),
            Some("CRLF line that is long enough to be a wrapped line and this one is nearly as long too\nend")
        );
        for junk in ["", "\n\n\n", "x", "@\n@", "１２３\n\u{0}"] {
            let _ = remove_extra_line_breaks(junk);
        }
        // Numbered lists keep their breaks.
        assert!(starts_own_line("1. one") && starts_own_line("a) one") && !starts_own_line("1.5 litres"));
    }

    #[test]
    fn forwarded_header_lines_keep_their_breaks() {
        // A forwarded header block in a message wrapped at 72: the long To line ends close enough to the
        // margin that "Cc:" would not have fitted after it, but each header is its own line.
        let forwarded = "From: Ann Lee <ann@example.org> on behalf of the planning group for the\n\
                         spring offsite, who asked me to pass the notes along to everyone here\n\
                         To: Ann <ann@example.org>; Bo <bo@example.net>; Cy <cy@example.com>;\n\
                         Cc: Dee <dee@example.com>\n\
                         Subject: Notes from the planning meeting about the spring offsite\n\
                         Reply-To: planning@example.org\n\
                         Sent: Monday";
        let out = remove_extra_line_breaks(forwarded).unwrap();
        assert_eq!(
            out,
            "From: Ann Lee <ann@example.org> on behalf of the planning group for the spring offsite, who \
             asked me to pass the notes along to everyone here\n\
             To: Ann <ann@example.org>; Bo <bo@example.net>; Cy <cy@example.com>;\n\
             Cc: Dee <dee@example.com>\n\
             Subject: Notes from the planning meeting about the spring offsite\n\
             Reply-To: planning@example.org\n\
             Sent: Monday"
        );
        assert!(header_like("To: x") && header_like("Reply-To:") && header_like("X-Mailer: y"));
        for not in ["note:x", "10:30 tomorrow", "-x: y", "a key with spaces: y", "no colon here", ":"] {
            assert!(!header_like(not), "{not}");
        }
        assert!(!header_like(&format!("{}: y", "k".repeat(MAX_HEADER_KEY + 1))));
    }
}
