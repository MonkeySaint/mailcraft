//! IMAP NAMESPACE (RFC 2342, part of IMAP4rev2): where the personal mailboxes, other users' mailboxes and
//! shared mailboxes live on the server.
//!
//! The response is `* NAMESPACE <personal> <other users> <shared>`, each either `NIL` or a parenthesised
//! list of `("prefix" "delimiter" [extension …])`. Prefixes come back as written on the wire (modified
//! UTF-7 unless UTF8=ACCEPT is on); the client decodes them.

/// One namespace: a mailbox name prefix and its hierarchy delimiter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Namespace {
    pub prefix: String,
    pub delimiter: Option<char>,
}

/// The three namespace groups.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Namespaces {
    pub personal: Vec<Namespace>,
    pub other_users: Vec<Namespace>,
    pub shared: Vec<Namespace>,
}

impl Namespaces {
    /// What a server without NAMESPACE has: one personal namespace with an empty prefix.
    pub fn personal_only(delimiter: Option<char>) -> Namespaces {
        Namespaces { personal: vec![Namespace { prefix: String::new(), delimiter }], ..Default::default() }
    }

    /// Prefixes of the other users' and shared namespaces (empty prefixes left out).
    pub fn foreign_prefixes(&self) -> Vec<&Namespace> {
        self.other_users.iter().chain(&self.shared).filter(|n| !n.prefix.is_empty()).collect()
    }
}

/// Most namespaces kept per group.
const MAX_PER_GROUP: usize = 32;
/// Deepest nesting followed (extensions may nest lists).
const MAX_DEPTH: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Open,
    Close,
    Nil,
    Str(String),
    Atom,
}

fn tokens(s: &str) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '(' => out.push(Tok::Open),
            ')' => out.push(Tok::Close),
            '"' => {
                let mut v = String::new();
                loop {
                    match it.next()? {
                        '\\' => v.push(it.next()?),
                        '"' => break,
                        ch => v.push(ch),
                    }
                }
                out.push(Tok::Str(v));
            }
            c if c.is_whitespace() => {}
            _ => {
                let mut atom = String::from(c);
                while let Some(&n) = it.peek() {
                    if n.is_whitespace() || n == '(' || n == ')' {
                        break;
                    }
                    atom.push(n);
                    it.next();
                }
                out.push(if atom.eq_ignore_ascii_case("NIL") { Tok::Nil } else { Tok::Atom });
            }
        }
        if out.len() > 10_000 {
            return None;
        }
    }
    Some(out)
}

/// One group: `NIL` or `( (prefix delim ext…) … )`. Advances `i`.
fn group(t: &[Tok], i: &mut usize) -> Option<Vec<Namespace>> {
    match t.get(*i)? {
        Tok::Nil => {
            *i += 1;
            return Some(Vec::new());
        }
        Tok::Open => *i += 1,
        _ => return None,
    }
    let mut out = Vec::new();
    loop {
        match t.get(*i)? {
            Tok::Close => {
                *i += 1;
                return Some(out);
            }
            Tok::Open => {
                *i += 1;
                let Tok::Str(prefix) = t.get(*i)?.clone() else { return None };
                *i += 1;
                let delimiter = match t.get(*i)? {
                    Tok::Str(d) => d.chars().next(),
                    Tok::Nil => None,
                    _ => return None,
                };
                *i += 1;
                // Skip extensions up to the descriptor's closing parenthesis.
                let mut depth = 0usize;
                loop {
                    match t.get(*i)? {
                        Tok::Open => {
                            depth += 1;
                            if depth > MAX_DEPTH {
                                return None;
                            }
                        }
                        Tok::Close if depth == 0 => break,
                        Tok::Close => depth -= 1,
                        _ => {}
                    }
                    *i += 1;
                }
                *i += 1;
                if out.len() < MAX_PER_GROUP {
                    out.push(Namespace { prefix, delimiter });
                }
            }
            _ => return None,
        }
    }
}

/// Parses a `* NAMESPACE …` response line. `None` when it is not one or is malformed.
pub fn parse(line: &str) -> Option<Namespaces> {
    let rest = line.trim().strip_prefix('*').unwrap_or(line).trim_start();
    let head = rest.get(..9)?;
    if !head.eq_ignore_ascii_case("NAMESPACE") {
        return None;
    }
    let t = tokens(rest.get(9..)?)?;
    let mut i = 0usize;
    let personal = group(&t, &mut i)?;
    let other_users = group(&t, &mut i)?;
    let shared = group(&t, &mut i)?;
    Some(Namespaces { personal, other_users, shared })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_2342_examples() {
        let n = parse(r#"* NAMESPACE (("" "/")) NIL NIL"#).unwrap();
        assert_eq!(n, Namespaces::personal_only(Some('/')));
        let n = parse(r#"* NAMESPACE (("" "/")) (("Other Users/" "/")) (("Shared/" "/"))"#).unwrap();
        assert_eq!(n.other_users[0].prefix, "Other Users/");
        assert_eq!(n.shared[0].delimiter, Some('/'));
        assert_eq!(n.foreign_prefixes().len(), 2);
        let n = parse(r##"* NAMESPACE (("INBOX." ".")) (("user." ".")) (("#public." ".")("#ftp/" "/"))"##)
            .unwrap();
        assert_eq!(n.personal[0].prefix, "INBOX.");
        assert_eq!(n.shared.len(), 2);
        // Extensions are skipped; NIL delimiters are kept as none.
        let n = parse(r#"* NAMESPACE (("" "/" "X-PARAM" ("FLAG1" "FLAG2"))) NIL (("Public" NIL))"#).unwrap();
        assert_eq!(n.personal.len(), 1);
        assert_eq!(n.shared[0], Namespace { prefix: "Public".into(), delimiter: None });
        let n = parse(r#"* namespace (("~\"x\\" "/")) nil nil"#).unwrap();
        assert_eq!(n.personal[0].prefix, "~\"x\\");
    }

    #[test]
    fn junk_is_refused() {
        for bad in [
            "* NAMESPACE",
            "* NAMESPACE ((",
            "* NAMESPACE (\"x\") NIL NIL",
            "* LIST () \"/\" INBOX",
            "* NAMESPACE (((((((((((\"\" \"/\" ((((((((((((",
            "",
        ] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }
}
