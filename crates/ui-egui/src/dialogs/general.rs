//! Small and informational dialogs: About, Help, What's New, Feedback, Contact Support, generic message,
//! Exit, Save Changes, View Source, Reveal Path, search tools, Reminders, Send/Receive progress, Recall,
//! Ribbon Display Options, Quick Access Toolbar, Manage / Edit Quick Steps and first-use setup.

use egui::{Context, RichText, Sense, Vec2};
use serde_json::{Value, json};

use super::common::{
    self, Dlg, Out, btn, btn_en, child, folder_path, footer, muted, ok_cancel, primary, take_replies, window,
    with_state,
};
use super::folders::last_error;
use crate::App;
use mailcraft_model::{FlagState, FolderId, Importance, ItemId, QuickStepAction};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "about" => about(ctx, app, d),
        "help" => help(ctx, app, d),
        "whatsNew" => whats_new(ctx, app, d),
        "feedback" | "contactSupport" => feedback(ctx, app, d),
        "message" | "notAvailable" => message(ctx, app, d),
        "exit" => {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            Out::Close
        }
        "saveChanges" => save_changes(ctx, app, d),
        "viewSource" => view_source(ctx, app, d),
        "revealPath" => reveal_path(ctx, app, d),
        "searchBox" => {
            app.ui.search_active = true;
            Out::Close
        }
        "searchTools" => search_tools(ctx, app, d),
        "indexingStatus" => indexing(ctx, app, d),
        "reminders" => reminders(ctx, app, d),
        "recallMessage" => recall(ctx, app, d),
        "ribbonDisplayOptions" => ribbon_display(ctx, app, d),
        "quickAccess" => quick_access(ctx, app, d),
        "manageQuickSteps" => manage_quick_steps(ctx, app, d),
        "editQuickStep" => edit_quick_step(ctx, app, d),
        "quickStepFirstUse" => quick_step_first_use(ctx, app, d),
        _ => return None,
    })
}

/// Generic box for dialog ids without a renderer.
pub(crate) fn not_available(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    window(ctx, app, d, "MailCraft", 380.0, |ui, app| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
            app.icons.paint(ui, r, "info", app.theme.accent);
            ui.label(format!("This window ({}) is not available yet.", d.name));
        });
        footer(ui, |ui| if primary(ui, app, true, "OK") { Out::Close } else { Out::Keep })
    })
}

fn ok_only(ui: &mut egui::Ui, app: &App) -> Out {
    footer(ui, |ui| if primary(ui, app, true, "OK") { Out::Close } else { Out::Keep })
}

fn message(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let title = if d.str("title").is_empty() { "MailCraft".to_owned() } else { d.str("title") };
    window(ctx, app, d, &title, 400.0, |ui, app| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
            app.icons.paint(ui, r, "info", app.theme.accent);
            ui.label(if d.str("text").is_empty() {
                "This is not available yet.".to_owned()
            } else {
                d.str("text")
            });
        });
        ok_only(ui, app)
    })
}

fn about(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    window(ctx, app, d, "About MailCraft", 420.0, |ui, app| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
            app.icons.paint(ui, r, "mail", app.theme.accent);
            ui.vertical(|ui| {
                ui.label(RichText::new("MailCraft").size(20.0).strong());
                let v = if d.str("version").is_empty() {
                    mailcraft_engine::cmd::help::VERSION.to_owned()
                } else {
                    d.str("version")
                };
                ui.label(format!("Version {v}"));
            });
        });
        ui.add_space(6.0);
        ui.label("A clean-room, open-source desktop mail client.");
        if !d.str("license").is_empty() {
            muted(ui, app, &format!("License: {}", d.str("license")));
        }
        if !d.str("os").is_empty() {
            muted(ui, app, &format!("Running on {} ({})", d.str("os"), d.str("arch")));
        }
        muted(ui, app, &format!("{} commands available.", app.s.registry.len()));
        ok_only(ui, app)
    })
}

/// Built-in help topics (our own text).
const TOPICS: [(&str, &str, &str); 7] = [
    (
        "getting-started",
        "Getting started",
        "MailCraft keeps your mail, calendar, people, tasks and notes in one window. Pick a module on the left rail, a folder in the folder pane, and a message in the list; it opens in the reading pane. Everything on the ribbon is also available from the keyboard.",
    ),
    (
        "reading",
        "Reading mail",
        "Select a message to read it in the reading pane, or double-click to open it in its own window. Use the View tab to move the reading pane, switch to conversations, or change how the list is arranged.",
    ),
    (
        "writing",
        "Writing mail",
        "Choose New Email (Ctrl+Shift+M) to write a message. Reply, Reply All and Forward start a response in the reading pane; Pop Out moves it to its own window. Drafts are saved automatically.",
    ),
    (
        "rules",
        "Organizing with rules",
        "Rules act on messages as they arrive: move them to folders, flag them, assign categories and more. Create one from a message (Rules › Create Rule) or manage them all in Rules and Alerts.",
    ),
    (
        "search",
        "Searching",
        "Type in the search box at the top to search the current folder. Use the Search tab to widen the scope or refine by sender, subject, attachments and more. Advanced Find (Ctrl+Shift+F) searches with several criteria at once.",
    ),
    ("shortcuts", "Keyboard shortcuts", ""),
    (
        "data",
        "Your data",
        "MailCraft stores your mailbox in a local data file. Use File › Open & Export to import EML or MBOX files and to export a folder to MBOX.",
    ),
];

