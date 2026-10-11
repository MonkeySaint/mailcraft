//! A local OAuth 2.0 authorization server for tests: the device authorization grant (RFC 8628), the
//! authorization-code grant with PKCE (RFC 6749 section 4.1, RFC 7636; the authorization endpoint approves at
//! once for the user named in `login_hint`), and refresh tokens (RFC 6749 section 6, rotated on use).
//!
//! Access tokens it issues sign in to the IMAP and SMTP servers with XOAUTH2 or OAUTHBEARER (RFC 7628) and to
//! JMAP as Bearer tokens. Clients are public (no secret); only the configured client ids are accepted. A test
//! approves a device code with [`crate::TestServers::oauth_approve`] (standing in for the user at the
//! verification page) and can expire every access token at once to exercise refreshing.

use std::collections::BTreeMap;

use base64::Engine;
use serde_json::{Value, json};

use crate::date::DateTime;

/// A pending device authorization.
#[derive(Clone, Debug)]
struct Device {
    user_code: String,
    client_id: String,
    expires: i64,
    approved: Option<String>,
    denied: bool,
}

/// An issued authorization code.
#[derive(Clone, Debug)]
struct Code {
    user: String,
    client_id: String,
    redirect_uri: String,
    challenge: String,
    expires: i64,
}

/// Every grant and token of the server.
#[derive(Debug, Default)]
pub struct OAuthState {
    counter: u64,
    /// Accepted client ids.
    pub clients: Vec<String>,
    /// Access token lifetime in seconds.
    pub lifetime_secs: i64,
    access: BTreeMap<String, (String, i64)>,
    refresh: BTreeMap<String, (String, String)>,
    devices: BTreeMap<String, Device>,
    codes: BTreeMap<String, Code>,
    /// Grant types redeemed at the token endpoint, in order (for tests).
    pub grants: Vec<String>,
}

fn fnv(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A token-endpoint answer: HTTP status and JSON body.
pub type Answer = (u16, Value);

fn err(status: u16, error: &str, desc: &str) -> Answer {
    (status, json!({"error": error, "error_description": desc}))
}

/// RFC 7636 S256: BASE64URL(SHA-256(verifier)). A small SHA-256 (FIPS 180-4) so the test server needs no
/// crypto dependency.
pub fn s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()))
}

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (slot, word) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *slot = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let v = {
                let g = |j: usize| w.get(j).copied().unwrap_or(0);
                let (w15, w2, w16, w7) = (g(i - 15), g(i - 2), g(i - 16), g(i - 7));
                let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
                let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
                w16.wrapping_add(s0).wrapping_add(w7).wrapping_add(s1)
            };
            if let Some(slot) = w.get_mut(i) {
                *slot = v;
            }
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (k, wi) in K.iter().zip(w.iter()) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(*k).wrapping_add(*wi);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (chunk, v) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *chunk = v.to_be_bytes();
    }
    out
}

impl OAuthState {
    pub fn new(clients: Vec<String>, lifetime_secs: i64) -> OAuthState {
        OAuthState { clients, lifetime_secs: lifetime_secs.max(1), ..Default::default() }
    }

    fn token(&mut self, prefix: &str) -> String {
        self.counter = self.counter.wrapping_add(1);
        let seed = format!("{prefix}{}{}", self.counter, DateTime::now().unix);
        format!("{prefix}{:016x}{:08x}", fnv(seed.as_bytes()), self.counter)
    }

    fn known_client(&self, id: &str) -> bool {
        self.clients.iter().any(|c| c == id)
    }

