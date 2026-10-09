//! Tree builder: a simplified WHATWG insertion algorithm.
//!
//! Handles the cases mail actually needs: implied end tags (`p`, `li`, `dt`/`dd`, `option`, table rows and
//! cells, headings), void elements, implicit `tr` for stray cells, mis-nested formatting elements (closed and
//! re-opened, a cheap stand-in for the adoption agency algorithm) and end tags that do not match anything.
//! `html`, `head`, `body` and table section tags produce no nodes; `body` attributes are kept for the
//! document defaults. Nesting is capped at [`crate::MAX_DEPTH`] and the arena at [`MAX_NODES`].

use crate::tokenizer::{Token, Tokenizer};

pub(crate) type NodeId = usize;

/// Hard cap on arena size; further elements are ignored and text only merges into existing nodes.
pub(crate) const MAX_NODES: usize = 1_000_000;
/// Formatting elements re-opened after a mis-nested end tag.
const MAX_REOPEN: usize = 8;
/// Total attribute bytes copied into re-opened formatting elements. Each mis-nested end tag can clone the
/// elements above it, so without a budget one large attribute could be copied once per open element.
/// Attributes that no longer fit are left off the clone.
const REOPEN_BUDGET: usize = crate::MAX_INPUT_BYTES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NodeData {
    Root,
    Element { name: String, attrs: Vec<(String, String)> },
    Text(String),
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub data: NodeData,
    pub children: Vec<NodeId>,
}

#[derive(Debug)]
pub(crate) struct Dom {
    pub nodes: Vec<Node>,
    pub body_attrs: Vec<(String, String)>,
}

impl Dom {
    pub(crate) const ROOT: NodeId = 0;

    pub(crate) fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub(crate) fn name(&self, id: NodeId) -> Option<&str> {
        match &self.node(id)?.data {
            NodeData::Element { name, .. } => Some(name),
            _ => None,
        }
    }

    pub(crate) fn children(&self, id: NodeId) -> &[NodeId] {
        self.node(id).map_or(&[], |n| n.children.as_slice())
    }
}

pub(crate) fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

