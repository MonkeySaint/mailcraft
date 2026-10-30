//! Exchange Web Services (EWS): a SOAP client for on-premises Exchange-compatible servers, from the public
//! Microsoft Open Specifications: [MS-OXWSCORE] (items: GetItem, CreateItem, UpdateItem, MoveItem,
//! DeleteItem, FindItem), [MS-OXWSFOLD] (GetFolder, FindFolder, CreateFolder, UpdateFolder, MoveFolder,
//! DeleteFolder), [MS-OXWSSYNC] (SyncFolderHierarchy, SyncFolderItems), [MS-OXWSMSG] (messages, MimeContent,
//! SendAndSaveCopy), [MS-OXWSMTGS] (AcceptItem, DeclineItem, TentativelyAcceptItem), [MS-OXWSOOF] (Get/Set
//! UserOofSettings), [MS-OXWAVLS] (GetUserAvailability), with the item types of [MS-OXWSCAL], [MS-OXWSCONT]
//! and [MS-OXWSTASK]. Autodiscover ([MS-OXDSCLI], plain-old-XML) is in [`autodiscover`]; NTLM ([MS-NLMP]) in
//! [`ntlm`].
//!
//! **Requests** are SOAP 1.1 envelopes POSTed to the EWS URL (`https://host/EWS/Exchange.asmx`) with a
//! `RequestServerVersion` of Exchange 2010 SP2, which every server this targets understands. **Sign-in**:
//! HTTP Basic, NTLM (a handshake on a kept-alive connection, then the connection stays signed in), or an
//! OAuth bearer token. `Auto` starts with Basic and switches to NTLM when the server offers only that.
//!
//! **Hostile input.** Responses are capped ([`MAX_RESPONSE`]) and parsed with the crate's small XML reader
//! (bounded depth and element count, no entity expansion). Every value read from a response is optional: a
//! missing or malformed element gives an error or is skipped, never a panic.

pub mod autodiscover;
pub mod ntlm;

use std::cell::RefCell;

use base64::Engine;

use crate::autoconfig::{Element, parse_xml_with};
use crate::error::{NetError, Result};
use crate::http::{HttpClient, HttpResponse, KeepAlive};

/// Largest response read (a GetItem with the MIME content of a large message).
pub const MAX_RESPONSE: usize = 96 * 1024 * 1024;
/// Most XML elements in one response.
const MAX_ELEMENTS: usize = 1_000_000;
/// Items per GetItem with MIME content (bounds one response).
pub const MIME_BATCH: usize = 10;
/// Message bytes (by the items' `Size`) per GetItem with MIME content: with base64 well below
/// [`MAX_RESPONSE`].
pub const MIME_BATCH_BYTES: u64 = 16 * 1024 * 1024;
/// Items per GetItem without content, per UpdateItem, MoveItem or DeleteItem.
pub const ITEM_BATCH: usize = 100;
/// Folders asked for per FindFolder page.
const FOLDER_PAGE: usize = 500;
/// Most folders listed.
pub const MAX_FOLDERS: usize = 5_000;
/// Most items per FindItem page.
const FIND_PAGE: usize = 200;

/// The SOAP envelope namespace and the EWS types and messages namespaces.
const SOAP_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";
pub const TYPES_NS: &str = "http://schemas.microsoft.com/exchange/services/2006/types";
pub const MESSAGES_NS: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";
/// The server version requested.
const VERSION: &str = "Exchange2010_SP2";

/// The distinguished folder ids MailCraft uses ([MS-OXWSCDATA] DistinguishedFolderIdNameType).
pub const DISTINGUISHED: [&str; 12] = [
    "msgfolderroot",
    "inbox",
    "drafts",
    "sentitems",
    "deleteditems",
    "junkemail",
    "outbox",
    "archive",
    "calendar",
    "contacts",
    "tasks",
    "notes",
];

/// PidTagFlagStatus ([MS-OXPROPS]): 0 none, 1 complete, 2 flagged.
pub const FLAG_STATUS: &str = r#"<t:ExtendedFieldURI PropertyTag="0x1090" PropertyType="Integer"/>"#;
/// PidTagLastVerbExecuted: 102 reply, 103 reply to all, 104 forward.
pub const LAST_VERB: &str = r#"<t:ExtendedFieldURI PropertyTag="0x1081" PropertyType="Integer"/>"#;
/// PidTagMessageFlags: bit 1 read, bit 8 unsent (a draft).
pub const MESSAGE_FLAGS: &str = r#"<t:ExtendedFieldURI PropertyTag="0x0E07" PropertyType="Integer"/>"#;
/// MailCraft's own UID of a contact, task or event made here (a named string property), so its address
/// stays the same across runs.
pub const ITEM_UID: &str = r#"<t:ExtendedFieldURI DistinguishedPropertySetId="PublicStrings" PropertyName="MailCraftUid" PropertyType="String"/>"#;

/// XML-escapes text and attribute values.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Characters XML 1.0 cannot carry are dropped.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// A folder reference: a distinguished folder by name, any other by its server id.
pub fn folder_ref(id: &str) -> String {
    if DISTINGUISHED.contains(&id) {
        format!(r#"<t:DistinguishedFolderId Id="{id}"/>"#)
    } else {
        format!(r#"<t:FolderId Id="{}"/>"#, esc(id))
    }
}

/// An item reference.
pub fn item_ref(id: &str) -> String {
    format!(r#"<t:ItemId Id="{}"/>"#, esc(id))
}

/// How the client signs in.
#[derive(Clone, PartialEq, Eq)]
pub enum EwsAuth {
    Basic {
        user: String,
        password: String,
    },
    Ntlm {
        user: String,
        password: String,
    },
    /// Basic first; NTLM when the server refuses Basic and offers NTLM.
    Auto {
        user: String,
        password: String,
    },
    /// An OAuth 2.0 access token.
    Bearer(String),
}

impl std::fmt::Debug for EwsAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (kind, user) = match self {
            EwsAuth::Basic { user, .. } => ("Basic", user.as_str()),
            EwsAuth::Ntlm { user, .. } => ("Ntlm", user.as_str()),
            EwsAuth::Auto { user, .. } => ("Auto", user.as_str()),
            EwsAuth::Bearer(_) => ("Bearer", ""),
        };
        write!(f, "EwsAuth::{kind}({user})")
    }
}

impl EwsAuth {
    /// From a mechanism name ("", "AUTO", "BASIC", "NTLM", "BEARER"/"XOAUTH2" for a token).
    pub fn from_mechanism(mechanism: &str, user: &str, secret: &str) -> EwsAuth {
        let (user, password) = (user.to_owned(), secret.to_owned());
        match mechanism.trim().to_ascii_uppercase().as_str() {
            "BASIC" | "PLAIN" | "LOGIN" => EwsAuth::Basic { user, password },
            "NTLM" => EwsAuth::Ntlm { user, password },
            "BEARER" | "XOAUTH2" | "OAUTHBEARER" | "OAUTH" => EwsAuth::Bearer(password),
            _ => EwsAuth::Auto { user, password },
        }
    }
}

/// The connection state behind a client: the kept-alive connection and how it signed in.
#[derive(Debug, Default)]
struct Link {
    keep: KeepAlive,
    /// Auto switched to NTLM.
    use_ntlm: bool,
    /// The open connection completed an NTLM handshake.
    ntlm_signed_in: bool,
}

/// Signs requests in and sends them (shared by EWS calls and Autodiscover).
#[derive(Debug)]
pub struct Signer {
    pub auth: EwsAuth,
    link: RefCell<Link>,
}

fn status_error(resp: &HttpResponse) -> NetError {
    match resp.status {
        401 | 403 => NetError::Auth(format!("the server refused the sign-in (HTTP {})", resp.status)),
        s => NetError::Http { status: s, body: resp.text_snippet() },
    }
}

/// The schemes a 401 answer offers (lower-case words of every `WWW-Authenticate` header).
fn offered(resp: &HttpResponse) -> Vec<String> {
    resp.headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("www-authenticate"))
        .filter_map(|(_, v)| {
            v.split_whitespace().next().map(|w| w.trim_end_matches(',').to_ascii_lowercase())
        })
        .collect()
}

