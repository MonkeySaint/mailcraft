//! NTLM authentication over HTTP ([MS-NLMP], the NTLMv2 response of section 3.3.2; [MS-NTHT] for the
//! `WWW-Authenticate: NTLM` exchange): a NEGOTIATE message, the server's CHALLENGE, and an AUTHENTICATE
//! message carrying the NTLMv2 response. No signing or sealing (the HTTP channel is protected by TLS), no
//! LM or NTLMv1 responses.
//!
//! The hashes NTLM needs and `ring` does not offer are implemented here from their public specifications:
//! MD4 (RFC 1320), MD5 (RFC 1321) and HMAC (RFC 2104). They serve this protocol only; nothing else here
//! relies on them for security.
//!
//! Every byte read from a CHALLENGE message is bounds-checked: a hostile server gets an error, never a panic.

use base64::Engine;

use crate::error::{NetError, Result};

const SIGNATURE: &[u8; 8] = b"NTLMSSP\0";
const NEGOTIATE_UNICODE: u32 = 0x0000_0001;
const NEGOTIATE_OEM: u32 = 0x0000_0002;
const REQUEST_TARGET: u32 = 0x0000_0004;
const NEGOTIATE_NTLM: u32 = 0x0000_0200;
const NEGOTIATE_ALWAYS_SIGN: u32 = 0x0000_8000;
const NEGOTIATE_EXTENDED_SESSIONSECURITY: u32 = 0x0008_0000;
const NEGOTIATE_TARGET_INFO: u32 = 0x0080_0000;
const NEGOTIATE_128: u32 = 0x2000_0000;
const NEGOTIATE_56: u32 = 0x8000_0000;
/// The flags MailCraft asks for.
const FLAGS: u32 = NEGOTIATE_UNICODE
    | NEGOTIATE_OEM
    | REQUEST_TARGET
    | NEGOTIATE_NTLM
    | NEGOTIATE_ALWAYS_SIGN
    | NEGOTIATE_EXTENDED_SESSIONSECURITY
    | NEGOTIATE_128
    | NEGOTIATE_56;
/// AV pair ids ([MS-NLMP] 2.2.2.1).
const AV_EOL: u16 = 0;
const AV_TIMESTAMP: u16 = 7;
/// Largest CHALLENGE message accepted.
const MAX_CHALLENGE: usize = 16 * 1024;

// ---------------------------------------------------------------------------------------------------
// MD4 (RFC 1320), MD5 (RFC 1321), HMAC-MD5 (RFC 2104)
// ---------------------------------------------------------------------------------------------------

/// The message padded to whole 64-byte blocks with its bit length (little-endian), as MD4 and MD5 do.
fn padded(data: &[u8]) -> Vec<u8> {
    let bits = u64::try_from(data.len()).unwrap_or(u64::MAX).wrapping_mul(8);
    let mut m = data.to_vec();
    m.push(0x80);
    while m.len() % 64 != 56 {
        m.push(0);
    }
    m.extend_from_slice(&bits.to_le_bytes());
    m
}

fn words(block: &[u8]) -> [u32; 16] {
    let mut x = [0u32; 16];
    for (w, c) in x.iter_mut().zip(block.as_chunks::<4>().0) {
        *w = u32::from_le_bytes(*c);
    }
    x
}

