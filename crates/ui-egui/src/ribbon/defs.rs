//! Ribbon definitions: the `*.ribbon` text files in this directory parsed into tabs → groups → controls and
//! named menus. The grammar is documented at the top of `main.ribbon`. Parsing never panics; malformed lines
//! are collected in [`Defs::errors`] (a unit test asserts there are none).

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// The definition sources, in lookup order.
pub const SOURCES: [(&str, &str); 4] = [
    ("main.ribbon", include_str!("main.ribbon")),
    ("compose.ribbon", include_str!("compose.ribbon")),
    ("read.ribbon", include_str!("read.ribbon")),
    ("modules.ribbon", include_str!("modules.ribbon")),
];

/// Ribbon mode a tab definition applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Simplified,
    Classic,
    Both,
}

/// Control kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Runs its command.
    Button,
    /// Runs its command; the ▾ part opens its menu.
    Split,
    /// Opens its menu.
    Drop,
    /// Pressed state from `on=`; runs its command (or `oncmd` when on).
    Toggle,
    /// Like a toggle, drawn with a check box in menus and the Classic ribbon.
    Check,
    /// Inline gallery of dynamic items (quick steps, styles, views) with a ▾ for the full menu.
    Gallery,
    /// Value box with a dropdown list (font, size).
    Combo,
    /// Text box that runs its command with `$value` on Enter.
    Edit,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "button" => Kind::Button,
            "split" => Kind::Split,
            "drop" => Kind::Drop,
            "toggle" => Kind::Toggle,
            "check" => Kind::Check,
            "gallery" => Kind::Gallery,
            "combo" => Kind::Combo,
            "edit" => Kind::Edit,
            _ => return None,
        })
    }
    /// Has a pressed state.
    pub fn is_toggle(self) -> bool {
        matches!(self, Kind::Toggle | Kind::Check)
    }
    /// Shows a ▾.
    pub fn has_arrow(self) -> bool {
        matches!(self, Kind::Split | Kind::Drop | Kind::Combo)
    }
}

/// Size class in the Classic ribbon (the Simplified ribbon draws every control small).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// Large: 32-px icon over a (possibly two-line) label.
    Large,
    /// Small: 16-px icon and label on one 22-px row.
    Small,
    /// Small, icon only (tooltip carries the label).
    Icon,
}

/// Width-degradation action (plan/reference/responsive.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Labels drop (Simplified: every control; Classic: small buttons).
    NoLabel,
    /// Simplified: every control but the first loses its label.
    NoLabelButFirst,
    /// Galleries narrow (Simplified: fewer chips; Classic: one column).
    Narrow,
    /// Simplified: move into the "…" overflow menu.
    Overflow,
    /// Classic: large controls become small.
    Small,
    /// Classic: everything icon-only.
    IconOnly,
    /// Classic: the group becomes one dropdown button.
    Collapse,
}

impl Act {
    fn parse(s: &str) -> Option<Act> {
        Some(match s {
            "nolabel" => Act::NoLabel,
            "nolabel1" => Act::NoLabelButFirst,
            "narrow" => Act::Narrow,
            "overflow" => Act::Overflow,
            "small" => Act::Small,
            "iconly" => Act::IconOnly,
            "collapse" => Act::Collapse,
            _ => return None,
        })
    }
}

/// "Apply `act` when the effective width is ≤ `max_width`" in the given ribbon mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rule {
    pub classic: bool,
    pub max_width: f32,
    pub act: Act,
}

/// A command reference with its JSON params text (may contain `$…` placeholders).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CmdRef {
    pub id: String,
    pub params: String,
}

impl CmdRef {
    /// Parses `cmd.id{json}` (the JSON part optional).
    fn parse(s: &str) -> CmdRef {
        match s.find('{') {
            Some(i) => CmdRef {
                id: s.get(..i).unwrap_or_default().trim().to_owned(),
                params: s.get(i..).unwrap_or_default().trim().to_owned(),
            },
            None => CmdRef { id: s.trim().to_owned(), params: String::new() },
        }
    }
}

/// One ribbon control.
#[derive(Clone, Debug, PartialEq)]
pub struct Ctl {
    pub kind: Kind,
    pub size: Size,
    pub label: String,
    /// `assets/icons` name ("" = none).
    pub icon: String,
    /// Command run on click ("" for menu-only controls).
    pub cmd: CmdRef,
    /// Keytip within the tab ("N1", "RP").
    pub key: String,
    pub menu: Option<String>,
    /// State predicate (see `state.rs`).
    pub on: Option<String>,
    /// Command run instead of `cmd` while the toggle is on.
    pub oncmd: Option<CmdRef>,
    /// Dynamic item source for galleries.
    pub source: Option<String>,
    /// Fixed width for edits, combos and galleries.
    pub width: Option<f32>,
    pub rules: Vec<Rule>,
    /// Starts a new row (groups laid out in rows, Classic).
    pub row_break: bool,
}