#[derive(Clone, Default)]
struct HelpState {
    topic: usize,
    filter: String,
}

fn help(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let want = d.str("topic");
    let init = |_: &App| HelpState {
        topic: TOPICS.iter().position(|(k, _, _)| *k == want).unwrap_or(0),
        filter: String::new(),
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "MailCraft Help", 640.0, |ui, app| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    for (i, (_, title, _)) in TOPICS.iter().enumerate() {
                        if ui.selectable_label(st.topic == i, *title).clicked() {
                            st.topic = i;
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    let Some((key, title, text)) = TOPICS.get(st.topic) else { return };
                    ui.label(RichText::new(*title).size(16.0).strong());
                    if *key == "shortcuts" {
                        ui.add(
                            egui::TextEdit::singleline(&mut st.filter)
                                .hint_text("Filter")
                                .desired_width(220.0),
                        );
                        egui::ScrollArea::vertical().id_salt(("helpkeys", uid)).max_height(300.0).show(
                            ui,
                            |ui| {
                                egui::Grid::new(("keys", uid)).striped(true).num_columns(2).show(ui, |ui| {
                                    let f = st.filter.to_lowercase();
                                    for spec in &app.s.registry.specs {
                                        if spec.shortcut.is_empty()
                                            || (!f.is_empty() && !spec.label.to_lowercase().contains(&f))
                                        {
                                            continue;
                                        }
                                        ui.label(spec.label);
                                        ui.label(
                                            RichText::new(spec.shortcut.replace("Mod", "Ctrl")).monospace(),
                                        );
                                        ui.end_row();
                                    }
                                });
                            },
                        );
                    } else {
                        ui.label(*text);
                    }
                });
            });
            footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
        })
    })
}

fn whats_new(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    window(ctx, app, d, "What's New", 440.0, |ui, app| {
        ui.label(RichText::new(format!("MailCraft {}", d.str("version"))).strong());
        for line in [
            "Outlook-style shell: ribbon, folder pane, message list and reading pane.",
            "Rules: Create Rule, Rules and Alerts and the four-step Rules Wizard.",
            "Import and export EML and MBOX from File › Open & Export.",
            "Light and dark themes (File › Account).",
        ] {
            ui.label(format!("•  {line}"));
        }
        ok_only(ui, app)
    })
}

#[derive(Clone, Default)]
struct FeedbackState {
    text: String,
    info: bool,
    copied: bool,
}

fn feedback(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let support = d.name == "contactSupport";
    with_state(
        ctx,
        app,
        uid,
        |_: &App| FeedbackState { info: true, ..Default::default() },
        |st, app| {
            let title = if support {
                "Contact Support"
            } else if d.str("kind") == "suggestion" {
                "Send a Suggestion"
            } else {
                "Feedback"
            };
            window(ctx, app, d, title, 460.0, |ui, app| {
                muted(
                    ui,
                    app,
                    "MailCraft has no feedback service of its own. Write your note here, copy it and send it to the project's issue tracker.",
                );
                ui.add(egui::TextEdit::multiline(&mut st.text).desired_rows(6).desired_width(f32::INFINITY));
                ui.checkbox(&mut st.info, "Include version and system information");
                let about = d.get("about").cloned().unwrap_or(Value::Null);
                if st.info {
                    muted(ui, app, &about_line(&about));
                }
                if st.copied {
                    muted(ui, app, "Copied to the clipboard.");
                }
                footer(ui, |ui| {
                    if btn(ui, "Close") {
                        return Out::Close;
                    }
                    if btn(ui, "Copy") {
                        let mut t = st.text.clone();
                        if st.info {
                            t.push_str("\n\n");
                            t.push_str(&about_line(&about));
                        }
                        ui.ctx().copy_text(t);
                        st.copied = true;
                    }
                    Out::Keep
                })
            })
        },
    )
}

fn about_line(about: &Value) -> String {
    let g = |k: &str| about.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    format!("MailCraft {} on {} ({})", g("version"), g("os"), g("arch"))
}