fn digest(state: [u32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (o, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *o = w.to_le_bytes();
    }
    out
}

fn word(x: &[u32; 16], i: usize) -> u32 {
    x.get(i).copied().unwrap_or(0)
}

/// MD4 (RFC 1320).
pub fn md4(data: &[u8]) -> [u8; 16] {
    let mut h: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    const R2: [usize; 16] = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15];
    const R3: [usize; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];
    for block in padded(data).as_chunks::<64>().0 {
        let x = words(block);
        let mut v = h;
        let step = |v: &mut [u32; 4], f: u32, k: u32, s: u32| {
            let [a, b, c, d] = *v;
            let t = a.wrapping_add(f).wrapping_add(k).rotate_left(s);
            *v = [d, t, b, c];
        };
        for (j, s) in (0..16usize).zip([3u32, 7, 11, 19].into_iter().cycle()) {
            let [_, b, c, d] = v;
            step(&mut v, (b & c) | (!b & d), word(&x, j), s);
        }
        for (j, s) in R2.into_iter().zip([3u32, 5, 9, 13].into_iter().cycle()) {
            let [_, b, c, d] = v;
            step(&mut v, ((b & c) | (b & d) | (c & d)).wrapping_add(0x5a82_7999), word(&x, j), s);
        }
        for (j, s) in R3.into_iter().zip([3u32, 9, 11, 15].into_iter().cycle()) {
            let [_, b, c, d] = v;
            step(&mut v, (b ^ c ^ d).wrapping_add(0x6ed9_eba1), word(&x, j), s);
        }
        for (hw, vw) in h.iter_mut().zip(v) {
            *hw = hw.wrapping_add(vw);
        }
    }
    digest(h)
}

/// K[i] = floor(abs(sin(i + 1)) * 2^32) (RFC 1321 section 3.4).
const MD5_K: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// MD5 (RFC 1321).
pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let mut h: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for block in padded(data).as_chunks::<64>().0 {
        let x = words(block);
        let mut v = h;
        for i in 0..64usize {
            let [a, b, c, d] = v;
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let k = MD5_K.get(i).copied().unwrap_or(0);
            let s = S.get((i / 16) * 4 + i % 4).copied().unwrap_or(0);
            let t = a.wrapping_add(f).wrapping_add(k).wrapping_add(word(&x, g)).rotate_left(s);
            v = [d, b.wrapping_add(t), b, c];
        }
        for (hw, vw) in h.iter_mut().zip(v) {
            *hw = hw.wrapping_add(vw);
        }
    }
    digest(h)
}

/// HMAC-MD5 (RFC 2104).
pub fn hmac_md5(key: &[u8], data: &[u8]) -> [u8; 16] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k.get_mut(..16).map(|d| d.copy_from_slice(&md5(key))).unwrap_or_default();
    } else if let Some(d) = k.get_mut(..key.len()) {
        d.copy_from_slice(key);
    }
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.extend_from_slice(data);
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.extend_from_slice(&md5(&inner));
    md5(&outer)
}

// ---------------------------------------------------------------------------------------------------
// NTLMv2
// ---------------------------------------------------------------------------------------------------

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// NTOWFv2 ([MS-NLMP] 3.3.2): HMAC-MD5 keyed with the MD4 hash of the password over the upper-case user name
/// and the domain.
pub fn ntowf_v2(password: &str, user: &str, domain: &str) -> [u8; 16] {
    let nt = md4(&utf16le(password));
    let mut who = utf16le(&user.to_uppercase());
    who.extend_from_slice(&utf16le(domain));
    hmac_md5(&nt, &who)
}

/// A user name split into (user, domain): `DOMAIN\user` gives both; `user@realm` (a UPN) and a bare name keep
/// the whole name as the user, with an empty domain ([MS-NLMP] 3.1.5.1.2 lets the server resolve a UPN).
pub fn split_user(name: &str) -> (String, String) {
    match name.split_once('\\') {
        Some((d, u)) => (u.to_owned(), d.to_owned()),
        None => (name.to_owned(), String::new()),
    }
}

