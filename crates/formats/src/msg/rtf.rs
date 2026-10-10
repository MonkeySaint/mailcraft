//! Compressed RTF ([MS-OXRTFCP]): the LZ77-style "LZFu" format and the uncompressed "MELA" form.

use std::collections::HashMap;

/// Largest decompressed RTF produced.
pub const MAX_RTF_BYTES: usize = 64 * 1024 * 1024;

const COMPRESSED: u32 = 0x7546_5A4C; // "LZFu"
const UNCOMPRESSED: u32 = 0x414C_454D; // "MELA"
const DICT_SIZE: usize = 4096;
const MAX_MATCH: usize = 17;

/// The dictionary's initial contents ([MS-OXRTFCP] §2.1.2.1).
pub const INITIAL_DICTIONARY: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    let b = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?]))
}

/// CRC-32 used by the format (reflected polynomial 0xEDB88320, initial value 0, no final XOR).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for &b in data {
        let mut c = (crc ^ u32::from(b)) & 0xFF;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        crc = c ^ (crc >> 8);
    }
    crc
}

fn initial_dict() -> [u8; DICT_SIZE] {
    let mut dict = [0u8; DICT_SIZE];
    for (slot, &b) in dict.iter_mut().zip(INITIAL_DICTIONARY) {
        *slot = b;
    }
    dict
}

/// Decompresses a PidTagRtfCompressed value. The CRC is not enforced (damaged values decode best-effort).
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    let comp_size = u32_at(data, 0).ok_or("compressed RTF header too short")?;
    let raw_size = u32_at(data, 4).ok_or("compressed RTF header too short")?;
    let kind = u32_at(data, 8).ok_or("compressed RTF header too short")?;
    let raw_size = usize::try_from(raw_size).unwrap_or(usize::MAX).min(MAX_RTF_BYTES);
    let end = usize::try_from(comp_size).unwrap_or(usize::MAX).saturating_add(4).min(data.len());
    let body = data.get(16..end.max(16)).unwrap_or(&[]);
    match kind {
        UNCOMPRESSED => Ok(body.get(..raw_size.min(body.len())).unwrap_or(body).to_vec()),
        COMPRESSED => Ok(lzfu(body, raw_size)),
        other => Err(format!("unknown compressed RTF type {other:#010x}")),
    }
}

fn lzfu(body: &[u8], raw_size: usize) -> Vec<u8> {
    let mut dict = initial_dict();
    let mut wp = INITIAL_DICTIONARY.len();
    let mut out = Vec::with_capacity(raw_size.min(1 << 20));
    let mut i = 0usize;
    let cap = raw_size.min(MAX_RTF_BYTES);
    if cap == 0 {
        return out;
    }
    'outer: while let Some(&control) = body.get(i) {
        i += 1;
        for bit in 0..8 {
            if control & (1 << bit) == 0 {
                let Some(&b) = body.get(i) else { break 'outer };
                i += 1;
                out.push(b);
                if let Some(slot) = dict.get_mut(wp) {
                    *slot = b;
                }
                wp = (wp + 1) % DICT_SIZE;
            } else {
                let (Some(&hi), Some(&lo)) = (body.get(i), body.get(i + 1)) else { break 'outer };
                i += 2;
                let word = usize::from(hi) << 8 | usize::from(lo);
                let offset = word >> 4;
                let len = (word & 0xF) + 2;
                if offset == wp {
                    break 'outer;
                }
                for k in 0..len {
                    let b = dict.get((offset + k) % DICT_SIZE).copied().unwrap_or(0);
                    out.push(b);
                    if let Some(slot) = dict.get_mut(wp) {
                        *slot = b;
                    }
                    wp = (wp + 1) % DICT_SIZE;
                }
            }
            if out.len() >= cap {
                break 'outer;
            }
        }
    }
    out.truncate(cap);
    out
}

/// True when the ring ranges [a, a+len) and [b, b+len) overlap (mod the dictionary size).
fn ring_overlap(a: usize, b: usize, len: usize) -> bool {
    let d = (b + DICT_SIZE - a) % DICT_SIZE;
    let e = (a + DICT_SIZE - b) % DICT_SIZE;
    d < len || e < len
}

/// Compresses RTF into the LZFu form (greedy matching over a small hash index).
pub fn compress(raw: &[u8]) -> Vec<u8> {
    let mut dict = initial_dict();
    let mut wp = INITIAL_DICTIONARY.len();
    let mut index: HashMap<[u8; 2], Vec<usize>> = HashMap::new();
    let remember = |index: &mut HashMap<[u8; 2], Vec<usize>>, dict: &[u8; DICT_SIZE], pos: usize| {
        let key =
            [dict.get(pos).copied().unwrap_or(0), dict.get((pos + 1) % DICT_SIZE).copied().unwrap_or(0)];
        let list = index.entry(key).or_default();
        if list.len() >= 16 {
            list.remove(0);
        }
        list.push(pos);
    };
    for p in 0..INITIAL_DICTIONARY.len().saturating_sub(1) {
        remember(&mut index, &dict, p);
    }
    let mut body: Vec<u8> = Vec::with_capacity(raw.len() / 2 + 16);
    let mut control_at = 0usize;
    let mut bit = 8u8;
    let mut pos = 0usize;
    let mut done = false;
    while !done {
        if bit == 8 {
            control_at = body.len();
            body.push(0);
            bit = 0;
        }
        if pos >= raw.len() {
            // End marker: a reference whose offset is the write position.
            if let Some(c) = body.get_mut(control_at) {
                *c |= 1 << bit;
            }
            let word = (wp << 4) as u16;
            body.extend_from_slice(&word.to_be_bytes());
            done = true;
            continue;
        }
        let mut best = (0usize, 0usize);
        let key = [raw.get(pos).copied().unwrap_or(0), raw.get(pos + 1).copied().unwrap_or(0)];
        if pos + 1 < raw.len()
            && let Some(cands) = index.get(&key)
        {
            for &cand in cands.iter().rev() {
                let max = MAX_MATCH.min(raw.len() - pos);
                let mut len = 0;
                while len < max && dict.get((cand + len) % DICT_SIZE) == raw.get(pos + len) {
                    len += 1;
                }
                while len >= 2 && ring_overlap(cand, wp, len) {
                    len -= 1;
                }
                if len >= 2 && len > best.1 {
                    best = (cand, len);
                }
            }
        }
        let take = if best.1 >= 2 {
            if let Some(c) = body.get_mut(control_at) {
                *c |= 1 << bit;
            }
            let word = ((best.0 << 4) | (best.1 - 2)) as u16;
            body.extend_from_slice(&word.to_be_bytes());
            best.1
        } else {
            body.push(raw.get(pos).copied().unwrap_or(0));
            1
        };
        for k in 0..take {
            let b = raw.get(pos + k).copied().unwrap_or(0);
            if let Some(slot) = dict.get_mut(wp) {
                *slot = b;
            }
            let prev = (wp + DICT_SIZE - 1) % DICT_SIZE;
            remember(&mut index, &dict, prev);
            wp = (wp + 1) % DICT_SIZE;
        }
        pos += take;
        bit += 1;
    }
    let mut out = Vec::with_capacity(body.len() + 16);
    let comp_size = u32::try_from(body.len() + 12).unwrap_or(u32::MAX);
    out.extend_from_slice(&comp_size.to_le_bytes());
    out.extend_from_slice(&u32::try_from(raw.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&COMPRESSED.to_le_bytes());
    out.extend_from_slice(&crc32(&body).to_le_bytes());
    out.extend_from_slice(&body);
    out
}