    fn issue(&mut self, user: &str, client_id: &str) -> Value {
        let now = DateTime::now().unix;
        let access = self.token("at-");
        let refresh = self.token("rt-");
        self.access.insert(access.clone(), (user.to_owned(), now.saturating_add(self.lifetime_secs)));
        self.refresh.insert(refresh.clone(), (user.to_owned(), client_id.to_owned()));
        json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": self.lifetime_secs,
            "refresh_token": refresh,
            "scope": "mail",
        })
    }

    /// The user an access token belongs to (unexpired tokens only).
    pub fn user_of(&self, token: &str) -> Option<String> {
        let (user, expires) = self.access.get(token)?;
        (*expires > DateTime::now().unix).then(|| user.clone())
    }

    /// Ends the life of every access token (refresh tokens stay valid).
    pub fn expire_access_tokens(&mut self) {
        self.access.clear();
    }

    /// RFC 8628 section 3.1/3.2.
    pub fn device_authorization(&mut self, form: &BTreeMap<String, String>, base: &str) -> Answer {
        let client = form.get("client_id").cloned().unwrap_or_default();
        if !self.known_client(&client) {
            return err(401, "invalid_client", "unknown client");
        }
        let device_code = self.token("dc-");
        let n = fnv(device_code.as_bytes());
        let user_code = format!("{:04X}-{:04X}", n & 0xFFFF, (n >> 16) & 0xFFFF);
        let expires = DateTime::now().unix.saturating_add(600);
        self.devices.insert(
            device_code.clone(),
            Device {
                user_code: user_code.clone(),
                client_id: client,
                expires,
                approved: None,
                denied: false,
            },
        );
        (
            200,
            json!({
                "device_code": device_code,
                "user_code": user_code,
                "verification_uri": format!("{base}/oauth/verify"),
                "verification_uri_complete": format!("{base}/oauth/verify?user_code={user_code}"),
                "expires_in": 600,
                "interval": 1,
            }),
        )
    }

    /// The user at the verification page approves (`Some(user)`) or denies (`None`) a code. False when no
    /// pending authorization has that code.
    pub fn decide(&mut self, user_code: &str, user: Option<&str>) -> bool {
        let Some(d) = self.devices.values_mut().find(|d| d.user_code.eq_ignore_ascii_case(user_code.trim()))
        else {
            return false;
        };
        match user {
            Some(u) => d.approved = Some(u.to_owned()),
            None => d.denied = true,
        }
        true
    }

    /// User codes waiting for a decision.
    pub fn pending_user_codes(&self) -> Vec<String> {
        self.devices
            .values()
            .filter(|d| d.approved.is_none() && !d.denied)
            .map(|d| d.user_code.clone())
            .collect()
    }

    /// The authorization endpoint: approves at once for `login_hint` (a known user) and answers with the
    /// redirect location, or an error text.
    pub fn authorize(
        &mut self,
        q: &BTreeMap<String, String>,
        user_exists: impl Fn(&str) -> bool,
    ) -> Result<String, String> {
        let client = q.get("client_id").cloned().unwrap_or_default();
        if !self.known_client(&client) {
            return Err("unknown client".into());
        }
        let redirect = q.get("redirect_uri").cloned().unwrap_or_default();
        if !redirect.starts_with("http://127.0.0.1:") && !redirect.starts_with("http://localhost:") {
            return Err("redirect_uri must be a loopback address".into());
        }
        if q.get("response_type").map(String::as_str) != Some("code") {
            return Err("response_type must be code".into());
        }
        if q.get("code_challenge_method").map(String::as_str) != Some("S256") {
            return Err("PKCE with S256 is required".into());
        }
        let challenge = q.get("code_challenge").cloned().unwrap_or_default();
        let user = q.get("login_hint").cloned().unwrap_or_default();
        if !user_exists(&user) {
            return Err("unknown user in login_hint".into());
        }
        let code = self.token("ac-");
        let expires = DateTime::now().unix.saturating_add(300);
        self.codes.insert(
            code.clone(),
            Code { user, client_id: client, redirect_uri: redirect.clone(), challenge, expires },
        );
        let state = q.get("state").cloned().unwrap_or_default();
        let sep = if redirect.contains('?') { '&' } else { '?' };
        Ok(format!("{redirect}{sep}code={}&state={}", pct(&code), pct(&state)))
    }

    /// The token endpoint (RFC 6749 section 3.2): device_code, authorization_code and refresh_token grants.
    pub fn token_endpoint(&mut self, form: &BTreeMap<String, String>) -> Answer {
        let get = |k: &str| form.get(k).cloned().unwrap_or_default();
        let client = get("client_id");
        if !self.known_client(&client) {
            return err(401, "invalid_client", "unknown client");
        }
        let grant = get("grant_type");
        let now = DateTime::now().unix;
        let answer = match grant.as_str() {
            "urn:ietf:params:oauth:grant-type:device_code" => {
                let code = get("device_code");
                let Some(d) = self.devices.get(&code).cloned() else {
                    return err(400, "invalid_grant", "unknown device code");
                };
                if d.client_id != client {
                    return err(400, "invalid_grant", "device code of another client");
                }
                if d.expires <= now {
                    self.devices.remove(&code);
                    return err(400, "expired_token", "the device code expired");
                }
                if d.denied {
                    self.devices.remove(&code);
                    return err(400, "access_denied", "the user denied access");
                }
                let Some(user) = d.approved else {
                    return err(400, "authorization_pending", "waiting for the user");
                };
                self.devices.remove(&code);
                (200, self.issue(&user, &client))
            }
            "authorization_code" => {
                let Some(c) = self.codes.remove(&get("code")) else {
                    return err(400, "invalid_grant", "unknown or used code");
                };
                if c.client_id != client || c.redirect_uri != get("redirect_uri") || c.expires <= now {
                    return err(400, "invalid_grant", "code does not match");
                }
                if s256(&get("code_verifier")) != c.challenge {
                    return err(400, "invalid_grant", "PKCE verification failed");
                }
                (200, self.issue(&c.user, &client))
            }
            "refresh_token" => {
                let Some((user, owner)) = self.refresh.remove(&get("refresh_token")) else {
                    return err(400, "invalid_grant", "unknown refresh token");
                };
                if owner != client {
                    return err(400, "invalid_grant", "refresh token of another client");
                }
                (200, self.issue(&user, &client))
            }
            _ => return err(400, "unsupported_grant_type", &grant),
        };
        self.grants.push(grant);
        answer
    }
}