/// The NEGOTIATE message, base64 (the `Authorization: NTLM …` value of the first request).
pub fn negotiate() -> String {
    let mut m = Vec::with_capacity(32);
    m.extend_from_slice(SIGNATURE);
    m.extend_from_slice(&1u32.to_le_bytes());
    m.extend_from_slice(&FLAGS.to_le_bytes());
    // Domain and workstation: empty, offset just past the header.
    for _ in 0..2 {
        m.extend_from_slice(&0u16.to_le_bytes());
        m.extend_from_slice(&0u16.to_le_bytes());
        m.extend_from_slice(&32u32.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(m)
}

/// What the client needs from a CHALLENGE message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    pub flags: u32,
    pub server_challenge: [u8; 8],
    /// The TargetInfo AV pairs, as sent (they go back inside the NTLMv2 response).
    pub target_info: Vec<u8>,
    /// MsvAvTimestamp, when the server sent one (FILETIME).
    pub timestamp: Option<u64>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// The bytes a security buffer descriptor (length, max length, offset) at `at` points to.
fn field(b: &[u8], at: usize) -> Option<&[u8]> {
    let len = usize::from(u16_at(b, at)?);
    let off = usize::try_from(u32_at(b, at.checked_add(4)?)?).ok()?;
    b.get(off..off.checked_add(len)?)
}

/// Reads a CHALLENGE message from the base64 of a `WWW-Authenticate: NTLM …` header value.
pub fn parse_challenge(b64: &str) -> Result<Challenge> {
    let bad = |why: &str| NetError::Auth(format!("the server's NTLM challenge is not valid ({why})"));
    if b64.len() > MAX_CHALLENGE {
        return Err(bad("too large"));
    }
    let b = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|_| bad("not base64"))?;
    if b.get(..8) != Some(SIGNATURE.as_slice()) || u32_at(&b, 8) != Some(2) {
        return Err(bad("not a challenge message"));
    }
    let flags = u32_at(&b, 20).ok_or_else(|| bad("truncated"))?;
    let server_challenge: [u8; 8] =
        b.get(24..32).and_then(|c| c.try_into().ok()).ok_or_else(|| bad("truncated"))?;
    let target_info = if flags & NEGOTIATE_TARGET_INFO != 0 {
        field(&b, 40).ok_or_else(|| bad("bad target info"))?.to_vec()
    } else {
        Vec::new()
    };
    // Walk the AV pairs (bounded by the buffer): only the timestamp matters here.
    let mut timestamp = None;
    let mut at = 0usize;
    while let (Some(id), Some(len)) = (u16_at(&target_info, at), u16_at(&target_info, at.saturating_add(2))) {
        if id == AV_EOL {
            break;
        }
        let start = at.saturating_add(4);
        let end = start.saturating_add(usize::from(len));
        let Some(value) = target_info.get(start..end) else { return Err(bad("truncated AV pair")) };
        if id == AV_TIMESTAMP && value.len() == 8 {
            timestamp = value.try_into().ok().map(u64::from_le_bytes);
        }
        at = end;
    }
    Ok(Challenge { flags, server_challenge, target_info, timestamp })
}

/// The NTLMv2 response and the LMv2 response ([MS-NLMP] 3.3.2) for `challenge`, given the client challenge
/// and the time (FILETIME: 100 ns since 1601-01-01).
pub fn responses(
    key: &[u8; 16],
    challenge: &Challenge,
    client_challenge: &[u8; 8],
    time: u64,
) -> (Vec<u8>, Vec<u8>) {
    let mut temp = vec![1u8, 1, 0, 0, 0, 0, 0, 0];
    temp.extend_from_slice(&time.to_le_bytes());
    temp.extend_from_slice(client_challenge);
    temp.extend_from_slice(&[0; 4]);
    temp.extend_from_slice(&challenge.target_info);
    temp.extend_from_slice(&[0; 4]);
    let mut msg = challenge.server_challenge.to_vec();
    msg.extend_from_slice(&temp);
    let proof = hmac_md5(key, &msg);
    let mut nt = proof.to_vec();
    nt.extend_from_slice(&temp);
    let lm = if challenge.timestamp.is_some() {
        // With a server timestamp the LM response is all zeros ([MS-NLMP] 3.1.5.1.2).
        vec![0u8; 24]
    } else {
        let mut m = challenge.server_challenge.to_vec();
        m.extend_from_slice(client_challenge);
        let mut lm = hmac_md5(key, &m).to_vec();
        lm.extend_from_slice(client_challenge);
        lm
    };
    (nt, lm)
}

/// FILETIME of now.
fn filetime_now() -> u64 {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() / 100)
        .unwrap_or(0);
    u64::try_from(unix).unwrap_or(0).saturating_add(116_444_736_000_000_000)
}