/// A ribbon group.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub name: String,
    pub key: String,
    pub rules: Vec<Rule>,
    /// Icon of the collapsed-group button (default: the first control's).
    pub icon: Option<String>,
    /// Dialog launcher (↘ at the label's right, Classic).
    pub launch: Option<CmdRef>,
    /// Small controls flow in up to three rows instead of columns (Classic).
    pub rows: bool,
    pub ctls: Vec<Ctl>,
}

impl Group {
    /// Stable id: the name lower-cased with only letters and digits kept ("Quick Steps" → "quicksteps").
    pub fn id(&self) -> String {
        self.name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
    }

    pub fn icon(&self) -> &str {
        match &self.icon {
            Some(i) => i,
            None => self.ctls.iter().map(|c| c.icon.as_str()).find(|i| !i.is_empty()).unwrap_or("more"),
        }
    }
}

/// A ribbon tab for one surface and mode.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub surface: String,
    pub mode: Mode,
    pub name: String,
    pub key: String,
    /// Fixed items of the "…" (More Commands) menu.
    pub more: Option<String>,
    pub groups: Vec<Group>,
}

/// Menu entry kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Cmd,
    /// Submenu (`target` = menu name).
    Sub,
    /// Dynamic items (`target` = source name, `cmd` = template).
    Dyn,
    Sep,
}

/// A menu entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub kind: ItemKind,
    pub label: String,
    pub icon: String,
    pub cmd: CmdRef,
    pub key: String,
    pub on: Option<String>,
    pub target: String,
    /// Colour swatch drawn instead of an icon.
    pub swatch: Option<[u8; 3]>,
}

impl Item {
    pub fn sep() -> Item {
        Item {
            kind: ItemKind::Sep,
            label: String::new(),
            icon: String::new(),
            cmd: CmdRef::default(),
            key: String::new(),
            on: None,
            target: String::new(),
            swatch: None,
        }
    }
}

/// All parsed definitions.
#[derive(Clone, Debug, Default)]
pub struct Defs {
    pub tabs: Vec<Tab>,
    pub menus: BTreeMap<String, Vec<Item>>,
    /// Parse problems (`file:line: message`).
    pub errors: Vec<String>,
}

/// The parsed definitions (parsed once; immutable).
pub fn defs() -> &'static Defs {
    static DEFS: OnceLock<Defs> = OnceLock::new();
    DEFS.get_or_init(|| parse_all(&SOURCES))
}

impl Defs {
    /// The tab definition for a surface, mode and name: an exact-mode definition first, then `Both`, then
    /// (for Simplified) the Classic one, which the drawing code renders small.
    pub fn tab(&self, surface: &str, simplified: bool, name: &str) -> Option<&Tab> {
        let want = if simplified { Mode::Simplified } else { Mode::Classic };
        let find = |m: Mode| self.tabs.iter().find(|t| t.surface == surface && t.name == name && t.mode == m);
        find(want)
            .or_else(|| find(Mode::Both))
            .or_else(|| if simplified { find(Mode::Classic) } else { None })
    }

    /// Tab names defined for a surface and mode, in definition order (no duplicates).
    pub fn tab_names(&self, surface: &str, simplified: bool) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for t in &self.tabs {
            let fits = match t.mode {
                Mode::Both => true,
                Mode::Simplified => simplified,
                Mode::Classic => !simplified || !self.has_mode(surface, Mode::Simplified),
            };
            if t.surface == surface && fits && !out.contains(&t.name.as_str()) {
                out.push(&t.name);
            }
        }
        out
    }

    fn has_mode(&self, surface: &str, mode: Mode) -> bool {
        self.tabs.iter().any(|t| t.surface == surface && t.mode == mode)
    }

    pub fn menu(&self, name: &str) -> Option<&[Item]> {
        self.menus.get(name).map(Vec::as_slice)
    }
}

/// Parses every source.
pub fn parse_all(sources: &[(&str, &str)]) -> Defs {
    let mut d = Defs::default();
    for (file, text) in sources {
        parse_into(&mut d, file, text);
    }
    d
}

enum Block {
    None,
    Tab,
    Menu(String),
}

fn fields(rest: &str) -> Vec<String> {
    rest.split('|').map(|f| f.trim().to_owned()).collect()
}

fn field(f: &[String], i: usize) -> String {
    f.get(i).cloned().unwrap_or_default()
}