fn save_changes(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let compose = d.u64("compose");
    let many: Vec<u64> = d.arr("composes").iter().filter_map(Value::as_u64).collect();
    let subject = compose
        .and_then(|c| app.s.composes.get(&c))
        .map(|c| if c.subject.trim().is_empty() { "Untitled".to_owned() } else { c.subject.clone() })
        .unwrap_or_default();
    window(ctx, app, d, "MailCraft", 400.0, |ui, app| {
        if compose.is_some() {
            ui.label(format!("Do you want to save changes to \"{subject}\"?"));
        } else {
            ui.label(format!("You have {} unsaved message(s). Save them as drafts?", many.len().max(1)));
        }
        footer(ui, |ui| {
            if btn(ui, "Cancel") {
                return Out::Close;
            }
            let answer = if btn(ui, "No") {
                Some(false)
            } else if primary(ui, app, true, "Yes") {
                Some(true)
            } else {
                None
            };
            match answer {
                Some(save) => {
                    match compose {
                        Some(c) => app.run("compose.close", json!({"compose": c, "save": save})),
                        None => app.run("app.closeAllItems", json!({"save": save})),
                    };
                    Out::Close
                }
                None => Out::Keep,
            }
        })
    })
}

fn view_source(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let mut text = d.str("source");
    window(ctx, app, d, "Message Source", 700.0, |ui, _app| {
        egui::ScrollArea::both().id_salt(("src", uid)).max_height(420.0).show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut text)
                    .code_editor()
                    .interactive(true)
                    .desired_width(f32::INFINITY),
            );
        });
        footer(ui, |ui| {
            if btn(ui, "Close") {
                return Out::Close;
            }
            if btn(ui, "Copy All") {
                ui.ctx().copy_text(d.str("source"));
            }
            Out::Keep
        })
    })
}

fn reveal_path(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let path = d.str("path");
    window(ctx, app, d, "File Location", 460.0, |ui, app| {
        ui.label("The data file is stored in:");
        ui.label(RichText::new(&path).monospace());
        footer(ui, |ui| {
            if primary(ui, app, true, "OK") {
                return Out::Close;
            }
            if btn(ui, "Copy Path") {
                ui.ctx().copy_text(path.clone());
            }
            if btn(ui, "Open Folder") {
                ui.ctx().open_url(egui::OpenUrl::new_tab(format!("file://{path}")));
            }
            Out::Keep
        })
    })
}

fn search_tools(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    window(ctx, app, d, "Search Tools", 320.0, |ui, app| {
        let mut out = Out::Keep;
        if ui.button("Indexing Status...").clicked() {
            app.run("search.searchTools", json!({"tool": "indexingStatus"}));
            out = Out::Close;
        }
        if ui.button("Search Options...").clicked() {
            app.run("options.open", json!({"page": "search"}));
            out = Out::Close;
        }
        if ui.button("Advanced Find...").clicked() {
            app.run("search.advancedFind", json!({}));
            out = Out::Close;
        }
        out.or(footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep }))
    })
}

fn indexing(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    window(ctx, app, d, "Indexing Status", 380.0, |ui, app| {
        let n = d.u64("remaining").unwrap_or(0);
        if n == 0 {
            ui.label("All items are indexed.");
        } else {
            ui.label(format!("{n} items remain to be indexed."));
        }
        muted(ui, app, &format!("{} messages searchable.", app.s.mailbox.mail.len()));
        ok_only(ui, app)
    })
}

#[derive(Clone, Default)]
struct RemState {
    selected: Option<u64>,
    snooze: usize,
}

const SNOOZE: [(&str, i64); 6] = [
    ("5 minutes", 300),
    ("10 minutes", 600),
    ("15 minutes", 900),
    ("1 hour", 3600),
    ("4 hours", 14_400),
    ("1 day", 86_400),
];