/// The AUTHENTICATE message for `challenge_b64`, base64 (the `Authorization: NTLM …` value of the second
/// request). `user` may be `DOMAIN\user`, a UPN or a bare name.
pub fn authenticate(challenge_b64: &str, user: &str, password: &str) -> Result<String> {
    let challenge = parse_challenge(challenge_b64)?;
    let mut client_challenge = [0u8; 8];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut client_challenge)
        .map_err(|_| NetError::Auth("no random numbers for the NTLM sign-in".into()))?;
    let time = challenge.timestamp.unwrap_or_else(filetime_now);
    Ok(base64::engine::general_purpose::STANDARD.encode(authenticate_message(
        &challenge,
        user,
        password,
        &client_challenge,
        time,
    )))
}

/// The AUTHENTICATE message bytes (no MIC, no session key exchange).
pub fn authenticate_message(
    challenge: &Challenge,
    user: &str,
    password: &str,
    client_challenge: &[u8; 8],
    time: u64,
) -> Vec<u8> {
    let (name, domain) = split_user(user);
    let key = ntowf_v2(password, &name, &domain);
    let (nt, lm) = responses(&key, challenge, client_challenge, time);
    let fields: [Vec<u8>; 5] = [lm, nt, utf16le(&domain), utf16le(&name), utf16le("MAILCRAFT")];
    let mut header = Vec::with_capacity(64);
    header.extend_from_slice(SIGNATURE);
    header.extend_from_slice(&3u32.to_le_bytes());
    let mut payload = Vec::new();
    let mut offset = 64u32;
    for f in &fields {
        let len = u16::try_from(f.len()).unwrap_or(u16::MAX);
        header.extend_from_slice(&len.to_le_bytes());
        header.extend_from_slice(&len.to_le_bytes());
        header.extend_from_slice(&offset.to_le_bytes());
        offset = offset.saturating_add(u32::from(len));
        payload.extend_from_slice(f.get(..usize::from(len)).unwrap_or(f));
    }
    // No encrypted random session key.
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&offset.to_le_bytes());
    let flags = (challenge.flags & FLAGS) | NEGOTIATE_UNICODE | NEGOTIATE_NTLM;
    header.extend_from_slice(&flags.to_le_bytes());
    header.extend_from_slice(&payload);
    header
}

