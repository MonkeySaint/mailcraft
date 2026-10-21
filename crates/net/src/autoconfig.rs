//! Mail client autoconfiguration documents (`clientConfig` version 1.1, the XML format mail providers
//! publish at `https://autoconfig.<domain>/mail/config-v1.1.xml` and
//! `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`), read with a small XML parser of our own.
//!
//! Only what account setup needs is read: the incoming (IMAP, POP3) and outgoing (SMTP) servers in the
//! provider's order of preference, with host, port, socket type, user name (with the `%EMAILADDRESS%`,
//! `%EMAILLOCALPART%` and `%EMAILDOMAIN%` placeholders filled in) and authentication methods. The parser
//! handles elements, attributes, text, CDATA, comments, processing instructions and the predefined and
//! numeric character references; it does not expand DTD entities (a DOCTYPE is skipped). Input size, depth
//! and element count are capped.

use mailcraft_model::Security;

use crate::error::{NetError, Result};

/// Largest document accepted.
pub const MAX_DOCUMENT: usize = 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_ELEMENTS: usize = 20_000;

// ---------------------------------------------------------------------------------------------------------
// XML
// ---------------------------------------------------------------------------------------------------------

/// An XML element (names without their namespace prefix).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    /// The element's own text (concatenated, not trimmed).
    pub text: String,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    /// Child elements named `name`.
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }
    /// Trimmed text of the first child named `name`.
    pub fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.trim())
    }
}

fn xml_err(why: &str) -> NetError {
    NetError::Protocol(format!("invalid XML: {why}"))
}

fn local(name: &str) -> String {
    name.rsplit_once(':').map_or(name, |(_, l)| l).to_owned()
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
}

