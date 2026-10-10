//! Credentials and the SASL mechanisms shared by IMAP and SMTP: PLAIN (RFC 4616), LOGIN, XOAUTH2 and
//! OAUTHBEARER (RFC 7628).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// How to log in.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    Password {
        user: String,
        password: String,
    },
    /// OAuth 2.0 bearer token (XOAUTH2 or OAUTHBEARER).
    OAuth2 {
        user: String,
        access_token: String,
    },
}

impl Credentials {
    pub fn password(user: impl Into<String>, password: impl Into<String>) -> Self {
        Credentials::Password { user: user.into(), password: password.into() }
    }
    pub fn oauth2(user: impl Into<String>, access_token: impl Into<String>) -> Self {
        Credentials::OAuth2 { user: user.into(), access_token: access_token.into() }
    }
    pub fn user(&self) -> &str {
        match self {
            Credentials::Password { user, .. } | Credentials::OAuth2 { user, .. } => user,
        }
    }
}

impl std::fmt::Debug for Credentials {
    // Never print secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credentials::Password { user, .. } => write!(f, "Password {{ user: {user:?}, .. }}"),
            Credentials::OAuth2 { user, .. } => write!(f, "OAuth2 {{ user: {user:?}, .. }}"),
        }
    }
}

/// A SASL mechanism this crate implements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mechanism {
    Plain,
    Login,
    XOAuth2,
    OAuthBearer,
}

impl Mechanism {
    pub fn name(self) -> &'static str {
        match self {
            Mechanism::Plain => "PLAIN",
            Mechanism::Login => "LOGIN",
            Mechanism::XOAuth2 => "XOAUTH2",
            Mechanism::OAuthBearer => "OAUTHBEARER",
        }
    }
    pub fn from_name(s: &str) -> Option<Mechanism> {
        match s.trim().to_ascii_uppercase().as_str() {
            "PLAIN" => Some(Mechanism::Plain),
            "LOGIN" => Some(Mechanism::Login),
            "XOAUTH2" => Some(Mechanism::XOAuth2),
            "OAUTHBEARER" => Some(Mechanism::OAuthBearer),
            _ => None,
        }
    }
}

/// Picks a mechanism for `creds` from the server's advertised list (`offered`, any case), honouring an
/// explicit `preferred` name (empty = automatic). `None` when nothing fits.
pub fn choose(creds: &Credentials, offered: &[String], preferred: &str) -> Option<Mechanism> {
    let has = |m: Mechanism| offered.iter().any(|o| o.eq_ignore_ascii_case(m.name()));
    if let Some(m) = Mechanism::from_name(preferred) {
        let fits = matches!(
            (creds, m),
            (Credentials::Password { .. }, Mechanism::Plain | Mechanism::Login)
                | (Credentials::OAuth2 { .. }, Mechanism::XOAuth2 | Mechanism::OAuthBearer)
        );
        return fits.then_some(m);
    }
    let order: &[Mechanism] = match creds {
        Credentials::Password { .. } => &[Mechanism::Plain, Mechanism::Login],
        Credentials::OAuth2 { .. } => &[Mechanism::XOAuth2, Mechanism::OAuthBearer],
    };
    order.iter().copied().find(|&m| has(m))
}

/// RFC 4616 PLAIN message: `authzid NUL authcid NUL passwd` (authzid empty).
pub fn plain(user: &str, password: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(user.len() + password.len() + 2);
    v.push(0);
    v.extend_from_slice(user.as_bytes());
    v.push(0);
    v.extend_from_slice(password.as_bytes());
    v
}

/// XOAUTH2 initial response: `user=… ^A auth=Bearer … ^A ^A`.
pub fn xoauth2(user: &str, token: &str) -> Vec<u8> {
    format!("user={user}\x01auth=Bearer {token}\x01\x01").into_bytes()
}

/// RFC 7628 OAUTHBEARER initial response (GS2 header with the authzid, then key/value pairs).
pub fn oauthbearer(user: &str, host: &str, port: u16, token: &str) -> Vec<u8> {
    let user = user.replace('=', "=3D").replace(',', "=2C");
    format!("n,a={user},\x01host={host}\x01port={port}\x01auth=Bearer {token}\x01\x01").into_bytes()
}

/// The initial client response for `mech`, or `None` for LOGIN (which is challenge driven).
pub fn initial_response(mech: Mechanism, creds: &Credentials, host: &str, port: u16) -> Option<Vec<u8>> {
    match (mech, creds) {
        (Mechanism::Plain, Credentials::Password { user, password }) => Some(plain(user, password)),
        (Mechanism::XOAuth2, Credentials::OAuth2 { user, access_token }) => Some(xoauth2(user, access_token)),
        (Mechanism::OAuthBearer, Credentials::OAuth2 { user, access_token }) => {
            Some(oauthbearer(user, host, port, access_token))
        }
        _ => None,
    }
}

/// The client's answer to a challenge that arrives after the initial response, which servers use to report
/// a failure: XOAUTH2 expects an empty response, OAUTHBEARER a single %x01 (RFC 7628 section 3.2.3), and
/// any other mechanism cancels the exchange with `*` (RFC 4954 section 4, RFC 9051 section 6.2.2).
pub fn error_challenge_reply(mech: Mechanism) -> &'static str {
    match mech {
        Mechanism::XOAuth2 => "",
        Mechanism::OAuthBearer => "AQ==",
        Mechanism::Plain | Mechanism::Login => "*",
    }
}

pub fn b64(data: &[u8]) -> String {
    STANDARD.encode(data)
}

pub fn unb64(data: &str) -> Option<Vec<u8>> {
    STANDARD.decode(data.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        assert_eq!(b64(&plain("tim", "tanstaaftanstaaf")), "AHRpbQB0YW5zdGFhZnRhbnN0YWFm");
        assert_eq!(
            xoauth2("someuser@example.com", "ya29.vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg"),
            b"user=someuser@example.com\x01auth=Bearer ya29.vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg\x01\x01"
                .to_vec()
        );
        assert_eq!(
            oauthbearer("user@example.com", "server.example.com", 143, "vF9dft4qmTc2Nvb3RlckBhbHRhdmlzdGEuY29tCg=="),
            b"n,a=user@example.com,\x01host=server.example.com\x01port=143\x01auth=Bearer vF9dft4qmTc2Nvb3RlckBhbHRhdmlzdGEuY29tCg==\x01\x01".to_vec()
        );
        assert_eq!(unb64("AHRpbQB0YW5zdGFhZnRhbnN0YWFm").unwrap(), plain("tim", "tanstaaftanstaaf"));
    }

    #[test]
    fn choosing() {
        let pw = Credentials::password("a", "b");
        let tok = Credentials::oauth2("a", "t");
        let offered: Vec<String> = vec!["login".into(), "PLAIN".into(), "XOAUTH2".into()];
        assert_eq!(choose(&pw, &offered, ""), Some(Mechanism::Plain));
        assert_eq!(choose(&pw, &offered[..1], ""), Some(Mechanism::Login));
        assert_eq!(choose(&tok, &offered, ""), Some(Mechanism::XOAuth2));
        assert_eq!(choose(&tok, &offered[..2], ""), None);
        assert_eq!(choose(&pw, &offered, "login"), Some(Mechanism::Login));
        assert_eq!(choose(&pw, &offered, "XOAUTH2"), None);
        assert_eq!(format!("{pw:?}"), "Password { user: \"a\", .. }");
    }
}