/// The NTLM token in a `WWW-Authenticate` header value (`NTLM <base64>`), if it carries one.
pub fn challenge_token(header: &str) -> Option<&str> {
    let h = header.trim();
    let rest = h.get(..4).filter(|p| p.eq_ignore_ascii_case("ntlm")).and(h.get(4..))?;
    let t = rest.trim();
    (!t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn hashes_match_their_rfc_test_vectors() {
        // RFC 1320 appendix A.5.
        assert_eq!(hex(&md4(b"")), "31d6cfe0d16ae931b73c59d7e0c089c0");
        assert_eq!(hex(&md4(b"abc")), "a448017aaf21d8525fc10ae87aa6729d");
        assert_eq!(
            hex(&md4(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890")),
            "e33b4ddc9c38f2199c3e7b164fcc0536"
        );
        // RFC 1321 appendix A.5.
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex(&md5(b"message digest")), "f96b697d7cb7938d525a2f31aaf161d0");
        // RFC 2104 section 4 / RFC 2202.
        assert_eq!(hex(&hmac_md5(&[0x0b; 16], b"Hi There")), "9294727a3638bb1c13f48ef8158bfc9d");
        assert_eq!(
            hex(&hmac_md5(b"Jefe", b"what do ya want for nothing?")),
            "750c783e6ab0b503eaa86e310a5db738"
        );
        assert_eq!(
            hex(&hmac_md5(&[0xaa; 80], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "6b1ab7fe4bd7bf8f0b62e6ce61b9d0cd"
        );
    }

    /// [MS-NLMP] section 4.2.4: the NTLMv2 example (user "User", domain "Domain", password "Password").
    fn spec_challenge() -> Challenge {
        let mut info = Vec::new();
        for (id, v) in [(2u16, "Domain"), (1u16, "Server")] {
            let v = utf16le(v);
            info.extend_from_slice(&id.to_le_bytes());
            info.extend_from_slice(&u16::try_from(v.len()).unwrap().to_le_bytes());
            info.extend_from_slice(&v);
        }
        info.extend_from_slice(&[0; 4]);
        Challenge {
            flags: 0xe28a_8233,
            server_challenge: [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef],
            target_info: info,
            timestamp: None,
        }
    }

    #[test]
    fn ntlmv2_matches_the_specification_example() {
        assert_eq!(hex(&md4(&utf16le("Password"))), "a4f49c406510bdcab6824ee7c30fd852");
        let key = ntowf_v2("Password", "User", "Domain");
        assert_eq!(hex(&key), "0c868a403bfd7a93a3001ef22ef02e3f");
        let (nt, lm) = responses(&key, &spec_challenge(), &[0xaa; 8], 0);
        assert_eq!(hex(&lm), "86c35097ac9cec102554764a57cccc19aaaaaaaaaaaaaaaa");
        assert_eq!(hex(&nt[..16]), "68cd0ab851e51c96aabc927bebef6a1c");
    }

    #[test]
    fn messages_round_trip_and_hostile_challenges_are_errors() {
        let n = base64::engine::general_purpose::STANDARD.decode(negotiate()).unwrap();
        assert_eq!(&n[..8], SIGNATURE);
        assert_eq!(u32_at(&n, 8), Some(1));
        // A challenge built like a server's, with a timestamp.
        let c = spec_challenge();
        let mut info = vec![7u8, 0, 8, 0];
        info.extend_from_slice(&42u64.to_le_bytes());
        info.extend_from_slice(&c.target_info);
        let mut m = SIGNATURE.to_vec();
        m.extend_from_slice(&2u32.to_le_bytes());
        m.extend_from_slice(&[0, 0, 0, 0, 48, 0, 0, 0]);
        m.extend_from_slice(&(c.flags | NEGOTIATE_TARGET_INFO).to_le_bytes());
        m.extend_from_slice(&c.server_challenge);
        m.extend_from_slice(&[0; 8]);
        let len = u16::try_from(info.len()).unwrap();
        m.extend_from_slice(&len.to_le_bytes());
        m.extend_from_slice(&len.to_le_bytes());
        m.extend_from_slice(&48u32.to_le_bytes());
        m.extend_from_slice(&info);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&m);
        let parsed = parse_challenge(&b64).unwrap();
        assert_eq!(parsed.timestamp, Some(42));
        assert_eq!(parsed.server_challenge, c.server_challenge);
        let a = authenticate_message(&parsed, "EXAMPLE\\pat", "pw", &[1; 8], 42);
        assert_eq!(u32_at(&a, 8), Some(3));
        assert_eq!(field(&a, 12).unwrap(), &[0u8; 24][..], "zero LM response with a server timestamp");
        assert_eq!(field(&a, 28).unwrap(), utf16le("EXAMPLE").as_slice());
        assert_eq!(field(&a, 36).unwrap(), utf16le("pat").as_slice());
        assert!(authenticate(&b64, "pat@example.com", "pw").is_ok());
        assert_eq!(challenge_token("NTLM abc"), Some("abc"));
        assert_eq!(challenge_token("ntlm"), None);
        assert_eq!(challenge_token("Basic realm=x"), None);
        // Truncated, lying offsets, wrong type, junk: errors.
        for bad in [&m[..20], &m[..30]] {
            assert!(parse_challenge(&base64::engine::general_purpose::STANDARD.encode(bad)).is_err());
        }
        let mut lying = m.clone();
        lying[44..48].copy_from_slice(&60_000u32.to_le_bytes());
        assert!(parse_challenge(&base64::engine::general_purpose::STANDARD.encode(&lying)).is_err());
        let mut av = m.clone();
        let last = av.len() - 6;
        av[last..last + 2].copy_from_slice(&500u16.to_le_bytes());
        let _ = parse_challenge(&base64::engine::general_purpose::STANDARD.encode(&av));
        assert!(parse_challenge("%%%").is_err());
        assert!(
            parse_challenge(&base64::engine::general_purpose::STANDARD.encode(b"NTLMSSP\0\x01\0\0\0"))
                .is_err()
        );
        assert_eq!(split_user("pat"), ("pat".to_owned(), String::new()));
    }
}