impl Signer {
    pub fn new(auth: EwsAuth) -> Signer {
        Signer { auth, link: RefCell::new(Link::default()) }
    }

    /// Closes the kept connection (the next request signs in again).
    pub fn reset(&self) {
        if let Ok(mut l) = self.link.try_borrow_mut() {
            l.keep.close();
            l.ntlm_signed_in = false;
        }
    }

    /// POSTs `body` to `url` signed in; the response whatever its status (after the sign-in exchange).
    pub fn post(
        &self,
        http: &HttpClient,
        url: &str,
        content_type: &str,
        body: &[u8],
        extra: &[(&str, &str)],
    ) -> Result<HttpResponse> {
        let mut guard =
            self.link.try_borrow_mut().map_err(|_| NetError::Invalid("EWS client in use".into()))?;
        let link: &mut Link = &mut guard;
        let mut headers: Vec<(String, String)> =
            vec![("Content-Type".into(), content_type.into()), ("Accept".into(), "text/xml".into())];
        headers.extend(extra.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        let send = |link: &mut Link, auth: Option<String>| -> Result<HttpResponse> {
            let mut h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            if let Some(a) = auth.as_deref() {
                h.push(("Authorization", a));
            }
            http.request_kept(&mut link.keep, "POST", url, &h, Some(body))
        };
        let basic = |user: &str, password: &str| {
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
            )
        };
        match &self.auth {
            EwsAuth::Bearer(token) => send(link, Some(format!("Bearer {token}"))),
            EwsAuth::Basic { user, password } => send(link, Some(basic(user, password))),
            EwsAuth::Auto { user, password } if !link.use_ntlm => {
                let resp = send(link, Some(basic(user, password)))?;
                if resp.status == 401 {
                    let schemes = offered(&resp);
                    if schemes.iter().any(|s| s == "ntlm" || s == "negotiate")
                        && !schemes.iter().any(|s| s == "basic")
                    {
                        link.use_ntlm = true;
                        return self.ntlm(link, user, password, send);
                    }
                }
                Ok(resp)
            }
            EwsAuth::Auto { user, password } | EwsAuth::Ntlm { user, password } => {
                self.ntlm(link, user, password, send)
            }
        }
    }

    /// One request over a connection signed in with NTLM (the handshake first when the connection is new).
    fn ntlm(
        &self,
        link: &mut Link,
        user: &str,
        password: &str,
        send: impl Fn(&mut Link, Option<String>) -> Result<HttpResponse>,
    ) -> Result<HttpResponse> {
        if link.ntlm_signed_in && link.keep.is_open() {
            let resp = send(link, None)?;
            if resp.status != 401 {
                return Ok(resp);
            }
        }
        link.ntlm_signed_in = false;
        link.keep.close();
        let first = send(link, Some(format!("NTLM {}", ntlm::negotiate())))?;
        if first.status != 401 {
            // Signed in without a challenge (or refused outright): nothing more to do.
            return Ok(first);
        }
        let challenge = first
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("www-authenticate"))
            .find_map(|(_, v)| ntlm::challenge_token(v).map(str::to_owned))
            .ok_or_else(|| NetError::Auth("the server does not offer NTLM sign-in".into()))?;
        if !link.keep.is_open() {
            return Err(NetError::Auth("the server closed the connection during the NTLM sign-in".into()));
        }
        let token = ntlm::authenticate(&challenge, user, password)?;
        let resp = send(link, Some(format!("NTLM {token}")))?;
        link.ntlm_signed_in = resp.status != 401 && link.keep.is_open();
        Ok(resp)
    }
}

/// A folder as listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EwsFolder {
    pub id: String,
    pub change_key: String,
    pub parent: Option<String>,
    pub name: String,
    /// `IPF.Note` for mail, `IPF.Appointment`, `IPF.Contact`, `IPF.Task`, … (empty: unknown, treated as mail).
    pub class: String,
    pub total: Option<u64>,
    pub unread: Option<u64>,
    /// Subfolders directly below (`None`: not reported).
    pub children: Option<u64>,
    /// The element name (`Folder`, `CalendarFolder`, `ContactsFolder`, `TasksFolder`, `SearchFolder`).
    pub kind: String,
}

impl EwsFolder {
    /// True for a folder of mail (or of unknown class, as some servers leave the class out).
    pub fn is_mail(&self) -> bool {
        let c = self.class.to_ascii_lowercase();
        self.kind == "Folder" && (c.is_empty() || c == "ipf" || c.starts_with("ipf.note"))
    }
}

/// What an item's state is on the server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EwsItem {
    pub id: String,
    pub change_key: String,
    pub folder: Option<String>,
    pub read: Option<bool>,
    /// PidTagFlagStatus (0 none, 1 complete, 2 flagged).
    pub flag_status: Option<i64>,
    /// PidTagLastVerbExecuted.
    pub last_verb: Option<i64>,
    pub categories: Vec<String>,
    pub size: Option<u64>,
    /// `DateTimeReceived` as sent (xs:dateTime).
    pub received: Option<String>,
    pub item_class: String,
    pub message_id: Option<String>,
    /// The message source (MimeContent), when asked for.
    pub mime: Option<Vec<u8>>,
}

/// Splits messages (id, size) into GetItem batches: at most [`MIME_BATCH`] items and, unless one item alone is
/// larger, [`MIME_BATCH_BYTES`] (an unknown size counts as 0).
pub fn mime_batches(items: &[(String, Option<u64>)]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut bytes = 0u64;
    for (id, size) in items {
        let n = size.unwrap_or(0);
        if !cur.is_empty() && (cur.len() >= MIME_BATCH || bytes.saturating_add(n) > MIME_BATCH_BYTES) {
            out.push(std::mem::take(&mut cur));
            bytes = 0;
        }
        bytes = bytes.saturating_add(n);
        cur.push(id.clone());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// One change SyncFolderItems reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemChange {
    Create(EwsItem),
    Update(EwsItem),
    Delete(String),
    ReadFlag { id: String, read: bool },
}

/// A page of folder item changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncItems {
    pub changes: Vec<ItemChange>,
    pub state: String,
    /// No more changes after these.
    pub last: bool,
}

/// What to change on a message's flags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlagChange {
    pub read: Option<bool>,
    /// PidTagFlagStatus to set (0 removes the flag).
    pub flag_status: Option<i64>,
    pub categories: Option<Vec<String>>,
    /// PidTagLastVerbExecuted to set.
    pub last_verb: Option<i64>,
}