fn reminders(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    with_state(
        ctx,
        app,
        uid,
        |_: &App| RemState { snooze: 0, ..Default::default() },
        |st, app| {
            let now = app.s.now().unix;
            let mut due: Vec<(u64, String, i64)> = app
                .s
                .mailbox
                .mail
                .values()
                .filter_map(|m| match &m.flag {
                    FlagState::Flagged { reminder: Some(r), .. } => Some((m.id.0, m.subject.clone(), r.unix)),
                    _ => None,
                })
                .collect();
            due.sort_by_key(|x| x.2);
            let title = format!("{} Reminder(s)", due.len());
            window(ctx, app, d, &title, 480.0, |ui, app| {
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, app.theme.separator))
                    .inner_margin(4.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt(("rem", uid))
                            .max_height(180.0)
                            .min_scrolled_height(180.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if due.is_empty() {
                                    muted(ui, app, "No reminders.");
                                }
                                for (id, subject, at) in &due {
                                    ui.horizontal(|ui| {
                                        let (r, _) =
                                            ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                                        app.icons.paint(ui, r, "bell", app.theme.flag);
                                        if ui
                                            .selectable_label(
                                                st.selected == Some(*id),
                                                if subject.is_empty() { "(no subject)" } else { subject },
                                            )
                                            .clicked()
                                        {
                                            st.selected = Some(*id);
                                        }
                                        let mins = (now - at) / 60;
                                        let when = if mins >= 0 {
                                            format!("{mins} min overdue")
                                        } else {
                                            format!("in {} min", -mins)
                                        };
                                        ui.label(RichText::new(when).color(app.theme.text_muted));
                                    });
                                }
                            });
                    });
                let dismiss = |app: &mut App, ids: Vec<u64>| {
                    for id in ids {
                        if let Some(FlagState::Flagged { label, start, due, .. }) =
                            app.s.mailbox.mail.get(&ItemId(id)).map(|m| m.flag.clone())
                        {
                            let mut p = json!({"ids": [id], "label": label});
                            if let Some(o) = p.as_object_mut() {
                                if let Some(s) = start {
                                    o.insert("start".into(), json!(s.unix));
                                }
                                if let Some(s) = due {
                                    o.insert("due".into(), json!(s.unix));
                                }
                            }
                            app.run("mail.flagCustom", p);
                        }
                    }
                };
                ui.horizontal(|ui| {
                    if btn_en(ui, !due.is_empty(), "Dismiss All") {
                        dismiss(app, due.iter().map(|x| x.0).collect());
                    }
                    if btn_en(ui, st.selected.is_some(), "Open Item")
                        && let Some(id) = st.selected
                    {
                        app.run("mail.open", json!({"ids": [id]}));
                    }
                    if btn_en(ui, st.selected.is_some(), "Dismiss") {
                        dismiss(app, st.selected.into_iter().collect());
                        st.selected = None;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Click Snooze to be reminded again in:");
                });
                ui.horizontal(|ui| {
                    let names: Vec<&str> = SNOOZE.iter().map(|x| x.0).collect();
                    common::combo_idx(ui, ("snooze", uid), &mut st.snooze, &names, 160.0);
                    if btn_en(ui, st.selected.is_some(), "Snooze") {
                        let secs = SNOOZE.get(st.snooze).map_or(300, |x| x.1);
                        if let Some(id) = st.selected {
                            app.run("mail.addReminder", json!({"ids": [id], "at": now + secs}));
                        }
                    }
                });
                footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
            })
        },
    )
}

#[derive(Clone, Default)]
struct RecallState {
    replace: bool,
    tell: bool,
}

