//! Internationalised domain names in addresses (RFC 5890–5891, RFC 3492 Punycode via the `idna` crate's
//! strict UTS #46 processing): `ann@bücher.example` goes on the wire as `ann@xn--bcher-kva.example`.
//!
//! Only the domain is converted. A non-ASCII local part needs SMTPUTF8 (RFC 6531) and is left as it is.

/// Longest address accepted for conversion (RFC 5321 path limit is 256 octets; leave room for hostile input).
const MAX_ADDR: usize = 1024;

/// True when the domain of `addr` (after the last `@`) has non-ASCII characters.
pub fn has_idn(addr: &str) -> bool {
    addr.rsplit_once('@').is_some_and(|(_, d)| !d.is_ascii())
}

/// The ASCII (A-label) form of `domain`: lower-cased, mapped and Punycode-encoded per label. Errors when the
/// name is not a valid internationalised domain name (disallowed characters, bad hyphens, too long).
pub fn domain_to_ascii(domain: &str) -> Result<String, String> {
    let d = domain.trim();
    if d.is_empty() || d.len() > MAX_ADDR {
        return Err(format!("`{}` is not a domain name", short(d)));
    }
    let d = d.trim_end_matches('.');
    idna::domain_to_ascii_strict(d)
        .map_err(|_| format!("`{}` is not a valid international domain name", short(d)))
}

/// The Unicode (U-label) form of an ASCII `domain` for display; the input when it does not decode.
pub fn domain_to_unicode(domain: &str) -> String {
    let (u, res) = idna::domain_to_unicode(domain);
    if res.is_ok() { u } else { domain.to_owned() }
}

/// `addr` with its domain in ASCII form (unchanged when it already is ASCII).
pub fn addr_to_ascii(addr: &str) -> Result<String, String> {
    if addr.len() > MAX_ADDR {
        return Err("the address is too long".into());
    }
    match addr.rsplit_once('@') {
        Some((local, domain)) if !domain.is_ascii() => Ok(format!("{local}@{}", domain_to_ascii(domain)?)),
        _ => Ok(addr.to_owned()),
    }
}

fn short(s: &str) -> String {
    s.chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_convert_to_punycode_and_back() {
        // RFC 3492 / IDNA test vectors.
        assert_eq!(domain_to_ascii("bücher.example").unwrap(), "xn--bcher-kva.example");
        assert_eq!(domain_to_ascii("BÜCHER.example.").unwrap(), "xn--bcher-kva.example");
        assert_eq!(domain_to_ascii("例え.テスト").unwrap(), "xn--r8jz45g.xn--zckzah");
        assert_eq!(domain_to_ascii("example.com").unwrap(), "example.com");
        assert_eq!(domain_to_unicode("xn--bcher-kva.example"), "bücher.example");
        assert_eq!(addr_to_ascii("Ann@bücher.example").unwrap(), "Ann@xn--bcher-kva.example");
        assert_eq!(addr_to_ascii("ann@example.org").unwrap(), "ann@example.org");
        assert!(has_idn("ann@bücher.example") && !has_idn("ann@example.org") && !has_idn("nobody"));
        // Invalid names are refused, never panicked on.
        for bad in ["", "a b.example", "-bad-.example", "bü@cher.example", "\u{0}x.example"] {
            assert!(domain_to_ascii(bad).is_err(), "{bad:?}");
        }
        assert!(domain_to_ascii(&"ü".repeat(2000)).is_err());
    }
}