/// Automatic replies ([MS-OXWSOOF]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OofSettings {
    /// `Disabled`, `Enabled` or `Scheduled`.
    pub state: String,
    /// `None`, `Known` or `All`.
    pub external_audience: String,
    /// The scheduled window (xs:dateTime), for `Scheduled`.
    pub start: Option<String>,
    pub end: Option<String>,
    pub internal_reply: String,
    pub external_reply: String,
}

/// One busy period of a mailbox ([MS-OXWAVLS] CalendarEvent).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BusyBlock {
    pub start: String,
    pub end: String,
    /// `Free`, `Tentative`, `Busy`, `OOF`, `WorkingElsewhere`, `NoData`.
    pub busy_type: String,
}

/// How a meeting request is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeetingReply {
    Accept,
    Tentative,
    Decline,
}

impl MeetingReply {
    fn element(self) -> &'static str {
        match self {
            MeetingReply::Accept => "AcceptItem",
            MeetingReply::Tentative => "TentativelyAcceptItem",
            MeetingReply::Decline => "DeclineItem",
        }
    }
}

/// The error of a failed response message, `Ok` for success and warnings.
pub fn check(msg: &Element) -> Result<()> {
    if msg.attr("ResponseClass").is_some_and(|c| c.eq_ignore_ascii_case("Error")) {
        let code = msg.child_text("ResponseCode").unwrap_or("Error").to_owned();
        let message = msg.child_text("MessageText").unwrap_or_default().chars().take(500).collect();
        return Err(NetError::Ews { code, message });
    }
    Ok(())
}

/// True when `e` is the EWS error `code`.
pub fn is_code(e: &NetError, code: &str) -> bool {
    matches!(e, NetError::Ews { code: c, .. } if c == code)
}

/// The id and change key of the `ItemId` (or `FolderId`) child of `el`.
fn id_of(el: &Element, which: &str) -> Option<(String, String)> {
    let id = el.child(which)?;
    Some((id.attr("Id")?.to_owned(), id.attr("ChangeKey").unwrap_or_default().to_owned()))
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// The integer extended properties of `el`, by property tag (decimal or `0x` hex in the response).
pub fn extended_int(el: &Element, tag: u32) -> Option<i64> {
    el.all("ExtendedProperty").find_map(|p| {
        let uri = p.child("ExtendedFieldURI")?;
        let t = uri.attr("PropertyTag")?;
        let n = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            Some(h) => u32::from_str_radix(h, 16).ok()?,
            None => t.parse().ok()?,
        };
        (n == tag).then(|| p.child_text("Value")?.parse().ok()).flatten()
    })
}

/// A named string extended property of `el`.
pub fn extended_named(el: &Element, name: &str) -> Option<String> {
    el.all("ExtendedProperty").find_map(|p| {
        let uri = p.child("ExtendedFieldURI")?;
        (uri.attr("PropertyName")? == name).then(|| p.child_text("Value").map(str::to_owned)).flatten()
    })
}