fn recall(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    with_state(
        ctx,
        app,
        uid,
        |_: &App| RecallState { replace: false, tell: true },
        |st, app| {
            window(ctx, app, d, "Recall This Message", 420.0, |ui, app| {
                muted(ui, app, "Recipients may already have read the message.");
                ui.radio_value(&mut st.replace, false, "Delete unread copies of this message");
                ui.radio_value(&mut st.replace, true, "Delete unread copies and replace with a new message");
                ui.checkbox(&mut st.tell, "Tell me if recall succeeds or fails for each recipient");
                match ok_cancel(ui, app, true) {
                    Some(true) => {
                        if st.replace {
                            app.run("mail.resend", json!({"ids": [d.u64("id")]}));
                            Out::Close
                        } else {
                            Out::close_open(
                                "message",
                                json!({"title": "Recall This Message", "text": "Recall needs a server that supports it. The message stays with its recipients."}),
                            )
                        }
                    }
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

#[derive(Clone, Default)]
struct RibbonState {
    display: String,
    style: String,
}

fn ribbon_display(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |_: &App| RibbonState { display: d.str("display"), style: d.str("style") };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Ribbon Display Options", 360.0, |ui, app| {
            common::heading(ui, app, "Show ribbon");
            ui.radio_value(&mut st.display, "autoHide".to_owned(), "Full-screen mode");
            ui.radio_value(&mut st.display, "tabsOnly".to_owned(), "Show tabs only");
            ui.radio_value(&mut st.display, "always".to_owned(), "Always show Ribbon");
            common::heading(ui, app, "Ribbon layout");
            ui.radio_value(&mut st.style, "simplified".to_owned(), "Simplified Ribbon");
            ui.radio_value(&mut st.style, "classic".to_owned(), "Classic Ribbon");
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    if st.style != d.str("style") && !st.style.is_empty() {
                        app.run("app.toggleRibbonMode", json!({"mode": st.style}));
                    }
                    if st.display != d.str("display") && !st.display.is_empty() {
                        app.run("app.toggleRibbonMode", json!({"mode": st.display}));
                    }
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

#[derive(Clone, Default)]
struct QatState {
    filter: String,
    pick: Option<String>,
    current: Option<String>,
}

fn quick_access(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    with_state(
        ctx,
        app,
        uid,
        |_: &App| QatState::default(),
        |st, app| {
            window(ctx, app, d, "Customize Quick Access Toolbar", 620.0, |ui, app| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(270.0);
                        ui.label("Choose commands from:");
                        ui.add(
                            egui::TextEdit::singleline(&mut st.filter)
                                .hint_text("Filter commands")
                                .desired_width(260.0),
                        );
                        egui::ScrollArea::vertical()
                            .id_salt(("qatall", uid))
                            .max_height(260.0)
                            .min_scrolled_height(260.0)
                            .show(ui, |ui| {
                                let f = st.filter.to_lowercase();
                                for spec in &app.s.registry.specs {
                                    if !f.is_empty()
                                        && !spec.label.to_lowercase().contains(&f)
                                        && !spec.id.contains(&f)
                                    {
                                        continue;
                                    }
                                    let sel = st.pick.as_deref() == Some(spec.id);
                                    if ui
                                        .selectable_label(sel, format!("{}  ({})", spec.label, spec.id))
                                        .clicked()
                                    {
                                        st.pick = Some(spec.id.to_owned());
                                    }
                                }
                            });
                    });
                    ui.vertical(|ui| {
                        ui.add_space(80.0);
                        if btn_en(ui, st.pick.is_some(), "Add >>")
                            && let Some(id) = &st.pick
                        {
                            app.run("app.quickAccess", json!({"add": id}));
                        }
                        if btn_en(ui, st.current.is_some(), "<< Remove")
                            && let Some(id) = st.current.take()
                        {
                            app.run("app.quickAccess", json!({"remove": id}));
                        }
                    });
                    ui.vertical(|ui| {
                        ui.set_width(220.0);
                        ui.label("Customize Quick Access Toolbar:");
                        egui::Frame::new()
                            .stroke(egui::Stroke::new(1.0, app.theme.separator))
                            .inner_margin(4.0)
                            .show(ui, |ui| {
                                ui.set_min_height(260.0);
                                let items = app.s.view.quick_access.clone();
                                for id in items {
                                    let label =
                                        app.s.registry.get(&id).map_or(id.clone(), |s| s.label.to_owned());
                                    if ui
                                        .selectable_label(st.current.as_deref() == Some(id.as_str()), label)
                                        .clicked()
                                    {
                                        st.current = Some(id.clone());
                                    }
                                }
                            });
                    });
                });
                footer(ui, |ui| if primary(ui, app, true, "OK") { Out::Close } else { Out::Keep })
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Quick steps

fn qs_action_text(app: &App, a: &QuickStepAction) -> String {
    let folder = |f: &Option<FolderId>| {
        f.and_then(|f| app.s.mailbox.folder(f)).map_or("(choose on first use)".to_owned(), |f| f.name.clone())
    };
    match a {
        QuickStepAction::MoveTo(f) => format!("Move to folder: {}", folder(f)),
        QuickStepAction::CopyTo(f) => format!("Copy to folder: {}", folder(f)),
        QuickStepAction::Delete => "Delete message".into(),
        QuickStepAction::PermanentlyDelete => "Permanently delete message".into(),
        QuickStepAction::MarkRead => "Mark as read".into(),
        QuickStepAction::MarkUnread => "Mark as unread".into(),
        QuickStepAction::SetImportance(i) => format!("Set importance: {i:?}"),
        QuickStepAction::Categorize(c) => format!("Categorize: {c}"),
        QuickStepAction::ClearCategories => "Clear categories".into(),
        QuickStepAction::Flag(k) => format!("Flag message: {k}"),
        QuickStepAction::ClearFlag => "Clear flag".into(),
        QuickStepAction::MarkComplete => "Mark complete".into(),
        QuickStepAction::NewMessageTo(to) => format!("New message to: {}", to.join("; ")),
        QuickStepAction::Forward(to) => format!("Forward to: {}", to.join("; ")),
        QuickStepAction::Reply => "Reply".into(),
        QuickStepAction::ReplyAll => "Reply All".into(),
        QuickStepAction::NewMeeting => "New meeting".into(),
    }
}

#[derive(Clone, Default)]
struct ManageQs {
    selected: Option<u64>,
}

const QS_TEMPLATES: [(&str, &str); 7] = [
    ("moveToFolder", "Move to Folder"),
    ("categorizeMove", "Categorize & Move"),
    ("flagMove", "Flag & Move"),
    ("newEmailTo", "New E-mail To"),
    ("forwardTo", "Forward To"),
    ("newMeeting", "New Meeting"),
    ("custom", "Custom"),
];

fn manage_quick_steps(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    with_state(
        ctx,
        app,
        uid,
        |a: &App| ManageQs { selected: a.s.mailbox.quick_steps.first().map(|q| q.id) },
        |st, app| {
            window(ctx, app, d, "Manage Quick Steps", 580.0, |ui, app| {
                let steps = app.s.mailbox.quick_steps.clone();
                let pos = st.selected.and_then(|id| steps.iter().position(|q| q.id == id));
                let sel = pos.and_then(|i| steps.get(i)).cloned();
                let mut out = Out::Keep;
                ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(230.0);
                    ui.label(RichText::new("Quick step").strong());
                    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(ui, |ui| {
                        ui.set_min_height(220.0);
                        for q in &steps {
                            if ui.selectable_label(st.selected == Some(q.id), &q.name).clicked() {
                                st.selected = Some(q.id);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.add_enabled(pos.is_some_and(|i| i > 0), egui::Button::new("▲")).on_hover_text("Move Up in List").clicked()
                            && let Some(q) = &sel
                        {
                            app.run("quicksteps.moveUp", json!({"id": q.id}));
                        }
                        if ui.add_enabled(pos.is_some_and(|i| i + 1 < steps.len()), egui::Button::new("▼")).on_hover_text("Move Down in List").clicked()
                            && let Some(q) = &sel
                        {
                            app.run("quicksteps.moveDown", json!({"id": q.id}));
                        }
                        ui.menu_button("New", |ui| {
                            for (key, label) in QS_TEMPLATES {
                                if ui.button(label).clicked() {
                                    app.run("quicksteps.new", json!({"template": key}));
                                    ui.close();
                                }
                            }
                        });
                    });
                });
                ui.vertical(|ui| {
                    ui.label(RichText::new("Description").strong());
                    if let Some(q) = &sel {
                        ui.label(RichText::new(&q.name).strong());
                        ui.label("Actions");
                        for a in &q.actions {
                            ui.label(format!("  {}", qs_action_text(app, a)));
                        }
                        ui.label(format!("Shortcut key: {}", if q.shortcut.is_empty() { "None" } else { &q.shortcut }));
                        if !q.tooltip.is_empty() {
                            muted(ui, app, &q.tooltip);
                        }
                    }
                    ui.horizontal(|ui| {
                        if btn_en(ui, sel.is_some(), "Edit") && let Some(q) = &sel {
                            app.run("quicksteps.edit", json!({"id": q.id}));
                        }
                        if btn_en(ui, sel.is_some(), "Duplicate") && let Some(q) = &sel {
                            app.run("quicksteps.duplicate", json!({"id": q.id}));
                        }
                        if btn_en(ui, sel.is_some(), "Delete") && let Some(q) = &sel {
                            out = Out::open("confirm", json!({"title": "Manage Quick Steps", "text": format!("Delete the quick step \"{}\"?", q.name), "command": "quicksteps.delete", "params": {"id": q.id}}));
                        }
                    });
                });
            });
                if !matches!(out, Out::Keep) {
                    return out;
                }
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        return Out::Close;
                    }
                    if primary(ui, app, true, "OK") {
                        return Out::Close;
                    }
                    if btn(ui, "Reset to Defaults") {
                        return Out::open(
                            "confirm",
                            json!({"title": "Manage Quick Steps", "text": "Put back the default quick steps? Your own quick steps are removed.", "command": "quicksteps.reset", "params": {}}),
                        );
                    }
                    Out::Keep
                })
            })
        },
    )
}

/// Action kinds offered in the quick step editor.
const QS_KINDS: [&str; 17] = [
    "Move to folder",
    "Copy to folder",
    "Delete message",
    "Permanently delete message",
    "Mark as read",
    "Mark as unread",
    "Set importance",
    "Categorize message",
    "Clear categories",
    "Flag message",
    "Clear flag",
    "Mark complete",
    "New message",
    "Forward",
    "Reply",
    "Reply All",
    "New meeting",
];
const FLAG_KINDS: [&str; 5] = ["today", "tomorrow", "thisWeek", "nextWeek", "noDate"];

#[derive(Clone, Debug, Default)]
struct QsAct {
    kind: usize,
    folder: Option<u64>,
    text: String,
    choice: usize,
}

impl QsAct {
    fn from(a: &QuickStepAction) -> QsAct {
        let k = |kind: usize| QsAct { kind, ..Default::default() };
        match a {
            QuickStepAction::MoveTo(f) => QsAct { kind: 0, folder: f.map(|f| f.0), ..Default::default() },
            QuickStepAction::CopyTo(f) => QsAct { kind: 1, folder: f.map(|f| f.0), ..Default::default() },
            QuickStepAction::Delete => k(2),
            QuickStepAction::PermanentlyDelete => k(3),
            QuickStepAction::MarkRead => k(4),
            QuickStepAction::MarkUnread => k(5),
            QuickStepAction::SetImportance(i) => QsAct {
                kind: 6,
                choice: match i {
                    Importance::Low => 0,
                    Importance::Normal => 1,
                    Importance::High => 2,
                },
                ..Default::default()
            },
            QuickStepAction::Categorize(c) => QsAct { kind: 7, text: c.clone(), ..Default::default() },
            QuickStepAction::ClearCategories => k(8),
            QuickStepAction::Flag(f) => QsAct {
                kind: 9,
                choice: FLAG_KINDS.iter().position(|x| x == f).unwrap_or(0),
                ..Default::default()
            },
            QuickStepAction::ClearFlag => k(10),
            QuickStepAction::MarkComplete => k(11),
            QuickStepAction::NewMessageTo(to) => {
                QsAct { kind: 12, text: to.join("; "), ..Default::default() }
            }
            QuickStepAction::Forward(to) => QsAct { kind: 13, text: to.join("; "), ..Default::default() },
            QuickStepAction::Reply => k(14),
            QuickStepAction::ReplyAll => k(15),
            QuickStepAction::NewMeeting => k(16),
        }
    }
    fn build(&self) -> QuickStepAction {
        match self.kind {
            0 => QuickStepAction::MoveTo(self.folder.map(FolderId)),
            1 => QuickStepAction::CopyTo(self.folder.map(FolderId)),
            2 => QuickStepAction::Delete,
            3 => QuickStepAction::PermanentlyDelete,
            4 => QuickStepAction::MarkRead,
            5 => QuickStepAction::MarkUnread,
            6 => QuickStepAction::SetImportance(match self.choice {
                0 => Importance::Low,
                2 => Importance::High,
                _ => Importance::Normal,
            }),
            7 => QuickStepAction::Categorize(self.text.trim().to_owned()),
            8 => QuickStepAction::ClearCategories,
            9 => QuickStepAction::Flag(FLAG_KINDS.get(self.choice).copied().unwrap_or("today").to_owned()),
            10 => QuickStepAction::ClearFlag,
            11 => QuickStepAction::MarkComplete,
            12 => QuickStepAction::NewMessageTo(common::split_list(&self.text)),
            13 => QuickStepAction::Forward(common::split_list(&self.text)),
            14 => QuickStepAction::Reply,
            15 => QuickStepAction::ReplyAll,
            _ => QuickStepAction::NewMeeting,
        }
    }
}

#[derive(Clone, Default)]
struct EditQs {
    id: Option<u64>,
    name: String,
    acts: Vec<QsAct>,
    shortcut: String,
    tooltip: String,
    error: String,
}

const QS_KEYS: [&str; 10] = [
    "None",
    "Ctrl+Shift+1",
    "Ctrl+Shift+2",
    "Ctrl+Shift+3",
    "Ctrl+Shift+4",
    "Ctrl+Shift+5",
    "Ctrl+Shift+6",
    "Ctrl+Shift+7",
    "Ctrl+Shift+8",
    "Ctrl+Shift+9",
];

fn edit_quick_step(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |a: &App| {
        let step = d.get("step").cloned().unwrap_or(Value::Null);
        let id = step
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| a.s.mailbox.quick_steps.iter().any(|q| q.id == *id));
        let actions: Vec<QuickStepAction> =
            step.get("actions").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        let shortcut = step.get("shortcut").and_then(Value::as_str).unwrap_or_default();
        EditQs {
            id,
            name: step.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
            acts: if actions.is_empty() {
                vec![QsAct::default()]
            } else {
                actions.iter().map(QsAct::from).collect()
            },
            shortcut: if shortcut.is_empty() { "None".into() } else { shortcut.to_owned() },
            tooltip: step.get("tooltip").and_then(Value::as_str).unwrap_or_default().to_owned(),
            error: String::new(),
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if let Some(i) = k.strip_prefix("folder:").and_then(|i| i.parse::<usize>().ok())
                && let Some(a) = st.acts.get_mut(i)
            {
                a.folder = v.as_u64();
            }
        }
        window(ctx, app, d, "Edit Quick Step", 520.0, |ui, app| {
            common::field(ui, "Name:", &mut st.name, 360.0);
            muted(ui, app, "Add the actions this quick step performs, in order.");
            ui.label(RichText::new("Actions").strong());
            let mut out = Out::Keep;
            let mut remove = None;
            let cats = super::categories::category_names(app);
            for (i, a) in st.acts.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    common::combo_idx(ui, ("qsk", uid, i), &mut a.kind, &QS_KINDS, 190.0);
                    match a.kind {
                        0 | 1 => {
                            let p = folder_path(app, a.folder);
                            ui.label(if p.is_empty() { "Choose on first use".to_owned() } else { p });
                            if ui.button("Folder...").clicked() {
                                out = Out::open(
                                    "pickFolder",
                                    child(uid, &format!("folder:{i}"), json!({"selected": a.folder})),
                                );
                            }
                        }
                        6 => {
                            common::combo_idx(
                                ui,
                                ("qsi", uid, i),
                                &mut a.choice,
                                &["Low", "Normal", "High"],
                                100.0,
                            );
                        }
                        7 => {
                            let c: Vec<&str> = cats.iter().map(String::as_str).collect();
                            common::combo(ui, ("qsc", uid, i), &mut a.text, &c, 160.0);
                        }
                        9 => {
                            common::combo_idx(
                                ui,
                                ("qsf", uid, i),
                                &mut a.choice,
                                &["Today", "Tomorrow", "This Week", "Next Week", "No Date"],
                                120.0,
                            );
                        }
                        12 | 13 => {
                            ui.add(
                                egui::TextEdit::singleline(&mut a.text).hint_text("To").desired_width(160.0),
                            );
                        }
                        _ => {}
                    }
                    if ui.button("✕").on_hover_text("Delete action").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove
                && i < st.acts.len()
            {
                st.acts.remove(i);
            }
            if btn(ui, "Add Action") {
                st.acts.push(QsAct { kind: 4, ..Default::default() });
            }
            common::heading(ui, app, "Optional");
            ui.horizontal(|ui| {
                ui.label("Shortcut key:");
                common::combo(ui, ("qskey", uid), &mut st.shortcut, &QS_KEYS, 140.0);
            });
            common::field(ui, "Tooltip text:", &mut st.tooltip, 330.0);
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            if !matches!(out, Out::Keep) {
                return out;
            }
            footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    return Out::Close;
                }
                if primary(ui, app, !st.name.trim().is_empty() && !st.acts.is_empty(), "Finish") {
                    let actions: Vec<QuickStepAction> = st.acts.iter().map(QsAct::build).collect();
                    let shortcut = if st.shortcut == "None" { String::new() } else { st.shortcut.clone() };
                    let p = json!({"name": st.name.trim(), "actions": actions, "shortcut": shortcut, "tooltip": st.tooltip.trim()});
                    let res = match st.id {
                        Some(id) => {
                            let mut p = p;
                            if let Some(o) = p.as_object_mut() {
                                o.insert("id".into(), json!(id));
                            }
                            app.run("quicksteps.edit", p)
                        }
                        None => {
                            let mut p = p;
                            if let Some(o) = p.as_object_mut() {
                                o.insert("template".into(), json!("custom"));
                            }
                            app.run("quicksteps.new", p)
                        }
                    };
                    if res.is_some() {
                        return Out::Close;
                    }
                    st.error = last_error(app);
                }
                Out::Keep
            })
        })
    })
}

