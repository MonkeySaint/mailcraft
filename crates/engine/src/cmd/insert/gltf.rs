//! glTF 2.0 models (`.gltf` with its buffers embedded as `data:` URIs, or binary `.glb`) read into a
//! wireframe: every mesh primitive's positions and its triangles or lines (Khronos glTF 2.0 specification).
//! Node transforms, other files the model points to, and compressed geometry are not read.

use base64::Engine as _;
use serde_json::Value;

/// Most vertices and edges read.
const MAX_VERTS: usize = 300_000;
const MAX_EDGES: usize = 600_000;
/// Most accessor numbers read over the whole model, however many meshes and primitives share them.
const MAX_WORK: usize = 3_000_000;

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i.checked_add(4)?)?.try_into().ok()?))
}

/// The JSON and binary chunk of a `.glb` file.
fn glb(data: &[u8]) -> Option<(Value, Option<&[u8]>)> {
    if !data.starts_with(b"glTF") || u32_at(data, 4)? != 2 {
        return None;
    }
    let json_len = usize::try_from(u32_at(data, 12)?).ok()?;
    if u32_at(data, 16)? != 0x4E4F_534A {
        return None;
    }
    let json = data.get(20..20usize.checked_add(json_len)?)?;
    let json: Value = serde_json::from_slice(json).ok()?;
    let bin_at = 20usize.checked_add(json_len)?;
    let bin = (|| {
        let len = usize::try_from(u32_at(data, bin_at)?).ok()?;
        if u32_at(data, bin_at.checked_add(4)?)? != 0x004E_4942 {
            return None;
        }
        let start = bin_at.checked_add(8)?;
        data.get(start..start.checked_add(len)?)
    })();
    Some((json, bin))
}