/// The strings of a `Categories` (or any `ArrayOfStringsType`) child.
pub fn strings(el: &Element, name: &str) -> Vec<String> {
    el.child(name)
        .map(|c| c.all("String").map(|s| s.text.trim().to_owned()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

/// Reads an item element (`Message`, `MeetingRequest`, `Item`, …).
pub fn parse_item(el: &Element) -> Option<EwsItem> {
    let (id, change_key) = id_of(el, "ItemId")?;
    let folder = el.child("ParentFolderId").and_then(|f| f.attr("Id")).map(str::to_owned);
    let mime = el.child("MimeContent").and_then(|m| {
        base64::engine::general_purpose::STANDARD.decode(m.text.trim().replace(['\r', '\n', ' '], "")).ok()
    });
    Some(EwsItem {
        id,
        change_key,
        folder,
        read: el.child_text("IsRead").and_then(parse_bool),
        flag_status: extended_int(el, 0x1090),
        last_verb: extended_int(el, 0x1081),
        categories: strings(el, "Categories"),
        size: el.child_text("Size").and_then(|s| s.parse().ok()),
        received: el.child_text("DateTimeReceived").map(str::to_owned),
        item_class: el.child_text("ItemClass").unwrap_or_default().to_owned(),
        message_id: el.child_text("InternetMessageId").map(str::to_owned),
        mime,
    })
}

fn parse_folder(el: &Element) -> Option<EwsFolder> {
    let (id, change_key) = id_of(el, "FolderId")?;
    Some(EwsFolder {
        id,
        change_key,
        parent: el.child("ParentFolderId").and_then(|p| p.attr("Id")).map(str::to_owned),
        name: el.child_text("DisplayName").unwrap_or_default().chars().take(255).collect(),
        class: el.child_text("FolderClass").unwrap_or_default().to_owned(),
        total: el.child_text("TotalCount").and_then(|s| s.parse().ok()),
        unread: el.child_text("UnreadCount").and_then(|s| s.parse().ok()),
        children: el.child_text("ChildFolderCount").and_then(|s| s.parse().ok()),
        kind: el.name.clone(),
    })
}

/// The shape of mail items in sync responses: ids, read state, flag, categories, size, date.
const MAIL_SHAPE: &str = concat!(
    "<t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>",
    r#"<t:FieldURI FieldURI="item:ItemClass"/><t:FieldURI FieldURI="item:DateTimeReceived"/>"#,
    r#"<t:FieldURI FieldURI="item:Size"/><t:FieldURI FieldURI="item:Categories"/>"#,
    r#"<t:FieldURI FieldURI="message:IsRead"/>"#,
    r#"<t:ExtendedFieldURI PropertyTag="0x1090" PropertyType="Integer"/>"#,
    r#"<t:ExtendedFieldURI PropertyTag="0x1081" PropertyType="Integer"/>"#,
    "</t:AdditionalProperties>"
);

const FOLDER_SHAPE: &str = concat!(
    "<m:FolderShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>",
    r#"<t:FieldURI FieldURI="folder:DisplayName"/><t:FieldURI FieldURI="folder:ParentFolderId"/>"#,
    r#"<t:FieldURI FieldURI="folder:FolderClass"/><t:FieldURI FieldURI="folder:TotalCount"/>"#,
    r#"<t:FieldURI FieldURI="folder:UnreadCount"/><t:FieldURI FieldURI="folder:ChildFolderCount"/>"#,
    "</t:AdditionalProperties></m:FolderShape>"
);

/// An EWS endpoint signed in.
#[derive(Debug)]
pub struct EwsClient {
    url: String,
    http: HttpClient,
    signer: Signer,
    /// `X-AnchorMailbox` (the account's address), which hosted servers route by.
    anchor: Option<String>,
}

impl EwsClient {
    /// A client for the EWS endpoint `url` (nothing is sent yet; see [`EwsClient::connect`]).
    pub fn new(url: &str, auth: EwsAuth, mut http: HttpClient, anchor: Option<&str>) -> Result<EwsClient> {
        let u = crate::http::Url::parse(url)?;
        if !u.https && !http.options.allow_plaintext_auth {
            return Err(NetError::Insecure(format!("the Exchange server {} over plain HTTP", u.origin())));
        }
        http.max_body = MAX_RESPONSE;
        Ok(EwsClient {
            url: url.trim().to_owned(),
            http,
            signer: Signer::new(auth),
            anchor: anchor.map(str::to_owned).filter(|a| !a.contains(['\r', '\n'])),
        })
    }

    /// A client that has signed in (one GetFolder of the mailbox root).
    pub fn connect(url: &str, auth: EwsAuth, http: HttpClient, anchor: Option<&str>) -> Result<EwsClient> {
        let c = EwsClient::new(url, auth, http, anchor)?;
        c.distinguished(&["msgfolderroot"])?;
        Ok(c)
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Lowers (or raises) the largest response read (default [`MAX_RESPONSE`]).
    pub fn set_max_response(&mut self, bytes: usize) {
        self.http.max_body = bytes;
    }

    /// Sends one SOAP request whose body is `op` and returns the element inside `soap:Body`.
    pub fn call(&self, op: &str) -> Result<Element> {
        let envelope = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><soap:Envelope xmlns:soap="{SOAP_NS}" xmlns:t="{TYPES_NS}" xmlns:m="{MESSAGES_NS}"><soap:Header><t:RequestServerVersion Version="{VERSION}"/></soap:Header><soap:Body>{op}</soap:Body></soap:Envelope>"#
        );
        let mut extra: Vec<(&str, &str)> = Vec::new();
        if let Some(a) = &self.anchor {
            extra.push(("X-AnchorMailbox", a.as_str()));
        }
        let resp =
            self.signer.post(&self.http, &self.url, "text/xml; charset=utf-8", envelope.as_bytes(), &extra);
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                self.signer.reset();
                return Err(e);
            }
        };
        // A SOAP fault comes with HTTP 500; anything else not 2xx is an HTTP error.
        let is_xml = resp.header("content-type").is_some_and(|t| t.to_ascii_lowercase().contains("xml"));
        if !resp.is_success() && !(resp.status == 500 && is_xml) {
            return Err(status_error(&resp));
        }
        let text = String::from_utf8_lossy(&resp.body);
        let mut root = parse_xml_with(&text, MAX_RESPONSE.saturating_add(1024 * 1024), MAX_ELEMENTS)?;
        drop(text);
        drop(resp);
        // Taken out of the tree, not copied: a large answer is held once.
        let at = root
            .children
            .iter()
            .position(|c| c.name == "Body")
            .ok_or_else(|| NetError::Protocol("the Exchange server's answer has no SOAP body".into()))?;
        let body = root.children.swap_remove(at);
        if let Some(f) = body.child("Fault") {
            let code = f
                .child("detail")
                .and_then(|d| d.child_text("ResponseCode"))
                .or_else(|| f.child_text("faultcode"))
                .unwrap_or("Fault")
                .to_owned();
            let message = f.child_text("faultstring").unwrap_or_default().chars().take(500).collect();
            return Err(NetError::Ews { code, message });
        }
        body.children
            .into_iter()
            .next()
            .ok_or_else(|| NetError::Protocol("the Exchange server's answer is empty".into()))
    }

    /// The response messages of an operation (`ResponseMessages` children), one per request item.
    pub fn messages(&self, op: &str) -> Result<Vec<Element>> {
        let mut r = self.call(op)?;
        Ok(match r.children.iter().position(|c| c.name == "ResponseMessages") {
            Some(i) => r.children.swap_remove(i).children,
            None => Vec::new(),
        })
    }

    /// The single response message of an operation, checked.
    fn one(&self, op: &str) -> Result<Element> {
        let msg = self
            .messages(op)?
            .into_iter()
            .next()
            .ok_or_else(|| NetError::Protocol("the Exchange server sent no response message".into()))?;
        check(&msg)?;
        Ok(msg)
    }

    // ---- folders ----

    /// The distinguished folders `names` (a folder the server lacks is `None`).
    pub fn distinguished(&self, names: &[&str]) -> Result<Vec<(String, Option<EwsFolder>)>> {
        let ids: String = names.iter().map(|n| folder_ref(n)).collect();
        let op = format!("<m:GetFolder>{FOLDER_SHAPE}<m:FolderIds>{ids}</m:FolderIds></m:GetFolder>");
        let msgs = self.messages(&op)?;
        let mut out = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let folder = msgs.get(i).and_then(|m| {
                check(m).ok()?;
                m.child("Folders")?.children.first().and_then(parse_folder)
            });
            out.push(((*name).to_owned(), folder));
        }
        if out.iter().all(|(_, f)| f.is_none())
            && let Some(m) = msgs.first()
        {
            // Not one folder: report why (the mailbox root missing means the sign-in reached no mailbox).
            check(m)?;
        }
        Ok(out)
    }

    /// Every folder below the mailbox root (FindFolder, deep traversal, paged).
    pub fn folders(&self) -> Result<Vec<EwsFolder>> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let op = format!(
                r#"<m:FindFolder Traversal="Deep">{FOLDER_SHAPE}<m:IndexedPageFolderView MaxEntriesReturned="{FOLDER_PAGE}" Offset="{offset}" BasePoint="Beginning"/><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindFolder>"#,
                folder_ref("msgfolderroot")
            );
            let msg = self.one(&op)?;
            let root = msg
                .child("RootFolder")
                .ok_or_else(|| NetError::Protocol("FindFolder without RootFolder".into()))?;
            let page: Vec<EwsFolder> = root
                .child("Folders")
                .map(|f| f.children.iter().filter_map(parse_folder).collect())
                .unwrap_or_default();
            let n = page.len();
            out.extend(page);
            if out.len() > MAX_FOLDERS {
                return Err(NetError::TooLarge(format!("more than {MAX_FOLDERS} folders")));
            }
            let last = root.attr("IncludesLastItemInRange").and_then(parse_bool).unwrap_or(true);
            if last || n == 0 {
                break;
            }
            offset = root
                .attr("IndexedPagingOffset")
                .and_then(|s| s.parse().ok())
                .filter(|o| *o > offset)
                .unwrap_or_else(|| offset.saturating_add(n));
        }
        Ok(out)
    }

    /// Whether the folder hierarchy changed since `state` (empty: unknown), and the state to keep.
    pub fn hierarchy_changed(&self, state: &str) -> Result<(bool, String)> {
        let st = if state.is_empty() {
            String::new()
        } else {
            format!("<m:SyncState>{}</m:SyncState>", esc(state))
        };
        let mut changed = state.is_empty();
        let mut current = st;
        for _ in 0..100 {
            let op = format!(
                "<m:SyncFolderHierarchy><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape></m:FolderShape>{current}</m:SyncFolderHierarchy>"
            );
            let msg = self.one(&op)?;
            let new_state = msg.child_text("SyncState").unwrap_or_default().to_owned();
            if msg.child("Changes").is_some_and(|c| !c.children.is_empty()) {
                changed = true;
            }
            let last = msg.child_text("IncludesLastFolderInRange").and_then(parse_bool).unwrap_or(true);
            current = format!("<m:SyncState>{}</m:SyncState>", esc(&new_state));
            if last {
                return Ok((changed, new_state));
            }
        }
        Ok((true, String::new()))
    }

    /// Makes a mail folder `name` below `parent`; its id.
    pub fn create_folder(&self, parent: &str, name: &str) -> Result<String> {
        let op = format!(
            "<m:CreateFolder><m:ParentFolderId>{}</m:ParentFolderId><m:Folders><t:Folder><t:FolderClass>IPF.Note</t:FolderClass><t:DisplayName>{}</t:DisplayName></t:Folder></m:Folders></m:CreateFolder>",
            folder_ref(parent),
            esc(name)
        );
        let msg = self.one(&op)?;
        msg.child("Folders")
            .and_then(|f| f.children.first())
            .and_then(|f| id_of(f, "FolderId"))
            .map(|(id, _)| id)
            .ok_or_else(|| NetError::Protocol("CreateFolder without a folder id".into()))
    }

    pub fn rename_folder(&self, id: &str, name: &str) -> Result<()> {
        let op = format!(
            r#"<m:UpdateFolder><m:FolderChanges><t:FolderChange>{}<t:Updates><t:SetFolderField><t:FieldURI FieldURI="folder:DisplayName"/><t:Folder><t:DisplayName>{}</t:DisplayName></t:Folder></t:SetFolderField></t:Updates></t:FolderChange></m:FolderChanges></m:UpdateFolder>"#,
            folder_ref(id),
            esc(name)
        );
        self.one(&op).map(|_| ())
    }

    pub fn move_folder(&self, id: &str, to: &str) -> Result<()> {
        let op = format!(
            "<m:MoveFolder><m:ToFolderId>{}</m:ToFolderId><m:FolderIds>{}</m:FolderIds></m:MoveFolder>",
            folder_ref(to),
            folder_ref(id)
        );
        self.one(&op).map(|_| ())
    }

    /// Removes a folder (with what is in it).
    pub fn delete_folder(&self, id: &str) -> Result<()> {
        let op = format!(
            r#"<m:DeleteFolder DeleteType="HardDelete"><m:FolderIds>{}</m:FolderIds></m:DeleteFolder>"#,
            folder_ref(id)
        );
        self.one(&op).map(|_| ())
    }

    /// One folder (its counts: items, subfolders), by id or distinguished name.
    pub fn folder(&self, id: &str) -> Result<Option<EwsFolder>> {
        Ok(self.distinguished(&[id])?.into_iter().next().and_then(|(_, f)| f))
    }

    /// The folder's item count.
    pub fn folder_total(&self, id: &str) -> Result<Option<u64>> {
        Ok(self.folder(id)?.and_then(|f| f.total))
    }

    // ---- mail ----

    /// One page of the changes in `folder` since `state` (empty: everything, as creations).
    pub fn sync_items(&self, folder: &str, state: &str, max: usize) -> Result<SyncItems> {
        let st = if state.is_empty() {
            String::new()
        } else {
            format!("<m:SyncState>{}</m:SyncState>", esc(state))
        };
        let op = format!(
            "<m:SyncFolderItems><m:ItemShape>{MAIL_SHAPE}</m:ItemShape><m:SyncFolderId>{}</m:SyncFolderId>{st}<m:MaxChangesReturned>{}</m:MaxChangesReturned></m:SyncFolderItems>",
            folder_ref(folder),
            max.clamp(1, 512)
        );
        let msg = self.one(&op)?;
        let mut changes = Vec::new();
        for c in msg.child("Changes").map(|c| c.children.as_slice()).unwrap_or_default() {
            let item = c.children.iter().find_map(parse_item);
            match c.name.as_str() {
                "Create" => changes.extend(item.map(ItemChange::Create)),
                "Update" => changes.extend(item.map(ItemChange::Update)),
                "Delete" => changes.extend(id_of(c, "ItemId").map(|(id, _)| ItemChange::Delete(id))),
                "ReadFlagChange" => {
                    if let (Some((id, _)), Some(read)) =
                        (id_of(c, "ItemId"), c.child_text("IsRead").and_then(parse_bool))
                    {
                        changes.push(ItemChange::ReadFlag { id, read });
                    }
                }
                _ => {}
            }
        }
        Ok(SyncItems {
            changes,
            state: msg.child_text("SyncState").unwrap_or_default().to_owned(),
            last: msg.child_text("IncludesLastItemInRange").and_then(parse_bool).unwrap_or(true),
        })
    }

    /// The items `ids` with their source (MimeContent) and state, in batches; an item gone meanwhile is left
    /// out.
    /// Downloads messages (id, `Size` when known) in batches of at most [`MIME_BATCH`] items and
    /// [`MIME_BATCH_BYTES`]. A batch whose answer is over the response limit is fetched again one item at a
    /// time; an item too large on its own is left out and returned in the second list (it stays on the
    /// server), so one huge message never stops the rest.
    pub fn get_mime_sized(&self, items: &[(String, Option<u64>)]) -> Result<(Vec<EwsItem>, Vec<String>)> {
        let mut out = Vec::new();
        let mut skipped = Vec::new();
        for batch in mime_batches(items) {
            match self.get_mime(&batch) {
                Ok(list) => out.extend(list),
                Err(NetError::TooLarge(_)) if batch.len() > 1 => {
                    for id in batch {
                        match self.get_mime(std::slice::from_ref(&id)) {
                            Ok(list) => out.extend(list),
                            Err(NetError::TooLarge(_)) => skipped.push(id),
                            Err(e) => return Err(e),
                        }
                    }
                }
                Err(NetError::TooLarge(_)) => skipped.extend(batch),
                Err(e) => return Err(e),
            }
        }
        Ok((out, skipped))
    }

    pub fn get_mime(&self, ids: &[String]) -> Result<Vec<EwsItem>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(MIME_BATCH) {
            let refs: String = chunk.iter().map(|i| item_ref(i)).collect();
            let op = format!(
                r#"<m:GetItem><m:ItemShape>{}</m:ItemShape><m:ItemIds>{refs}</m:ItemIds></m:GetItem>"#,
                MAIL_SHAPE.replace(
                    "<t:AdditionalProperties>",
                    r#"<t:AdditionalProperties><t:FieldURI FieldURI="item:MimeContent"/><t:FieldURI FieldURI="message:InternetMessageId"/><t:FieldURI FieldURI="item:ParentFolderId"/>"#
                )
            );
            for m in self.messages(&op)? {
                if let Err(e) = check(&m) {
                    if is_code(&e, "ErrorItemNotFound") {
                        continue;
                    }
                    return Err(e);
                }
                out.extend(m.child("Items").and_then(|i| i.children.first()).and_then(parse_item));
            }
        }
        Ok(out)
    }

    /// Changes read state, flag, categories and last verb of messages. One result per change: the item's
    /// new change key.
    pub fn set_flags(&self, changes: &[(String, FlagChange)]) -> Result<Vec<Result<String>>> {
        let mut out = Vec::new();
        for chunk in changes.chunks(ITEM_BATCH) {
            let mut body = String::new();
            for (id, c) in chunk {
                let mut u = String::new();
                if let Some(r) = c.read {
                    u.push_str(&format!(
                        r#"<t:SetItemField><t:FieldURI FieldURI="message:IsRead"/><t:Message><t:IsRead>{r}</t:IsRead></t:Message></t:SetItemField>"#
                    ));
                }
                for (uri, value) in [(FLAG_STATUS, c.flag_status), (LAST_VERB, c.last_verb)] {
                    match value {
                        Some(v) if v != 0 => u.push_str(&format!(
                            "<t:SetItemField>{uri}<t:Message><t:ExtendedProperty>{uri}<t:Value>{v}</t:Value></t:ExtendedProperty></t:Message></t:SetItemField>"
                        )),
                        Some(_) => u.push_str(&format!("<t:DeleteItemField>{uri}</t:DeleteItemField>")),
                        None => {}
                    }
                }
                match &c.categories {
                    Some(cats) if !cats.is_empty() => {
                        let list: String =
                            cats.iter().map(|c| format!("<t:String>{}</t:String>", esc(c))).collect();
                        u.push_str(&format!(
                            r#"<t:SetItemField><t:FieldURI FieldURI="item:Categories"/><t:Message><t:Categories>{list}</t:Categories></t:Message></t:SetItemField>"#
                        ));
                    }
                    Some(_) => u.push_str(
                        r#"<t:DeleteItemField><t:FieldURI FieldURI="item:Categories"/></t:DeleteItemField>"#,
                    ),
                    None => {}
                }
                body.push_str(&format!(
                    "<t:ItemChange>{}<t:Updates>{u}</t:Updates></t:ItemChange>",
                    item_ref(id)
                ));
            }
            let op = format!(
                r#"<m:UpdateItem MessageDisposition="SaveOnly" ConflictResolution="AlwaysOverwrite" SuppressReadReceipts="true"><m:ItemChanges>{body}</m:ItemChanges></m:UpdateItem>"#
            );
            let msgs = self.messages(&op)?;
            for i in 0..chunk.len() {
                out.push(match msgs.get(i) {
                    Some(m) => check(m).map(|_| {
                        m.child("Items")
                            .and_then(|it| it.children.first())
                            .and_then(|it| id_of(it, "ItemId"))
                            .map(|(_, ck)| ck)
                            .unwrap_or_default()
                    }),
                    None => Err(NetError::Protocol("UpdateItem answered fewer items than asked".into())),
                });
            }
        }
        Ok(out)
    }

    /// Moves items to folder `to`; one result per item: its new id (servers that keep ids may send none).
    pub fn move_items(&self, ids: &[String], to: &str) -> Result<Vec<Result<Option<(String, String)>>>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(ITEM_BATCH) {
            let refs: String = chunk.iter().map(|i| item_ref(i)).collect();
            let op = format!(
                "<m:MoveItem><m:ToFolderId>{}</m:ToFolderId><m:ItemIds>{refs}</m:ItemIds><m:ReturnNewItemIds>true</m:ReturnNewItemIds></m:MoveItem>",
                folder_ref(to)
            );
            let msgs = self.messages(&op)?;
            for i in 0..chunk.len() {
                out.push(match msgs.get(i) {
                    Some(m) => check(m).map(|_| {
                        m.child("Items").and_then(|it| it.children.first()).and_then(|it| id_of(it, "ItemId"))
                    }),
                    None => Err(NetError::Protocol("MoveItem answered fewer items than asked".into())),
                });
            }
        }
        Ok(out)
    }

    /// Deletes items for good (`hard`) or into Deleted Items; one result per item (already gone counts as
    /// done).
    pub fn delete_items(&self, ids: &[String], hard: bool) -> Result<Vec<Result<()>>> {
        let kind = if hard { "HardDelete" } else { "MoveToDeletedItems" };
        let mut out = Vec::new();
        for chunk in ids.chunks(ITEM_BATCH) {
            let refs: String = chunk.iter().map(|i| item_ref(i)).collect();
            let op = format!(
                r#"<m:DeleteItem DeleteType="{kind}" SendMeetingCancellations="SendToNone" AffectedTaskOccurrences="AllOccurrences"><m:ItemIds>{refs}</m:ItemIds></m:DeleteItem>"#
            );
            let msgs = self.messages(&op)?;
            for i in 0..chunk.len() {
                out.push(match msgs.get(i) {
                    Some(m) => match check(m) {
                        Err(e) if is_code(&e, "ErrorItemNotFound") => Ok(()),
                        r => r,
                    },
                    None => Err(NetError::Protocol("DeleteItem answered fewer items than asked".into())),
                });
            }
        }
        Ok(out)
    }

    /// Stores a message source in `folder` (read or not; never a draft unless `draft`); its id and change
    /// key.
    pub fn upload(&self, folder: &str, mime: &[u8], read: bool, draft: bool) -> Result<(String, String)> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(mime);
        let flags = i64::from(read) | if draft { 8 } else { 0 };
        let op = format!(
            r#"<m:CreateItem MessageDisposition="SaveOnly"><m:SavedItemFolderId>{}</m:SavedItemFolderId><m:Items><t:Message><t:MimeContent CharacterSet="UTF-8">{b64}</t:MimeContent><t:ExtendedProperty>{MESSAGE_FLAGS}<t:Value>{flags}</t:Value></t:ExtendedProperty><t:IsRead>{read}</t:IsRead></t:Message></m:Items></m:CreateItem>"#,
            folder_ref(folder)
        );
        let msg = self.one(&op)?;
        msg.child("Items")
            .and_then(|i| i.children.first())
            .and_then(|i| id_of(i, "ItemId"))
            .ok_or_else(|| NetError::Protocol("CreateItem without an item id".into()))
    }

    /// Sends a message source (a copy filed in Sent Items when `save_copy`); `bcc` are blind copies the
    /// source does not name.
    pub fn send(&self, mime: &[u8], bcc: &[String], save_copy: bool) -> Result<()> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(mime);
        let bcc_xml = if bcc.is_empty() {
            String::new()
        } else {
            let list: String = bcc
                .iter()
                .map(|a| format!("<t:Mailbox><t:EmailAddress>{}</t:EmailAddress></t:Mailbox>", esc(a)))
                .collect();
            format!("<t:BccRecipients>{list}</t:BccRecipients>")
        };
        let (disposition, saved) = if save_copy {
            (
                "SendAndSaveCopy",
                format!("<m:SavedItemFolderId>{}</m:SavedItemFolderId>", folder_ref("sentitems")),
            )
        } else {
            ("SendOnly", String::new())
        };
        let op = format!(
            r#"<m:CreateItem MessageDisposition="{disposition}">{saved}<m:Items><t:Message><t:MimeContent CharacterSet="UTF-8">{b64}</t:MimeContent>{bcc_xml}</t:Message></m:Items></m:CreateItem>"#
        );
        self.one(&op).map(|_| ())
    }

    // ---- items of any kind (contacts, events, tasks) ----

    /// The items in `folder` (FindItem, shallow, paged), read with `shape` (an `ItemShape`'s content).
    pub fn find_items(&self, folder: &str, shape: &str, max: usize) -> Result<Vec<Element>> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let op = format!(
                r#"<m:FindItem Traversal="Shallow"><m:ItemShape>{shape}</m:ItemShape><m:IndexedPageItemView MaxEntriesReturned="{FIND_PAGE}" Offset="{offset}" BasePoint="Beginning"/><m:ParentFolderIds>{}</m:ParentFolderIds></m:FindItem>"#,
                folder_ref(folder)
            );
            let msg = self.one(&op)?;
            let root = msg
                .child("RootFolder")
                .ok_or_else(|| NetError::Protocol("FindItem without RootFolder".into()))?;
            let page: Vec<Element> = root.child("Items").map(|i| i.children.clone()).unwrap_or_default();
            let n = page.len();
            out.extend(page);
            if out.len() > max {
                return Err(NetError::TooLarge(format!("more than {max} items in one folder")));
            }
            let last = root.attr("IncludesLastItemInRange").and_then(parse_bool).unwrap_or(true);
            if last || n == 0 {
                break;
            }
            offset = root
                .attr("IndexedPagingOffset")
                .and_then(|s| s.parse().ok())
                .filter(|o| *o > offset)
                .unwrap_or_else(|| offset.saturating_add(n));
        }
        Ok(out)
    }

    /// The items `ids` read with `shape`; `None` for one that is gone.
    pub fn get_items(&self, ids: &[String], shape: &str) -> Result<Vec<Option<Element>>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(ITEM_BATCH) {
            let refs: String = chunk.iter().map(|i| item_ref(i)).collect();
            let op = format!(
                "<m:GetItem><m:ItemShape>{shape}</m:ItemShape><m:ItemIds>{refs}</m:ItemIds></m:GetItem>"
            );
            let msgs = self.messages(&op)?;
            for i in 0..chunk.len() {
                out.push(match msgs.get(i) {
                    Some(m) => match check(m) {
                        Ok(()) => m.child("Items").and_then(|it| it.children.first()).cloned(),
                        Err(e) if is_code(&e, "ErrorItemNotFound") => None,
                        Err(e) => return Err(e),
                    },
                    None => None,
                });
            }
        }
        Ok(out)
    }

    /// Creates `item` (a whole `<t:Contact>…</t:Contact>` or similar) in `folder`; `attrs` go on CreateItem
    /// (`SendMeetingInvitations="SendToNone"`). Its id and change key.
    pub fn create_item(&self, folder: &str, item: &str, attrs: &str) -> Result<(String, String)> {
        let op = format!(
            "<m:CreateItem {attrs}><m:SavedItemFolderId>{}</m:SavedItemFolderId><m:Items>{item}</m:Items></m:CreateItem>",
            folder_ref(folder)
        );
        let msg = self.one(&op)?;
        msg.child("Items")
            .and_then(|i| i.children.first())
            .and_then(|i| id_of(i, "ItemId"))
            .ok_or_else(|| NetError::Protocol("CreateItem without an item id".into()))
    }

    /// Applies `updates` (SetItemField / DeleteItemField elements) to item `id`; with a change key the
    /// server refuses when the item changed meanwhile (`ErrorIrresolvableConflict`). Its new id and change
    /// key.
    pub fn update_item(
        &self,
        id: &str,
        change_key: Option<&str>,
        updates: &str,
        attrs: &str,
    ) -> Result<(String, String)> {
        let item = match change_key.filter(|c| !c.is_empty()) {
            Some(ck) => format!(r#"<t:ItemId Id="{}" ChangeKey="{}"/>"#, esc(id), esc(ck)),
            None => item_ref(id),
        };
        let conflict = if change_key.is_some() { "NeverOverwrite" } else { "AlwaysOverwrite" };
        let op = format!(
            r#"<m:UpdateItem ConflictResolution="{conflict}" {attrs}><m:ItemChanges><t:ItemChange>{item}<t:Updates>{updates}</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#
        );
        let msg = self.one(&op)?;
        Ok(msg
            .child("Items")
            .and_then(|i| i.children.first())
            .and_then(|i| id_of(i, "ItemId"))
            .unwrap_or_else(|| (id.to_owned(), String::new())))
    }

    /// Deletes one item for good (meeting cancellations are not sent; MailCraft mails them itself).
    pub fn delete_item(&self, id: &str) -> Result<()> {
        self.delete_items(std::slice::from_ref(&id.to_owned()), true)?.into_iter().next().unwrap_or(Ok(()))
    }

    /// Answers a meeting request (or a calendar item of one) on the server, which tells the organizer when
    /// `send` (else the answer is only recorded).
    pub fn reply_to_meeting(
        &self,
        reply: MeetingReply,
        id: &str,
        change_key: &str,
        send: bool,
        text: &str,
    ) -> Result<()> {
        let el = reply.element();
        let body = if text.trim().is_empty() {
            String::new()
        } else {
            format!(r#"<t:Body BodyType="Text">{}</t:Body>"#, esc(text))
        };
        let reference = if change_key.is_empty() {
            format!(r#"<t:ReferenceItemId Id="{}"/>"#, esc(id))
        } else {
            format!(r#"<t:ReferenceItemId Id="{}" ChangeKey="{}"/>"#, esc(id), esc(change_key))
        };
        let disposition = if send { "SendAndSaveCopy" } else { "SaveOnly" };
        let op = format!(
            r#"<m:CreateItem MessageDisposition="{disposition}"><m:Items><t:{el}>{body}{reference}</t:{el}></m:Items></m:CreateItem>"#
        );
        let msg = self.one(&op)?;
        if !send {
            // SaveOnly records the answer and keeps the response message as a draft (in Drafts): it is never
            // to be sent, so it goes (a stray draft reply would otherwise show up among the user's drafts).
            let drafts: Vec<String> = msg
                .child("Items")
                .map(|i| i.children.iter().filter_map(|c| id_of(c, "ItemId")).map(|(id, _)| id).collect())
                .unwrap_or_default();
            if !drafts.is_empty() {
                match self.delete_items(&drafts, true) {
                    Ok(r) => {
                        for e in r.into_iter().filter_map(std::result::Result::err) {
                            log::warn!("ews: the unsent meeting answer was kept as a draft: {e}");
                        }
                    }
                    Err(e) => log::warn!("ews: the unsent meeting answer was kept as a draft: {e}"),
                }
            }
        }
        Ok(())
    }

    // ---- automatic replies, availability ----

    /// The mailbox's automatic-reply settings.
    pub fn oof(&self, address: &str) -> Result<OofSettings> {
        let op = format!(
            "<m:GetUserOofSettingsRequest><t:Mailbox><t:Address>{}</t:Address></t:Mailbox></m:GetUserOofSettingsRequest>",
            esc(address)
        );
        let r = self.call(&op)?;
        if let Some(m) = r.child("ResponseMessage") {
            check(m)?;
        }
        let s = r
            .child("OofSettings")
            .ok_or_else(|| NetError::Protocol("no automatic-reply settings in the answer".into()))?;
        let reply =
            |name: &str| s.child(name).and_then(|r| r.child_text("Message")).unwrap_or_default().to_owned();
        let duration = s.child("Duration");
        Ok(OofSettings {
            state: s.child_text("OofState").unwrap_or("Disabled").to_owned(),
            external_audience: s.child_text("ExternalAudience").unwrap_or("None").to_owned(),
            start: duration.and_then(|d| d.child_text("StartTime")).map(str::to_owned),
            end: duration.and_then(|d| d.child_text("EndTime")).map(str::to_owned),
            internal_reply: reply("InternalReply"),
            external_reply: reply("ExternalReply"),
        })
    }

    /// Sets the mailbox's automatic-reply settings.
    pub fn set_oof(&self, address: &str, o: &OofSettings) -> Result<()> {
        let pick = |v: &str, allowed: &[&str], default: &str| -> String {
            allowed.iter().find(|a| a.eq_ignore_ascii_case(v)).map_or(default, |a| a).to_owned()
        };
        let state = pick(&o.state, &["Disabled", "Enabled", "Scheduled"], "Disabled");
        let audience = pick(&o.external_audience, &["None", "Known", "All"], "None");
        let duration = match (&o.start, &o.end) {
            (Some(s), Some(e)) => format!(
                "<t:Duration><t:StartTime>{}</t:StartTime><t:EndTime>{}</t:EndTime></t:Duration>",
                esc(s),
                esc(e)
            ),
            _ => String::new(),
        };
        let op = format!(
            "<m:SetUserOofSettingsRequest><t:Mailbox><t:Address>{}</t:Address></t:Mailbox><t:UserOofSettings><t:OofState>{state}</t:OofState><t:ExternalAudience>{audience}</t:ExternalAudience>{duration}<t:InternalReply><t:Message>{}</t:Message></t:InternalReply><t:ExternalReply><t:Message>{}</t:Message></t:ExternalReply></t:UserOofSettings></m:SetUserOofSettingsRequest>",
            esc(address),
            esc(&o.internal_reply),
            esc(&o.external_reply)
        );
        let r = self.call(&op)?;
        match r.child("ResponseMessage") {
            Some(m) => check(m),
            None => Ok(()),
        }
    }

    /// The busy times of `addresses` between `start` and `end` (UTC, `YYYY-MM-DDThh:mm:ss`); one result per
    /// address.
    pub fn availability(
        &self,
        addresses: &[String],
        start: &str,
        end: &str,
    ) -> Result<Vec<Result<Vec<BusyBlock>>>> {
        let utc = "<t:Bias>0</t:Bias><t:Time>00:00:00</t:Time><t:DayOrder>0</t:DayOrder><t:Month>0</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek>";
        let boxes: String = addresses
            .iter()
            .map(|a| {
                format!(
                    "<t:MailboxData><t:Email><t:Address>{}</t:Address></t:Email><t:AttendeeType>Required</t:AttendeeType><t:ExcludeConflicts>false</t:ExcludeConflicts></t:MailboxData>",
                    esc(a)
                )
            })
            .collect();
        let op = format!(
            "<m:GetUserAvailabilityRequest><t:TimeZone><t:Bias>0</t:Bias><t:StandardTime>{utc}</t:StandardTime><t:DaylightTime>{utc}</t:DaylightTime></t:TimeZone><m:MailboxDataArray>{boxes}</m:MailboxDataArray><t:FreeBusyViewOptions><t:TimeWindow><t:StartTime>{}</t:StartTime><t:EndTime>{}</t:EndTime></t:TimeWindow><t:MergedFreeBusyIntervalInMinutes>30</t:MergedFreeBusyIntervalInMinutes><t:RequestedView>FreeBusy</t:RequestedView></t:FreeBusyViewOptions></m:GetUserAvailabilityRequest>",
            esc(start),
            esc(end)
        );
        let r = self.call(&op)?;
        let answers: Vec<&Element> =
            r.child("FreeBusyResponseArray").map(|a| a.all("FreeBusyResponse").collect()).unwrap_or_default();
        let mut out = Vec::new();
        for i in 0..addresses.len() {
            let Some(a) = answers.get(i) else {
                out.push(Err(NetError::Protocol("no availability for this address".into())));
                continue;
            };
            if let Some(m) = a.child("ResponseMessage")
                && let Err(e) = check(m)
            {
                out.push(Err(e));
                continue;
            }
            let events = a
                .child("FreeBusyView")
                .and_then(|v| v.child("CalendarEventArray"))
                .map(|arr| {
                    arr.all("CalendarEvent")
                        .take(5_000)
                        .map(|e| BusyBlock {
                            start: e.child_text("StartTime").unwrap_or_default().to_owned(),
                            end: e.child_text("EndTime").unwrap_or_default().to_owned(),
                            busy_type: e.child_text("BusyType").unwrap_or("Busy").to_owned(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(Ok(events));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_and_references() {
        assert_eq!(esc("a<b>&\"c'\u{1}"), "a&lt;b&gt;&amp;&quot;c&apos;");
        assert_eq!(folder_ref("inbox"), r#"<t:DistinguishedFolderId Id="inbox"/>"#);
        assert_eq!(folder_ref("AAMk\"x"), r#"<t:FolderId Id="AAMk&quot;x"/>"#);
        assert!(matches!(EwsAuth::from_mechanism("ntlm", "u", "p"), EwsAuth::Ntlm { .. }));
        assert!(matches!(EwsAuth::from_mechanism("", "u", "p"), EwsAuth::Auto { .. }));
        assert!(matches!(EwsAuth::from_mechanism("XOAUTH2", "u", "t"), EwsAuth::Bearer(_)));
        assert!(
            !format!("{:?}", EwsAuth::Basic { user: "u".into(), password: "secret".into() })
                .contains("secret")
        );
    }

    #[test]
    fn items_and_folders_parse_and_junk_is_skipped() {
        let xml = r#"<t:Message xmlns:t="x"><t:ItemId Id="I1" ChangeKey="C1"/><t:ParentFolderId Id="F1"/>
            <t:MimeContent CharacterSet="UTF-8">U3ViamVjdDogaGkNCg0KYm9keQ==</t:MimeContent>
            <t:Size>42</t:Size><t:Categories><t:String>Red</t:String><t:String> </t:String></t:Categories>
            <t:IsRead>true</t:IsRead><t:ExtendedProperty><t:ExtendedFieldURI PropertyTag="4240" PropertyType="Integer"/><t:Value>2</t:Value></t:ExtendedProperty>
            <t:ExtendedProperty><t:ExtendedFieldURI PropertyTag="0x1081" PropertyType="Integer"/><t:Value>junk</t:Value></t:ExtendedProperty></t:Message>"#;
        let el = parse_xml_with(xml, 1 << 20, 1000).unwrap();
        let it = parse_item(&el).unwrap();
        assert_eq!((it.id.as_str(), it.change_key.as_str(), it.folder.as_deref()), ("I1", "C1", Some("F1")));
        assert_eq!(it.mime.as_deref(), Some(&b"Subject: hi\r\n\r\nbody"[..]));
        assert_eq!((it.read, it.flag_status, it.last_verb, it.size), (Some(true), Some(2), None, Some(42)));
        assert_eq!(it.categories, vec!["Red".to_owned()]);
        assert!(
            parse_item(&parse_xml_with("<Message><Subject>x</Subject></Message>", 1000, 100).unwrap())
                .is_none()
        );
        let f = parse_xml_with(r#"<Folder><FolderId Id="F2"/><ParentFolderId Id="R"/><DisplayName>Work</DisplayName><FolderClass>IPF.Note</FolderClass><TotalCount>x</TotalCount></Folder>"#, 1000, 100).unwrap();
        let f = parse_folder(&f).unwrap();
        assert!(f.is_mail() && f.total.is_none() && f.parent.as_deref() == Some("R"));
        let cal = EwsFolder {
            kind: "CalendarFolder".into(),
            class: "IPF.Appointment".into(),
            ..Default::default()
        };
        assert!(!cal.is_mail());
        let err = parse_xml_with(r#"<M ResponseClass="Error"><MessageText>gone</MessageText><ResponseCode>ErrorItemNotFound</ResponseCode></M>"#, 1000, 100).unwrap();
        let e = check(&err).unwrap_err();
        assert!(is_code(&e, "ErrorItemNotFound"));
        let ok = parse_xml_with(r#"<M ResponseClass="Warning"/>"#, 1000, 100).unwrap();
        assert!(check(&ok).is_ok());
    }
}