/// Parses `s1600=narrow` / `c1024=collapse`.
fn parse_rule(tok: &str) -> Option<Rule> {
    let (lhs, act) = tok.split_once('=')?;
    let classic = match lhs.chars().next()? {
        's' => false,
        'c' => true,
        _ => return None,
    };
    let max_width: f32 = lhs.get(1..)?.parse().ok()?;
    Some(Rule { classic, max_width, act: Act::parse(act)? })
}

/// Splits an options field into tokens; JSON inside `{…}` may contain spaces.
fn opt_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '{' => depth = depth.saturating_add(1),
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if ch.is_whitespace() && depth <= 0 {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([((v >> 16) & 0xff) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8])
}

fn parse_into(d: &mut Defs, file: &str, text: &str) {
    let mut surface = String::from("main");
    let mut mode = Mode::Both;
    let mut block = Block::None;
    let mut pending_break = false;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |msg: &str| format!("{file}:{}: {msg}", n.saturating_add(1));
        let (head, rest) = match line.split_once(char::is_whitespace) {
            Some((h, r)) => (h, r.trim()),
            None => (line, ""),
        };
        match head {
            "surface" => {
                surface = rest.to_owned();
                block = Block::None;
            }
            "mode" => {
                mode = match rest {
                    "simplified" => Mode::Simplified,
                    "classic" => Mode::Classic,
                    "both" => Mode::Both,
                    _ => {
                        d.errors.push(at("unknown mode"));
                        Mode::Both
                    }
                };
                block = Block::None;
            }
            "tab" => {
                let f = fields(rest);
                let mut more = None;
                for tok in opt_tokens(&field(&f, 2)) {
                    match tok.split_once('=') {
                        Some(("more", m)) => more = Some(m.to_owned()),
                        _ => d.errors.push(at(&format!("unknown tab option `{tok}`"))),
                    }
                }
                d.tabs.push(Tab {
                    surface: surface.clone(),
                    mode,
                    name: field(&f, 0),
                    key: field(&f, 1).replace(' ', ""),
                    more,
                    groups: Vec::new(),
                });
                block = Block::Tab;
            }
            "group" => {
                let Some(tab) = d.tabs.last_mut().filter(|_| matches!(block, Block::Tab)) else {
                    d.errors.push(at("group outside a tab"));
                    continue;
                };
                let f = fields(rest);
                let mut g = Group {
                    name: field(&f, 0),
                    key: field(&f, 1).replace(' ', ""),
                    rules: Vec::new(),
                    icon: None,
                    launch: None,
                    rows: false,
                    ctls: Vec::new(),
                };
                for tok in opt_tokens(&field(&f, 2)) {
                    if tok == "rows" {
                        g.rows = true;
                    } else if let Some(i) = tok.strip_prefix("icon=") {
                        g.icon = Some(i.to_owned());
                    } else if let Some(c) = tok.strip_prefix("launch=") {
                        g.launch = Some(CmdRef::parse(c));
                    } else if let Some(r) = parse_rule(&tok) {
                        g.rules.push(r);
                    } else {
                        d.errors.push(at(&format!("unknown group option `{tok}`")));
                    }
                }
                tab.groups.push(g);
                pending_break = false;
            }
            "br" => pending_break = true,
            "menu" => {
                let name = rest.to_owned();
                if d.menus.contains_key(&name) {
                    d.errors.push(at(&format!("menu `{name}` defined twice")));
                }
                d.menus.insert(name.clone(), Vec::new());
                block = Block::Menu(name);
            }
            "item" | "sub" | "dyn" | "sep" => {
                let Block::Menu(name) = &block else {
                    d.errors.push(at("menu entry outside a menu"));
                    continue;
                };
                let f = fields(rest.trim_start_matches('|'));
                let mut item = Item::sep();
                match head {
                    "sep" => {}
                    "dyn" => {
                        item.kind = ItemKind::Dyn;
                        item.target = field(&f, 0);
                        item.cmd = CmdRef { id: field(&f, 1), params: field(&f, 2) };
                    }
                    _ => {
                        item.kind = if head == "sub" { ItemKind::Sub } else { ItemKind::Cmd };
                        item.label = field(&f, 0);
                        item.icon = field(&f, 1);
                        if head == "sub" {
                            item.target = field(&f, 2);
                        } else {
                            item.cmd = CmdRef { id: field(&f, 2), params: field(&f, 3) };
                        }
                        item.key = field(&f, 4).replace(' ', "");
                        for tok in opt_tokens(&field(&f, 5)) {
                            if let Some(s) = tok.strip_prefix("on=") {
                                item.on = Some(s.to_owned());
                            } else if let Some(s) = tok.strip_prefix("swatch=") {
                                item.swatch = parse_hex(s);
                                if item.swatch.is_none() {
                                    d.errors.push(at(&format!("bad swatch `{s}`")));
                                }
                            } else {
                                d.errors.push(at(&format!("unknown item option `{tok}`")));
                            }
                        }
                    }
                }
                if let Some(m) = d.menus.get_mut(name) {
                    m.push(item);
                }
            }
            _ => {
                let Some(kind) = Kind::parse(head) else {
                    d.errors.push(at(&format!("unknown line kind `{head}`")));
                    continue;
                };
                let Some(group) = d
                    .tabs
                    .last_mut()
                    .filter(|_| matches!(block, Block::Tab))
                    .and_then(|t| t.groups.last_mut())
                else {
                    d.errors.push(at("control outside a group"));
                    continue;
                };
                let (size_tok, rest) = rest.split_once('|').unwrap_or((rest, ""));
                let size = match size_tok.trim() {
                    "L" => Size::Large,
                    "S" => Size::Small,
                    "I" => Size::Icon,
                    other => {
                        d.errors.push(at(&format!("unknown size `{other}`")));
                        Size::Small
                    }
                };
                let f = fields(rest);
                let mut c = Ctl {
                    kind,
                    size,
                    label: field(&f, 0),
                    icon: field(&f, 1),
                    cmd: CmdRef { id: field(&f, 2), params: field(&f, 3) },
                    key: field(&f, 4).replace(' ', ""),
                    menu: None,
                    on: None,
                    oncmd: None,
                    source: None,
                    width: None,
                    rules: Vec::new(),
                    row_break: std::mem::take(&mut pending_break),
                };
                for tok in opt_tokens(&field(&f, 5)) {
                    if let Some(s) = tok.strip_prefix("menu=") {
                        c.menu = Some(s.to_owned());
                    } else if let Some(s) = tok.strip_prefix("on=") {
                        c.on = Some(s.to_owned());
                    } else if let Some(s) = tok.strip_prefix("oncmd=") {
                        c.oncmd = Some(CmdRef::parse(s));
                    } else if let Some(s) = tok.strip_prefix("dyn=") {
                        c.source = Some(s.to_owned());
                    } else if let Some(s) = tok.strip_prefix("w=") {
                        c.width = s.parse().ok();
                        if c.width.is_none() {
                            d.errors.push(at(&format!("bad width `{s}`")));
                        }
                    } else if let Some(r) = parse_rule(&tok) {
                        c.rules.push(r);
                    } else {
                        d.errors.push(at(&format!("unknown control option `{tok}`")));
                    }
                }
                group.ctls.push(c);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home_classic() -> &'static Tab {
        defs().tab("main", false, "Home").unwrap()
    }

    #[test]
    fn group_ids() {
        let ids: Vec<String> = home_classic().groups.iter().map(Group::id).collect();
        assert!(ids.iter().any(|i| i == "respond") && ids.iter().any(|i| i == "quicksteps"), "{ids:?}");
    }

    #[test]
    fn classic_respond_goes_icon_only_at_1024() {
        // responsive.md, Classic 1024: "Respond icon-only" (Reply, Reply All and Forward lose their labels).
        let respond = home_classic().groups.iter().find(|g| g.id() == "respond").unwrap();
        let icon_only =
            |w: f32| respond.rules.iter().any(|r| r.classic && w <= r.max_width && r.act == Act::IconOnly);
        assert!(icon_only(1024.0) && icon_only(700.0));
        assert!(!icon_only(1025.0) && !icon_only(1366.0));
        // Only the Classic Home tab of the main window gets it; the Simplified ribbon keeps its own rule.
        let simplified = defs().tab("main", true, "Home").unwrap();
        let s_respond = simplified.groups.iter().find(|g| g.id() == "respond").unwrap();
        assert!(s_respond.rules.iter().all(|r| !r.classic));
        assert!(defs().errors.is_empty(), "{:?}", defs().errors);
    }

    #[test]
    fn group_rules_come_from_the_source_alone() {
        let src = "mode classic\ntab Home | H\ngroup Respond | ZR | c1024=iconly\nbutton L | Reply | reply | mail.reply | | RP\n";
        let d = parse_all(&[("t.ribbon", src)]);
        let g = d.tabs.first().and_then(|t| t.groups.first()).unwrap();
        assert_eq!(g.rules, [Rule { classic: true, max_width: 1024.0, act: Act::IconOnly }]);
        let other =
            parse_all(&[("t.ribbon", "surface main\nmode classic\ntab Home | H\ngroup Respond | ZR\n")]);
        assert!(other.tabs.first().and_then(|t| t.groups.first()).unwrap().rules.is_empty());
    }
}