fn pct(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes `application/x-www-form-urlencoded` (also a query string).
pub fn parse_form(s: &str) -> BTreeMap<String, String> {
    let dec = |v: &str| -> String {
        let b = v.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while let Some(&c) = b.get(i) {
            match c {
                b'+' => out.push(b' '),
                b'%' => {
                    let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
                    if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    out.push(c);
                }
                _ => out.push(c),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    s.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (dec(k), dec(v)),
            None => (dec(p), String::new()),
        })
        .collect()
}

/// The (user, token) of a SASL XOAUTH2 or OAUTHBEARER (RFC 7628) client response.
pub fn sasl_bearer(mech: &str, decoded: &[u8]) -> Option<(String, String)> {
    let text = String::from_utf8_lossy(decoded);
    let mut user = None;
    let mut token = None;
    let body = if mech.eq_ignore_ascii_case("OAUTHBEARER") {
        // gs2-header "n,a=<authzid>," then key=value pairs separated by ^A.
        let (gs2, rest) = text.split_once('\x01')?;
        let a = gs2.split(',').find_map(|p| p.strip_prefix("a="));
        user = a.map(|u| u.replace("=2C", ",").replace("=3D", "="));
        rest.to_owned()
    } else {
        text.into_owned()
    };
    for kv in body.split('\x01') {
        if let Some(u) = kv.strip_prefix("user=") {
            user = Some(u.to_owned());
        } else if let Some(a) = kv.strip_prefix("auth=") {
            let t = a.strip_prefix("Bearer ").or_else(|| a.strip_prefix("bearer "))?;
            token = Some(t.trim().to_owned());
        }
    }
    Some((user?, token?))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn sha256_and_pkce_vectors() {
        let hex = |b: [u8; 32]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(hex(sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hex(sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        // RFC 7636 appendix B.
        assert_eq!(
            s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn device_grant_refresh_and_expiry() {
        let mut o = OAuthState::new(vec!["c1".into()], 3600);
        let form = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
        };
        assert_eq!(o.device_authorization(&form(&[("client_id", "x")]), "https://h").0, 401);
        let (st, v) = o.device_authorization(&form(&[("client_id", "c1")]), "https://h");
        assert_eq!(st, 200);
        let dc = v["device_code"].as_str().unwrap().to_owned();
        let uc = v["user_code"].as_str().unwrap().to_owned();
        let poll = form(&[
            ("client_id", "c1"),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", &dc),
        ]);
        assert_eq!(o.token_endpoint(&poll).1["error"], "authorization_pending");
        assert_eq!(o.pending_user_codes(), std::slice::from_ref(&uc));
        assert!(o.decide(&uc, Some("kim@example.com")));
        let (st, t) = o.token_endpoint(&poll);
        assert_eq!(st, 200);
        let at = t["access_token"].as_str().unwrap().to_owned();
        let rt = t["refresh_token"].as_str().unwrap().to_owned();
        assert_eq!(o.user_of(&at).as_deref(), Some("kim@example.com"));
        assert_eq!(o.token_endpoint(&poll).1["error"], "invalid_grant", "a device code is redeemed once");
        o.expire_access_tokens();
        assert!(o.user_of(&at).is_none());
        let r = form(&[("client_id", "c1"), ("grant_type", "refresh_token"), ("refresh_token", &rt)]);
        let (st, t2) = o.token_endpoint(&r);
        assert_eq!(st, 200);
        assert!(o.user_of(t2["access_token"].as_str().unwrap()).is_some());
        assert_eq!(o.token_endpoint(&r).1["error"], "invalid_grant", "refresh tokens rotate");
        assert_eq!(o.grants.len(), 2);
    }

    #[test]
    fn sasl_messages_and_forms() {
        let x = b"user=kim@example.com\x01auth=Bearer at-1\x01\x01";
        assert_eq!(sasl_bearer("XOAUTH2", x), Some(("kim@example.com".into(), "at-1".into())));
        let ob = b"n,a=kim@example.com,\x01host=h\x01port=1\x01auth=Bearer at-2\x01\x01";
        assert_eq!(sasl_bearer("OAUTHBEARER", ob), Some(("kim@example.com".into(), "at-2".into())));
        assert_eq!(sasl_bearer("XOAUTH2", b"garbage"), None);
        let f = parse_form("a=1&b=x%20y+z&c&=&d=%zz");
        assert_eq!(f["b"], "x y z");
        assert_eq!(f["c"], "");
        assert_eq!(f["d"], "%zz");
    }
}
