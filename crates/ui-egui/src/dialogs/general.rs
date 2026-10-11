//! Small and informational dialogs: About, Help, What's New, Feedback, Contact Support, generic message,
//! Exit, Save Changes, View Source, Reveal Path, search tools, Send/Receive progress, Recall, Ribbon Display
//! Options, Quick Access Toolbar. (Quick steps: `quicksteps.rs`; the Reminders window: `modules`.)

use egui::{Context, RichText, Sense, Vec2};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn, btn_en, footer, muted, ok_cancel, primary, window, with_state};
use crate::App;

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
        "recallMessage" => recall(ctx, app, d),
        "ribbonDisplayOptions" => ribbon_display(ctx, app, d),
        "quickAccess" => quick_access(ctx, app, d),
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
            "Classic desktop mail layout: ribbon, folder pane, message list and reading pane.",
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