/// Replaces character references (`&lt;`, `&#65;`, `&#x41;` …). Unknown references are kept as written.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(rest.get(..i).unwrap_or_default());
        let after = rest.get(i..).unwrap_or_default();
        let semi = after.find(';').filter(|j| *j <= 12);
        let replaced = semi.and_then(|j| {
            let ent = after.get(1..j)?;
            let c = match ent {
                "lt" => '<',
                "gt" => '>',
                "amp" => '&',
                "quot" => '"',
                "apos" => '\'',
                _ => {
                    let n = if let Some(h) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                        u32::from_str_radix(h, 16).ok()?
                    } else {
                        ent.strip_prefix('#')?.parse::<u32>().ok()?
                    };
                    char::from_u32(n).filter(|c| *c != '\0')?
                }
            };
            Some((c, j))
        });
        match replaced {
            Some((c, j)) => {
                out.push(c);
                rest = after.get(j + 1..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = after.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parses a document and returns its root element.
pub fn parse_xml(input: &str) -> Result<Element> {
    parse_xml_with(input, MAX_DOCUMENT, MAX_ELEMENTS)
}

/// [`parse_xml`] with other caps on the document size and the element count (WebDAV listings are larger
/// than autoconfig documents).
pub fn parse_xml_with(input: &str, max_bytes: usize, max_elements: usize) -> Result<Element> {
    if input.len() > max_bytes {
        return Err(NetError::TooLarge("XML document".into()));
    }
    let s = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut count = 0usize;
    let mut pos = 0usize;
    while pos < s.len() {
        let rest = s.get(pos..).unwrap_or_default();
        let Some(lt) = rest.find('<') else {
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&unescape(rest));
            }
            break;
        };
        if lt > 0 {
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&unescape(rest.get(..lt).unwrap_or_default()));
            }
            pos += lt;
            continue;
        }
        if let Some(r) = rest.strip_prefix("<!--") {
            let end = r.find("-->").ok_or_else(|| xml_err("unterminated comment"))?;
            pos += 4 + end + 3;
            continue;
        }
        if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let end = r.find("]]>").ok_or_else(|| xml_err("unterminated CDATA"))?;
            if let Some(top) = stack.last_mut() {
                top.text.push_str(r.get(..end).unwrap_or_default());
            }
            pos += 9 + end + 3;
            continue;
        }
        if let Some(r) = rest.strip_prefix("<?") {
            let end = r.find("?>").ok_or_else(|| xml_err("unterminated processing instruction"))?;
            pos += 2 + end + 2;
            continue;
        }
        if let Some(r) = rest.strip_prefix("<!") {
            // DOCTYPE (with an optional internal subset): skipped, entities are not expanded.
            let end = match r.find('[') {
                Some(b) if r.find('>').is_none_or(|g| b < g) => {
                    r.find("]>").map(|e| e + 2).ok_or_else(|| xml_err("unterminated DOCTYPE"))?
                }
                _ => r.find('>').map(|e| e + 1).ok_or_else(|| xml_err("unterminated declaration"))?,
            };
            pos += 2 + end;
            continue;
        }
        if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>').ok_or_else(|| xml_err("unterminated end tag"))?;
            let name = local(r.get(..end).unwrap_or_default().trim());
            let el = stack.pop().ok_or_else(|| xml_err("end tag without start tag"))?;
            if el.name != name {
                return Err(xml_err(&format!("</{name}> closes <{}>", el.name)));
            }
            match stack.last_mut() {
                Some(parent) => parent.children.push(el),
                None => root = Some(el),
            }
            pos += 2 + end + 1;
            if root.is_some() {
                break;
            }
            continue;
        }
        // Start tag.
        let body = rest.get(1..).unwrap_or_default();
        let name_len = body.find(|c: char| !is_name_char(c)).unwrap_or(body.len());
        let name = body.get(..name_len).unwrap_or_default();
        if name.is_empty() {
            return Err(xml_err("bad tag"));
        }
        let mut el = Element { name: local(name), ..Default::default() };
        let mut i = 1 + name_len;
        let self_closing;
        loop {
            let r = rest.get(i..).unwrap_or_default();
            let trimmed = r.trim_start();
            i += r.len() - trimmed.len();
            if trimmed.starts_with("/>") {
                self_closing = true;
                i += 2;
                break;
            }
            if trimmed.starts_with('>') {
                self_closing = false;
                i += 1;
                break;
            }
            if trimmed.is_empty() {
                return Err(xml_err("unterminated start tag"));
            }
            let an_len = trimmed.find(|c: char| !is_name_char(c)).unwrap_or(trimmed.len());
            if an_len == 0 {
                return Err(xml_err("bad attribute"));
            }
            let an = trimmed.get(..an_len).unwrap_or_default();
            let after = trimmed.get(an_len..).unwrap_or_default();
            let after_ws = after.trim_start();
            let eq = after_ws.strip_prefix('=').ok_or_else(|| xml_err("attribute without value"))?;
            let v = eq.trim_start();
            let quote = v
                .chars()
                .next()
                .filter(|q| *q == '"' || *q == '\'')
                .ok_or_else(|| xml_err("unquoted attribute"))?;
            let vbody = v.get(1..).unwrap_or_default();
            let close = vbody.find(quote).ok_or_else(|| xml_err("unterminated attribute"))?;
            el.attrs.push((local(an), unescape(vbody.get(..close).unwrap_or_default())));
            let consumed = trimmed.len() - vbody.len() + close + 1;
            i += consumed;
        }
        pos += i;
        count += 1;
        if count > max_elements {
            return Err(NetError::TooLarge("too many XML elements".into()));
        }
        if self_closing {
            match stack.last_mut() {
                Some(parent) => parent.children.push(el),
                None => {
                    root = Some(el);
                    break;
                }
            }
        } else {
            if stack.len() >= MAX_DEPTH {
                return Err(NetError::TooLarge("XML nested too deeply".into()));
            }
            stack.push(el);
        }
    }
    if !stack.is_empty() {
        return Err(xml_err("unclosed elements"));
    }
    root.ok_or_else(|| xml_err("no root element"))
}

// ---------------------------------------------------------------------------------------------------------
// clientConfig
// ---------------------------------------------------------------------------------------------------------

/// Which protocol a server speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Protocol {
    Imap,
    Pop3,
    Smtp,
    Jmap,
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Protocol::Imap => "imap",
            Protocol::Pop3 => "pop3",
            Protocol::Smtp => "smtp",
            Protocol::Jmap => "jmap",
        }
    }
}

/// One server from a discovery source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    pub protocol: Protocol,
    /// Host name (JMAP: the session URL).
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// User name to sign in with (placeholders filled in); empty = the address.
    pub username: String,
    /// The document's authentication methods (`password-cleartext`, `OAuth2`, …), in its order.
    pub auth: Vec<String>,
    /// The server is in the address's domain (or the provider's own configuration said so). RFC 6186
    /// section 6: servers found elsewhere are only used after the user confirmed them.
    pub verified: bool,
}

impl ServerConfig {
    /// The provider signs in with OAuth 2.0 (only or as an option).
    pub fn offers_oauth(&self) -> bool {
        self.auth.iter().any(|a| a.eq_ignore_ascii_case("OAuth2"))
    }
    /// Only OAuth 2.0 is listed (a password will not work).
    pub fn oauth_only(&self) -> bool {
        !self.auth.is_empty() && self.auth.iter().all(|a| a.eq_ignore_ascii_case("OAuth2"))
    }
}