fn is_void(n: &str) -> bool {
    matches!(
        n,
        "area"
            | "base"
            | "basefont"
            | "bgsound"
            | "br"
            | "col"
            | "embed"
            | "frame"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

pub(crate) fn is_heading(n: &str) -> bool {
    matches!(n, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

/// Start tags that close an open `p`.
fn closes_p(n: &str) -> bool {
    is_heading(n)
        || matches!(
            n,
            "address"
                | "article"
                | "aside"
                | "blockquote"
                | "center"
                | "details"
                | "dialog"
                | "dir"
                | "div"
                | "dl"
                | "dd"
                | "dt"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "form"
                | "header"
                | "hgroup"
                | "hr"
                | "li"
                | "listing"
                | "main"
                | "menu"
                | "nav"
                | "ol"
                | "p"
                | "plaintext"
                | "pre"
                | "section"
                | "summary"
                | "table"
                | "ul"
                | "xmp"
        )
}

fn is_formatting(n: &str) -> bool {
    matches!(
        n,
        "a" | "b"
            | "big"
            | "code"
            | "em"
            | "font"
            | "i"
            | "s"
            | "small"
            | "strike"
            | "strong"
            | "tt"
            | "u"
            | "span"
            | "sub"
            | "sup"
            | "del"
            | "ins"
            | "mark"
    )
}

/// Tags that never produce nodes.
fn is_structural(n: &str) -> bool {
    matches!(
        n,
        "html" | "head" | "body" | "tbody" | "thead" | "tfoot" | "colgroup" | "col" | "caption" | "frameset"
    )
}

const BUTTON_SCOPE: &[&str] =
    &["table", "td", "th", "caption", "marquee", "object", "applet", "template", "button"];
const GENERAL_SCOPE: &[&str] = &["table", "td", "th", "caption", "marquee", "object", "applet", "template"];

struct Builder {
    dom: Dom,
    /// Open elements; `stack[0]` is the root.
    stack: Vec<NodeId>,
    /// Remaining [`REOPEN_BUDGET`].
    reopen_budget: usize,
}

impl Builder {
    fn current(&self) -> NodeId {
        self.stack.last().copied().unwrap_or(Dom::ROOT)
    }

    fn name_at(&self, depth: usize) -> &str {
        self.stack.get(depth).and_then(|&id| self.dom.name(id)).unwrap_or("")
    }

    fn current_name(&self) -> &str {
        self.dom.name(self.current()).unwrap_or("")
    }

    /// Stack index of the nearest open `name`, not looking past any element in `boundary`.
    fn find_in_scope(&self, name: &str, boundary: &[&str]) -> Option<usize> {
        for i in (1..self.stack.len()).rev() {
            let n = self.name_at(i);
            if n == name {
                return Some(i);
            }
            if boundary.contains(&n) {
                return None;
            }
        }
        None
    }

    fn nearest(&self, name: &str) -> Option<usize> {
        (1..self.stack.len()).rev().find(|&i| self.name_at(i) == name)
    }

    fn append(&mut self, data: NodeData) -> Option<NodeId> {
        if self.dom.nodes.len() >= MAX_NODES {
            return None;
        }
        let id = self.dom.nodes.len();
        self.dom.nodes.push(Node { data, children: Vec::new() });
        let parent = self.current();
        self.dom.nodes.get_mut(parent)?.children.push(id);
        Some(id)
    }

    fn insert_element(&mut self, name: String, attrs: Vec<(String, String)>, push: bool) {
        if push && self.stack.len() > crate::MAX_DEPTH {
            return;
        }
        if let Some(id) = self.append(NodeData::Element { name, attrs })
            && push
        {
            self.stack.push(id);
        }
    }

    fn text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let parent = self.current();
        let last = self.dom.node(parent).and_then(|n| n.children.last().copied());
        if let Some(last) = last
            && let Some(Node { data: NodeData::Text(t), .. }) = self.dom.nodes.get_mut(last)
        {
            t.push_str(s);
            return;
        }
        let _ = self.append(NodeData::Text(s.to_owned()));
    }

    fn close_p(&mut self) {
        if let Some(i) = self.find_in_scope("p", BUTTON_SCOPE) {
            self.stack.truncate(i);
        }
    }

    /// Closes an open list item of one of `items`, stopping at list containers and table parts.
    fn close_item(&mut self, items: &[&str], containers: &[&str]) {
        for i in (1..self.stack.len()).rev() {
            let n = self.name_at(i);
            if items.contains(&n) {
                self.stack.truncate(i);
                return;
            }
            if containers.contains(&n) || matches!(n, "table" | "td" | "th" | "blockquote") {
                return;
            }
        }
    }

    fn start(&mut self, name: String, attrs: Vec<(String, String)>) {
        let name = if name == "image" { "img".to_owned() } else { name };
        if name == "body" {
            for (k, v) in attrs {
                if !self.dom.body_attrs.iter().any(|(n, _)| *n == k) {
                    self.dom.body_attrs.push((k, v));
                }
            }
            return;
        }
        if is_structural(&name) {
            return;
        }
        if closes_p(&name) {
            self.close_p();
        }
        if is_heading(&name) && is_heading(self.current_name()) {
            self.stack.pop();
        }
        match name.as_str() {
            "li" => self.close_item(&["li"], &["ul", "ol", "menu", "dir"]),
            "dt" | "dd" => self.close_item(&["dt", "dd"], &["dl"]),
            "option" => {
                if self.current_name() == "option" {
                    self.stack.pop();
                }
            }
            "optgroup" => {
                if self.current_name() == "option" {
                    self.stack.pop();
                }
                if self.current_name() == "optgroup" {
                    self.stack.pop();
                }
            }
            "a" => {
                if let Some(i) = self.find_in_scope("a", GENERAL_SCOPE) {
                    self.stack.truncate(i);
                }
            }
            "tr" | "td" | "th" => {
                let Some(t) = self.nearest("table") else {
                    // Table parts outside a table are ignored (their content flows into the parent).
                    return;
                };
                let row = (t + 1..self.stack.len()).find(|&i| self.name_at(i) == "tr");
                if name == "tr" {
                    self.stack.truncate(t + 1);
                } else if let Some(r) = row {
                    self.stack.truncate(r + 1);
                } else {
                    self.stack.truncate(t + 1);
                    self.insert_element("tr".to_owned(), Vec::new(), true);
                }
            }
            _ => {}
        }
        let void = is_void(&name);
        self.insert_element(name, attrs, !void);
    }

    fn end(&mut self, name: &str) {
        if is_structural(name) {
            return;
        }
        match name {
            "br" => {
                self.insert_element("br".to_owned(), Vec::new(), false);
                return;
            }
            "p" => {
                self.close_p();
                return;
            }
            _ => {}
        }
        let boundary: &[&str] = match name {
            "td" | "th" | "tr" => &["table"],
            "table" => &[],
            "li" => &["ul", "ol", "table", "td", "th"],
            _ => GENERAL_SCOPE,
        };
        let found = if is_heading(name) {
            // Any open heading closes with any heading end tag.
            (1..self.stack.len())
                .rev()
                .take_while(|&i| !boundary.contains(&self.name_at(i)))
                .find(|&i| is_heading(self.name_at(i)))
        } else {
            self.find_in_scope(name, boundary)
        };
        let Some(i) = found else { return };
        let mut reopen: Vec<(String, Vec<(String, String)>)> = Vec::new();
        if is_formatting(name) {
            let mut budget = self.reopen_budget;
            for &id in self.stack.get(i + 1..).unwrap_or(&[]) {
                if reopen.len() >= MAX_REOPEN {
                    break;
                }
                let Some(NodeData::Element { name, attrs }) = self.dom.node(id).map(|n| &n.data) else {
                    continue;
                };
                if !is_formatting(name) {
                    continue;
                }
                let mut kept = Vec::new();
                for (k, v) in attrs {
                    let cost = k.len().saturating_add(v.len());
                    if cost <= budget {
                        budget -= cost;
                        kept.push((k.clone(), v.clone()));
                    }
                }
                reopen.push((name.clone(), kept));
            }
            self.reopen_budget = budget;
        }
        self.stack.truncate(i);
        for (name, attrs) in reopen {
            self.insert_element(name, attrs, true);
        }
    }
}

/// Parses (already size-capped, newline-normalised) HTML into an arena tree.
pub(crate) fn parse(html: &str) -> Dom {
    let mut b = Builder {
        dom: Dom { nodes: vec![Node { data: NodeData::Root, children: Vec::new() }], body_attrs: Vec::new() },
        stack: vec![Dom::ROOT],
        reopen_budget: REOPEN_BUDGET,
    };
    for tok in Tokenizer::new(html) {
        match tok {
            Token::Start { name, attrs, .. } => b.start(name, attrs),
            Token::End { name } => b.end(&name),
            Token::Text(t) => b.text(&t),
            Token::Comment | Token::Doctype => {}
        }
    }
    b.dom
}

#[cfg(test)]
pub(crate) fn debug_tree(dom: &Dom) -> String {
    fn walk(dom: &Dom, id: NodeId, out: &mut String) {
        let Some(node) = dom.node(id) else { return };
        match &node.data {
            NodeData::Root => {
                for &c in &node.children {
                    walk(dom, c, out);
                }
            }
            NodeData::Text(t) => out.push_str(t),
            NodeData::Element { name, .. } => {
                out.push('<');
                out.push_str(name);
                out.push('>');
                for &c in &node.children {
                    walk(dom, c, out);
                }
                if !is_void(name) {
                    out.push_str("</");
                    out.push_str(name);
                    out.push('>');
                }
            }
        }
    }
    let mut out = String::new();
    walk(dom, Dom::ROOT, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(s: &str) -> String {
        debug_tree(&parse(s))
    }

    #[test]
    fn implied_end_tags() {
        assert_eq!(tree("<p>a<p>b<div>c</div>"), "<p>a</p><p>b</p><div>c</div>");
        assert_eq!(
            tree("<ul><li>a<li>b<ul><li>c</ul><li>d</ul>"),
            "<ul><li>a</li><li>b<ul><li>c</li></ul></li><li>d</li></ul>"
        );
        assert_eq!(tree("<dl><dt>a<dd>b<dt>c</dl>"), "<dl><dt>a</dt><dd>b</dd><dt>c</dt></dl>");
        assert_eq!(
            tree("<select><option>a<option>b</select>"),
            "<select><option>a</option><option>b</option></select>"
        );
        assert_eq!(tree("<h1>a<h2>b</h1>c"), "<h1>a</h1><h2>b</h2>c");
        assert_eq!(tree("<p>a<h3>b</h3>"), "<p>a</p><h3>b</h3>");
    }

    #[test]
    fn tables() {
        assert_eq!(
            tree("<table><tbody><tr><td>a<td>b<tr><th>c</table>d"),
            "<table><tr><td>a</td><td>b</td></tr><tr><th>c</th></tr></table>d"
        );
        assert_eq!(tree("<table><td>x</td></table>"), "<table><tr><td>x</td></tr></table>");
        assert_eq!(tree("<td>stray</td>"), "stray");
        assert_eq!(
            tree("<table><tr><td><table><tr><td>in</table>out</table>"),
            "<table><tr><td><table><tr><td>in</td></tr></table>out</td></tr></table>"
        );
        // </div> may not escape the cell.
        assert_eq!(
            tree("<div><table><tr><td>a</div>b</table>"),
            "<div><table><tr><td>ab</td></tr></table></div>"
        );
    }

    #[test]
    fn void_and_unmatched() {
        assert_eq!(tree("a<br>b</br>c<img src=x>d<hr>"), "a<br>b<br>c<img>d<hr>");
        assert_eq!(tree("</span>x</div></p>"), "x");
        assert_eq!(tree("<div/>x"), "<div>x</div>");
    }

    #[test]
    fn misnested_formatting() {
        assert_eq!(tree("<b>1<i>2</b>3</i>"), "<b>1<i>2</i></b><i>3</i>");
        assert_eq!(tree("<a href=x>1<a href=y>2"), "<a>1</a><a>2</a>");
        assert_eq!(tree("<p><b>x</p>y"), "<p><b>x</b></p>y");
    }

    #[test]
    fn body_attrs_and_structure_tags() {
        let d = parse(
            "<html><head><title>T</title></head><body bgcolor=red text=blue><body bgcolor=green>x</body></html>",
        );
        assert_eq!(d.body_attrs, vec![("bgcolor".into(), "red".into()), ("text".into(), "blue".into())]);
        assert_eq!(debug_tree(&d), "<title>T</title>x");
    }

    #[test]
    fn depth_cap() {
        let s = "<div>".repeat(10_000) + "deep" + &"</div>".repeat(10_000);
        let d = parse(&s);
        let mut depth = 0;
        let mut id = Dom::ROOT;
        while let Some(&c) = d.children(id).first() {
            id = c;
            depth += 1;
        }
        assert!(depth <= crate::MAX_DEPTH + 2, "{depth}");
        assert!(debug_tree(&d).contains("deep"));
    }
}