/// Reads a glTF or GLB model into (vertices, edges); `None` when it is not one or has no geometry.
pub fn parse(data: &[u8]) -> Option<(Vec<[f64; 3]>, Vec<(usize, usize)>)> {
    let (json, bin) = if data.starts_with(b"glTF") {
        glb(data)?
    } else {
        let v: Value = serde_json::from_slice(data).ok()?;
        v.get("asset")?;
        (v, None)
    };
    let buffers: Vec<Vec<u8>> = json
        .get("buffers")
        .and_then(Value::as_array)
        .map(|bs| {
            bs.iter()
                .enumerate()
                .map(|(i, b)| match b.get("uri").and_then(Value::as_str) {
                    Some(uri) if uri.starts_with("data:") => uri
                        .split_once(',')
                        .and_then(|(_, b64)| {
                            base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()
                        })
                        .unwrap_or_default(),
                    None if i == 0 => bin.map(<[u8]>::to_vec).unwrap_or_default(),
                    _ => Vec::new(),
                })
                .collect()
        })
        .unwrap_or_default();
    let views = json.get("bufferViews").and_then(Value::as_array).cloned().unwrap_or_default();
    let accessors = json.get("accessors").and_then(Value::as_array).cloned().unwrap_or_default();
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok());
    // Numbers still allowed to be read; every accessor read spends from it.
    let budget = std::cell::Cell::new(MAX_WORK);
    // An accessor's values as numbers, `comps` per element.
    let read = |idx: usize, limit: usize| -> Option<(Vec<f64>, usize)> {
        let a = accessors.get(idx)?;
        let view = views.get(num(a, "bufferView")?)?;
        let buf = buffers.get(num(view, "buffer")?)?;
        let comps = match a.get("type")?.as_str()? {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => return None,
        };
        let (size, kind) = match num(a, "componentType")? {
            5120 | 5121 => (1, 0),
            5122 | 5123 => (2, 1),
            5125 => (4, 2),
            5126 => (4, 3),
            _ => return None,
        };
        let count = num(a, "count")?.min(limit).min(budget.get() / comps);
        budget.set(budget.get().saturating_sub(count.saturating_mul(comps)));
        let start = num(view, "byteOffset").unwrap_or(0).checked_add(num(a, "byteOffset").unwrap_or(0))?;
        let stride = num(view, "byteStride").filter(|s| *s > 0).unwrap_or(size * comps);
        let mut out = Vec::with_capacity(count.saturating_mul(comps).min(1 << 20));
        for e in 0..count {
            for k in 0..comps {
                let at = start.checked_add(e.checked_mul(stride)?)?.checked_add(k * size)?;
                let bytes = buf.get(at..at.checked_add(size)?)?;
                let v = match (kind, bytes) {
                    (0, [b]) => f64::from(*b),
                    (1, [a, b]) => f64::from(u16::from_le_bytes([*a, *b])),
                    (2, [a, b, c, d]) => f64::from(u32::from_le_bytes([*a, *b, *c, *d])),
                    (3, [a, b, c, d]) => f64::from(f32::from_le_bytes([*a, *b, *c, *d])),
                    _ => return None,
                };
                out.push(v);
            }
        }
        Some((out, comps))
    };
    let mut verts: Vec<[f64; 3]> = Vec::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    'meshes: for mesh in json.get("meshes").and_then(Value::as_array).into_iter().flatten() {
        for prim in mesh.get("primitives").and_then(Value::as_array).into_iter().flatten() {
            if verts.len() >= MAX_VERTS || edges.len() >= MAX_EDGES || budget.get() == 0 {
                break 'meshes;
            }
            let Some(pos) = prim.get("attributes").and_then(|a| num(a, "POSITION")) else { continue };
            let Some((p, 3)) = read(pos, MAX_VERTS.saturating_sub(verts.len())) else { continue };
            let base = verts.len();
            let n = p.len() / 3;
            // Fewer than two vertices make no edge: skip before reading any indices.
            if n < 2 {
                continue;
            }
            for c in p.chunks(3) {
                if let [x, y, z] = c
                    && x.is_finite()
                    && y.is_finite()
                    && z.is_finite()
                {
                    verts.push([*x, *y, *z]);
                } else {
                    verts.push([0.0; 3]);
                }
            }
            let idx: Vec<usize> = match num(prim, "indices") {
                Some(i) => read(i, MAX_EDGES.saturating_sub(edges.len()).saturating_mul(3))
                    .map(|(v, _)| v.into_iter().map(|x| x as usize).filter(|x| *x < n).collect())
                    .unwrap_or_default(),
                None => (0..n).collect(),
            };
            let mut edge = |a: usize, b: usize| {
                if edges.len() < MAX_EDGES && a != b {
                    edges.push((base + a, base + b));
                }
            };
            match num(prim, "mode").unwrap_or(4) {
                1 => idx.chunks(2).for_each(|c| {
                    if let [a, b] = c {
                        edge(*a, *b);
                    }
                }),
                2 | 3 => idx.windows(2).for_each(|w| {
                    if let [a, b] = w {
                        edge(*a, *b);
                    }
                }),
                4 => idx.chunks(3).for_each(|t| {
                    if let [a, b, c] = t {
                        edge(*a, *b);
                        edge(*b, *c);
                        edge(*c, *a);
                    }
                }),
                // Triangle strip: every three neighbours.
                5 => idx.windows(3).for_each(|t| {
                    if let [a, b, c] = t {
                        edge(*a, *b);
                        edge(*b, *c);
                        edge(*c, *a);
                    }
                }),
                // Triangle fan: the first vertex with every two neighbours after it.
                6 => {
                    if let Some((first, rest)) = idx.split_first() {
                        rest.windows(2).for_each(|w| {
                            if let [b, c] = w {
                                edge(*first, *b);
                                edge(*b, *c);
                                edge(*c, *first);
                            }
                        });
                    }
                }
                _ => {}
            }
        }
    }
    (!verts.is_empty() && !edges.is_empty()).then_some((verts, edges))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A triangle as glTF: three float positions and u16 indices in one embedded buffer.
    fn triangle_buffer() -> Vec<u8> {
        let mut b = Vec::new();
        for v in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.] {
            b.extend(v.to_le_bytes());
        }
        for i in [0u16, 1, 2] {
            b.extend(i.to_le_bytes());
        }
        b
    }

    fn doc(uri: Option<String>) -> serde_json::Value {
        let mut buffer = serde_json::json!({"byteLength": 42});
        if let Some(u) = uri {
            buffer["uri"] = serde_json::json!(u);
        }
        serde_json::json!({
            "asset": {"version": "2.0"},
            "buffers": [buffer],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 6}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}
            ],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": 1}]}]
        })
    }

    #[test]
    fn gltf_with_an_embedded_buffer_and_glb() {
        let uri = format!(
            "data:application/octet-stream;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(triangle_buffer())
        );
        let (v, e) = parse(doc(Some(uri)).to_string().as_bytes()).unwrap();
        assert_eq!((v.len(), e.len()), (3, 3));
        assert_eq!(v[1], [1.0, 0.0, 0.0]);
        // The same as binary glTF.
        let json = doc(None).to_string().into_bytes();
        let mut json_chunk = json.clone();
        while !json_chunk.len().is_multiple_of(4) {
            json_chunk.push(b' ');
        }
        let mut bin = triangle_buffer();
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let mut glb = Vec::new();
        glb.extend(b"glTF");
        glb.extend(2u32.to_le_bytes());
        glb.extend(((12 + 8 + json_chunk.len() + 8 + bin.len()) as u32).to_le_bytes());
        glb.extend((json_chunk.len() as u32).to_le_bytes());
        glb.extend(0x4E4F_534Au32.to_le_bytes());
        glb.extend(&json_chunk);
        glb.extend((bin.len() as u32).to_le_bytes());
        glb.extend(0x004E_4942u32.to_le_bytes());
        glb.extend(&bin);
        let (v, e) = parse(&glb).unwrap();
        assert_eq!((v.len(), e.len()), (3, 3));
    }

    #[test]
    fn broken_gltf_is_refused_without_crashing() {
        assert!(parse(b"{}").is_none());
        assert!(parse(b"glTF\x02\0\0\0").is_none());
        // Accessors pointing past their buffer.
        let mut d = doc(Some("data:,AAAA".into()));
        d["accessors"][0]["count"] = serde_json::json!(u64::MAX);
        assert!(parse(d.to_string().as_bytes()).is_none());
        let mut d = doc(None);
        d["bufferViews"][0]["byteStride"] = serde_json::json!(u64::MAX);
        assert!(parse(d.to_string().as_bytes()).is_none());
    }

    /// Regression: many meshes sharing one large index accessor used to re-read it for every mesh
    /// (about 1e10 reads for this model), hanging the window that inserted it.
    #[test]
    fn many_meshes_sharing_a_large_index_accessor_stop_at_the_work_budget() {
        let indices = 1_800_000usize;
        let mut buf = Vec::new();
        for v in [0f32, 0., 0., 1., 0., 0.] {
            buf.extend(v.to_le_bytes());
        }
        // Every index is 0, so no edge is ever made and the edge cap never ends the loop.
        buf.resize(24 + indices, 0);
        let uri = format!(
            "data:application/octet-stream;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&buf)
        );
        let mesh =
            serde_json::json!({"primitives": [{"attributes": {"POSITION": 0}, "indices": 1, "mode": 1}]});
        let d = serde_json::json!({
            "asset": {"version": "2.0"},
            "buffers": [{"uri": uri, "byteLength": buf.len()}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 24}, {"buffer": 0, "byteOffset": 24, "byteLength": indices}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 2, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5121, "count": indices, "type": "SCALAR"}
            ],
            "meshes": vec![mesh; 20_000]
        });
        let started = std::time::Instant::now();
        assert!(parse(d.to_string().as_bytes()).is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(60));
    }
}