/// The parts of a `clientConfig` document account setup uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoConfig {
    pub display_name: String,
    /// Domains the provider serves.
    pub domains: Vec<String>,
    /// Incoming servers in the provider's order of preference.
    pub incoming: Vec<ServerConfig>,
    pub outgoing: Vec<ServerConfig>,
}

impl AutoConfig {
    /// The first incoming server of `protocol`.
    pub fn incoming_of(&self, protocol: Protocol) -> Option<&ServerConfig> {
        self.incoming.iter().find(|s| s.protocol == protocol)
    }
}

/// Fills in the autoconfig placeholders for `address`.
pub fn expand_placeholders(s: &str, address: &str) -> String {
    let (local, domain) = address.rsplit_once('@').unwrap_or((address, ""));
    s.replace("%EMAILADDRESS%", address).replace("%EMAILLOCALPART%", local).replace("%EMAILDOMAIN%", domain)
}

/// A host name as found in a document: letters, digits, `-` and `.` only.
pub fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && !h.starts_with(['.', '-'])
        && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        && !h.contains("..")
}

fn security_of(socket_type: &str) -> Option<Security> {
    match socket_type.trim().to_ascii_uppercase().as_str() {
        "SSL" | "TLS" => Some(Security::Tls),
        "STARTTLS" => Some(Security::StartTls),
        "PLAIN" => Some(Security::None),
        _ => None,
    }
}

fn default_port(p: Protocol, sec: Security) -> u16 {
    match (p, sec) {
        (Protocol::Imap, Security::Tls) => 993,
        (Protocol::Imap, _) => 143,
        (Protocol::Pop3, Security::Tls) => 995,
        (Protocol::Pop3, _) => 110,
        (Protocol::Smtp, Security::Tls) => 465,
        (Protocol::Smtp, _) => 587,
        (Protocol::Jmap, _) => 443,
    }
}

fn server_from(el: &Element, address: &str) -> Option<ServerConfig> {
    let protocol = match el.attr("type")?.trim().to_ascii_lowercase().as_str() {
        "imap" => Protocol::Imap,
        "pop3" => Protocol::Pop3,
        "smtp" => Protocol::Smtp,
        _ => return None,
    };
    let host = expand_placeholders(el.child_text("hostname")?, address).to_ascii_lowercase();
    if !valid_host(&host) {
        return None;
    }
    let security = security_of(el.child_text("socketType").unwrap_or("SSL"))?;
    let port = el
        .child_text("port")
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or_else(|| default_port(protocol, security));
    let username = el
        .child_text("username")
        .map(|u| expand_placeholders(u, address))
        .filter(|u| !u.chars().any(char::is_control) && u.chars().count() <= 512)
        .unwrap_or_default();
    let auth = el.all("authentication").map(|a| a.text.trim().to_owned()).filter(|a| !a.is_empty()).collect();
    Some(ServerConfig { protocol, host, port, security, username, auth, verified: true })
}