#[derive(Clone, Default)]
struct FirstUse {
    folder: Option<u64>,
    to: String,
    name: String,
}

fn quick_step_first_use(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let step = d.get("step").cloned().unwrap_or(Value::Null);
    let actions: Vec<QuickStepAction> =
        step.get("actions").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    let needs_folder =
        actions.iter().any(|a| matches!(a, QuickStepAction::MoveTo(None) | QuickStepAction::CopyTo(None)));
    let needs_to = actions
        .iter()
        .any(|a| matches!(a, QuickStepAction::Forward(t) | QuickStepAction::NewMessageTo(t) if t.is_empty()));
    let init = |_: &App| FirstUse {
        name: step.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        ..Default::default()
    };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if k == "folder" {
                st.folder = v.as_u64();
            }
        }
        window(ctx, app, d, "First Time Setup", 420.0, |ui, app| {
            ui.label(format!("Set up the quick step \"{}\" before its first use.", st.name));
            let mut out = Out::Keep;
            if needs_folder {
                ui.horizontal(|ui| {
                    ui.label("Folder:");
                    let p = folder_path(app, st.folder);
                    ui.label(if p.is_empty() { "Choose folder".to_owned() } else { p });
                    if btn(ui, "Choose...") {
                        out = Out::open("pickFolder", child(uid, "folder", json!({"selected": st.folder})));
                    }
                });
            }
            if needs_to {
                common::field(ui, "To:", &mut st.to, 300.0);
            }
            for a in &actions {
                muted(ui, app, &qs_action_text(app, a));
            }
            if !matches!(out, Out::Keep) {
                return out;
            }
            let ready = (!needs_folder || st.folder.is_some()) && (!needs_to || !st.to.trim().is_empty());
            match ok_cancel(ui, app, ready) {
                Some(true) => {
                    let mut p = json!({"id": step.get("id")});
                    if let Some(o) = p.as_object_mut() {
                        if let Some(f) = st.folder {
                            o.insert("folder".into(), json!(f));
                        }
                        if needs_to {
                            o.insert("to".into(), json!(common::split_list(&st.to)));
                        }
                    }
                    app.run("quicksteps.run", p);
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}
