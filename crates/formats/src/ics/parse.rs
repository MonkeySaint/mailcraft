//! iCalendar object structure (RFC 5545 §3.4–3.6): nested BEGIN/END components holding content lines.

use crate::text::{ContentLine, unfold};

/// Deepest component nesting kept (VCALENDAR › VEVENT › VALARM is 3).
pub const MAX_DEPTH: usize = 16;
/// Most components read from one input (all levels).
pub const MAX_COMPONENTS: usize = 200_000;
/// Most properties kept per component.
pub const MAX_PROPERTIES: usize = 10_000;

/// A component (`VCALENDAR`, `VEVENT`, `VALARM`, …) with its properties and sub-components.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Component {
    /// Upper-cased name.
    pub name: String,
    pub props: Vec<ContentLine>,
    pub children: Vec<Component>,
}

impl Component {
    pub fn new(name: &str) -> Self {
        Component { name: name.to_ascii_uppercase(), ..Default::default() }
    }

    /// First property named `name`.
    pub fn prop(&self, name: &str) -> Option<&ContentLine> {
        self.props.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// Every property named `name`.
    pub fn props_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a ContentLine> + 'a {
        self.props.iter().filter(move |p| p.name.eq_ignore_ascii_case(name))
    }

    /// Decoded TEXT value of the first `name` property.
    pub fn text(&self, name: &str) -> Option<String> {
        self.prop(name).map(|p| crate::text::unescape(&p.value))
    }

    /// Direct children named `name`.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Component> + 'a {
        self.children.iter().filter(move |c| c.name.eq_ignore_ascii_case(name))
    }

    /// Adds a property with an already-escaped value.
    pub fn push(&mut self, name: &str, params: Vec<(&str, String)>, value: impl Into<String>) {
        self.props.push(ContentLine {
            group: None,
            name: name.to_owned(),
            params: params.into_iter().map(|(n, v)| (n.to_owned(), vec![v])).collect(),
            value: value.into(),
        });
    }

    /// Serialises with folding and CRLF line endings.
    pub fn write(&self, out: &mut String) {
        let mut stack: Vec<(&Component, usize)> = vec![(self, 0)];
        out.push_str(&format!("BEGIN:{}\r\n", self.name));
        for p in &self.props {
            p.write(out);
        }
        while let Some((comp, next)) = stack.pop() {
            match comp.children.get(next) {
                Some(child) => {
                    stack.push((comp, next + 1));
                    if stack.len() <= MAX_DEPTH {
                        out.push_str(&format!("BEGIN:{}\r\n", child.name));
                        for p in &child.props {
                            p.write(out);
                        }
                        stack.push((child, 0));
                    }
                }
                None => out.push_str(&format!("END:{}\r\n", comp.name)),
            }
        }
    }
}

/// Parses text into its top-level components (normally one `VCALENDAR`). Lines outside any component are
/// ignored; unbalanced END lines are tolerated; components left open at the end are closed.
pub fn parse(text: &str) -> Vec<Component> {
    let mut top: Vec<Component> = Vec::new();
    let mut stack: Vec<Component> = Vec::new();
    let mut count = 0usize;
    // Components nested deeper than MAX_DEPTH are skipped; this counts how deep into one we are.
    let mut skipping = 0usize;
    for line in unfold(text) {
        let Some(cl) = ContentLine::parse(&line) else { continue };
        if cl.name == "BEGIN" {
            if skipping > 0 || stack.len() >= MAX_DEPTH || count >= MAX_COMPONENTS {
                skipping = skipping.saturating_add(1);
                continue;
            }
            count += 1;
            stack.push(Component::new(cl.value.trim()));
            continue;
        }
        if cl.name == "END" {
            if skipping > 0 {
                skipping -= 1;
                continue;
            }
            let name = cl.value.trim().to_ascii_uppercase();
            // Close up to the matching component; an END that matches nothing open is ignored.
            if !stack.iter().any(|c| c.name == name) {
                continue;
            }
            while let Some(done) = stack.pop() {
                let matched = done.name == name;
                attach(&mut stack, &mut top, done);
                if matched {
                    break;
                }
            }
            continue;
        }
        if skipping > 0 {
            continue;
        }
        if let Some(cur) = stack.last_mut()
            && cur.props.len() < MAX_PROPERTIES
        {
            cur.props.push(cl);
        }
    }
    while let Some(done) = stack.pop() {
        attach(&mut stack, &mut top, done);
    }
    top
}

fn attach(stack: &mut [Component], top: &mut Vec<Component>, done: Component) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(done),
        None => top.push(done),
    }
}