/// Reads a `clientConfig` document for `address`. Servers with unusable entries are skipped; a document
/// without any usable server is an error.
pub fn parse_autoconfig(xml: &[u8], address: &str) -> Result<AutoConfig> {
    if xml.len() > MAX_DOCUMENT {
        return Err(NetError::TooLarge("autoconfig document".into()));
    }
    let text = std::str::from_utf8(xml).map_err(|_| xml_err("not UTF-8"))?;
    let root = parse_xml(text)?;
    if root.name != "clientConfig" {
        return Err(NetError::Protocol(format!("not a clientConfig document (<{}>)", root.name)));
    }
    let provider =
        root.child("emailProvider").ok_or_else(|| NetError::Protocol("no emailProvider".into()))?;
    let mut cfg = AutoConfig {
        display_name: provider
            .child_text("displayName")
            .filter(|d| !d.chars().any(char::is_control))
            .map(|d| d.chars().take(200).collect())
            .unwrap_or_default(),
        domains: provider
            .all("domain")
            .map(|d| d.text.trim().to_ascii_lowercase())
            .filter(|d| valid_host(d))
            .collect(),
        ..Default::default()
    };
    for s in provider.all("incomingServer").filter_map(|e| server_from(e, address)) {
        if matches!(s.protocol, Protocol::Imap | Protocol::Pop3) {
            cfg.incoming.push(s);
        }
    }
    for s in provider.all("outgoingServer").filter_map(|e| server_from(e, address)) {
        if s.protocol == Protocol::Smtp {
            cfg.outgoing.push(s);
        }
    }
    if cfg.incoming.is_empty() && cfg.outgoing.is_empty() {
        return Err(NetError::Protocol("the autoconfig document lists no usable server".into()));
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE clientConfig [ <!ENTITY x "y"> ]>
<!-- a provider -->
<clientConfig version="1.1">
  <emailProvider id="example.com">
    <domain>example.com</domain>
    <domain>Example.NET</domain>
    <displayName>Example &amp; Co</displayName>
    <incomingServer type="imap">
      <hostname>imap.%EMAILDOMAIN%</hostname>
      <port>993</port>
      <socketType>SSL</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
      <authentication>OAuth2</authentication>
    </incomingServer>
    <incomingServer type='pop3'>
      <hostname>pop.example.com</hostname>
      <port>110</port>
      <socketType>STARTTLS</socketType>
      <username>%EMAILLOCALPART%</username>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <incomingServer type="exchange"><hostname>x.example.com</hostname></incomingServer>
    <outgoingServer type="smtp">
      <hostname><![CDATA[smtp.example.com]]></hostname>
      <port>587</port>
      <socketType>STARTTLS</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
    </outgoingServer>
    <documentation url="https://example.com/help"><descr lang="en">Help</descr></documentation>
  </emailProvider>
</clientConfig>"#;

    #[test]
    fn reads_a_provider_document() {
        let c = parse_autoconfig(DOC.as_bytes(), "kim@example.com").unwrap();
        assert_eq!(c.display_name, "Example & Co");
        assert_eq!(c.domains, ["example.com", "example.net"]);
        assert_eq!(c.incoming.len(), 2, "exchange is skipped");
        let imap = c.incoming_of(Protocol::Imap).unwrap();
        assert_eq!((imap.host.as_str(), imap.port, imap.security), ("imap.example.com", 993, Security::Tls));
        assert_eq!(imap.username, "kim@example.com");
        assert!(imap.offers_oauth() && !imap.oauth_only());
        let pop = c.incoming_of(Protocol::Pop3).unwrap();
        assert_eq!((pop.port, pop.security, pop.username.as_str()), (110, Security::StartTls, "kim"));
        let smtp = &c.outgoing[0];
        assert_eq!(
            (smtp.host.as_str(), smtp.port, smtp.security),
            ("smtp.example.com", 587, Security::StartTls)
        );
    }

    #[test]
    fn xml_basics() {
        let e = parse_xml("<a x='1' y=\"&lt;2&#62;\"><b/><c>t&#x41;<d>u</d>v</c></a>").unwrap();
        assert_eq!(e.attr("y"), Some("<2>"));
        assert_eq!(e.children.len(), 2);
        assert_eq!(e.child("c").unwrap().text, "tAv");
        assert_eq!(parse_xml("<ns:a xmlns:ns='u'><ns:b>x</ns:b></ns:a>").unwrap().child_text("b"), Some("x"));
        assert_eq!(unescape("a &bogus; &#0; & b"), "a &bogus; &#0; & b");
        for bad in [
            "",
            "<a>",
            "<a></b>",
            "</a>",
            "<a x=1></a>",
            "<a x='1></a>",
            "<!-- x",
            "<a><![CDATA[x</a>",
            "< a/>",
        ] {
            assert!(parse_xml(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn caps_and_junk() {
        let deep = "<a>".repeat(MAX_DEPTH + 1) + &"</a>".repeat(MAX_DEPTH + 1);
        assert!(matches!(parse_xml(&deep), Err(NetError::TooLarge(_))));
        assert!(parse_autoconfig(b"<other/>", "a@example.com").is_err());
        assert!(parse_autoconfig(b"<clientConfig><emailProvider/></clientConfig>", "a@example.com").is_err());
        assert!(parse_autoconfig(&[0xff, 0xfe], "a@example.com").is_err());
        // Bad host names and ports are not used.
        let doc = "<clientConfig><emailProvider><incomingServer type='imap'><hostname>evil host</hostname>\
                   </incomingServer><outgoingServer type='smtp'><hostname>smtp.example.com</hostname>\
                   <port>99999</port><socketType>TLS</socketType></outgoingServer></emailProvider></clientConfig>";
        let c = parse_autoconfig(doc.as_bytes(), "a@example.com").unwrap();
        assert!(c.incoming.is_empty());
        assert_eq!(c.outgoing[0].port, 465, "an invalid port falls back to the default");
        // Every prefix of a real document: an error or a document, never a panic.
        for n in 0..DOC.len() {
            if let Some(p) = DOC.get(..n) {
                let _ = parse_autoconfig(p.as_bytes(), "a@example.com");
            }
        }
    }
}
