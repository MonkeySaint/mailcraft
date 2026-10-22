//! glTF 2.0 models (`.gltf` with its buffers embedded as `data:` URIs, or binary `.glb`) read into a
//! wireframe: every mesh primitive's positions and its triangles or lines (Khronos glTF 2.0 specification).
//! Node transforms, other files the model points to, and compressed geometry are not read.

use base64::Engine as _;
use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

/// Most vertices and edges read.
const MAX_VERTS: usize = 300_000;
const MAX_EDGES: usize = 600_000;
/// Most accessor numbers read over the whole model, however many meshes and primitives share them.
const MAX_WORK: usize = 3_000_000;
/// Most JSON objects in a model's JSON (all levels, counted before it is parsed). Only the members the
/// reader uses are kept and everything else is skipped as it is read, so with this the JSON takes little
/// memory whatever its size (real models keep their geometry in buffers and have a few thousand objects).
const MAX_OBJECTS: usize = 200_000;

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i.checked_add(4)?)?.try_into().ok()?))
}

/// The JSON objects of `json` (`{` outside strings).
fn object_count(json: &[u8]) -> usize {
    let (mut n, mut in_str, mut escaped) = (0usize, false, false);
    for &b in json {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else if b == b'"' {
            in_str = true;
        } else if b == b'{' {
            n = n.saturating_add(1);
        }
    }
    n
}

fn drain_seq<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<(), A::Error> {
    while seq.next_element::<IgnoredAny>()?.is_some() {}
    Ok(())
}

fn drain_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

/// Visitor methods for the JSON kinds a lenient member skips: each gives `$none`.
macro_rules! skip_other_kinds {
    ($none:expr; $($kind:ident),*) => {
        $(skip_other_kinds!(@one $kind, $none);)*
    };
    (@one u64, $none:expr) => {
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> { Ok($none) }
    };
    (@one i64, $none:expr) => {
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> { Ok($none) }
    };
    (@one f64, $none:expr) => {
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> { Ok($none) }
    };
    (@one bool, $none:expr) => {
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> { Ok($none) }
    };
    (@one str, $none:expr) => {
        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> { Ok($none) }
    };
    (@one unit, $none:expr) => {
        fn visit_unit<E>(self) -> Result<Self::Value, E> { Ok($none) }
    };
    (@one seq, $none:expr) => {
        fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            drain_seq(seq).map(|()| $none)
        }
    };
    (@one map, $none:expr) => {
        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
            drain_map(map).map(|()| $none)
        }
    };
}

/// A non-negative integer member; anything else (a string, an array, a fraction) counts as absent.
fn loose_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Option<u64>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a number")
        }
        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
            Ok(u64::try_from(v).ok())
        }
        skip_other_kinds!(None; f64, bool, str, unit, seq, map);
    }
    d.deserialize_any(V)
}

/// A string member; anything else counts as absent.
fn loose_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Option<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string")
        }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(v.to_owned()))
        }
        fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        skip_other_kinds!(None; u64, i64, f64, bool, unit, seq, map);
    }
    d.deserialize_any(V)
}

/// An object member read as `T`; anything else counts as absent.
struct Loose<T>(Option<T>);

impl<T> Default for Loose<T> {
    fn default() -> Self {
        Loose(None)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Loose<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Loose<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                T::deserialize(de::value::MapAccessDeserializer::new(map)).map(|t| Loose(Some(t)))
            }
            skip_other_kinds!(Loose(None); u64, i64, f64, bool, str, unit, seq);
        }
        d.deserialize_any(V(std::marker::PhantomData))
    }
}

/// An array of objects kept as (position, object): its other values are skipped without storing anything,
/// and positions stay those of the file (references are by position).
struct Objects<T>(Vec<(usize, T)>);

impl<T> Default for Objects<T> {
    fn default() -> Self {
        Objects(Vec::new())
    }
}

impl<T> Objects<T> {
    fn get(&self, i: usize) -> Option<&T> {
        let at = self.0.binary_search_by_key(&i, |(n, _)| *n).ok()?;
        self.0.get(at).map(|(_, t)| t)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Objects<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Objects<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                let mut i = 0usize;
                while let Some(Loose(item)) = seq.next_element::<Loose<T>>()? {
                    if let Some(t) = item {
                        out.push((i, t));
                    }
                    i = i.saturating_add(1);
                }
                Ok(Objects(out))
            }
            skip_other_kinds!(Objects::default(); u64, i64, f64, bool, str, unit, map);
        }
        d.deserialize_any(V(std::marker::PhantomData))
    }
}

/// The members of a glTF document the reader uses; every other member is skipped as it is read.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Doc {
    asset: Option<IgnoredAny>,
    buffers: Objects<Buffer>,
    #[serde(rename = "bufferViews")]
    views: Objects<View>,
    accessors: Objects<Accessor>,
    meshes: Objects<Mesh>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Buffer {
    #[serde(deserialize_with = "loose_string")]
    uri: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct View {
    #[serde(deserialize_with = "loose_u64")]
    buffer: Option<u64>,
    #[serde(deserialize_with = "loose_u64")]
    byte_offset: Option<u64>,
    #[serde(deserialize_with = "loose_u64")]
    byte_stride: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Accessor {
    #[serde(deserialize_with = "loose_u64")]
    buffer_view: Option<u64>,
    #[serde(deserialize_with = "loose_u64")]
    byte_offset: Option<u64>,
    #[serde(deserialize_with = "loose_u64")]
    component_type: Option<u64>,
    #[serde(deserialize_with = "loose_u64")]
    count: Option<u64>,
    #[serde(rename = "type", deserialize_with = "loose_string")]
    kind: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Mesh {
    primitives: Objects<Primitive>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Primitive {
    attributes: Loose<Attributes>,
    #[serde(deserialize_with = "loose_u64")]
    indices: Option<u64>,
    #[serde(deserialize_with = "loose_u64")]
    mode: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Attributes {
    #[serde(rename = "POSITION", deserialize_with = "loose_u64")]
    position: Option<u64>,
}

/// The document in `json`; `None` when it is not a JSON object or holds more than [`MAX_OBJECTS`] objects.
fn read_doc(json: &[u8]) -> Option<Doc> {
    if object_count(json) > MAX_OBJECTS {
        return None;
    }
    serde_json::from_slice(json).ok()
}

/// The JSON and binary chunk of a `.glb` file.
fn glb(data: &[u8]) -> Option<(Doc, Option<&[u8]>)> {
    if !data.starts_with(b"glTF") || u32_at(data, 4)? != 2 {
        return None;
    }
    let json_len = usize::try_from(u32_at(data, 12)?).ok()?;
    if u32_at(data, 16)? != 0x4E4F_534A {
        return None;
    }
    let json = data.get(20..20usize.checked_add(json_len)?)?;
    let json = read_doc(json)?;
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
        let doc = read_doc(data)?;
        doc.asset.as_ref()?;
        (doc, None)
    };
    let buffers: Vec<(usize, Vec<u8>)> = json
        .buffers
        .0
        .iter()
        .map(|(i, b)| {
            let data = match b.uri.as_deref() {
                Some(uri) if uri.starts_with("data:") => uri
                    .split_once(',')
                    .and_then(|(_, b64)| base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok())
                    .unwrap_or_default(),
                None if *i == 0 => bin.map(<[u8]>::to_vec).unwrap_or_default(),
                _ => Vec::new(),
            };
            (*i, data)
        })
        .collect();
    let buffers = Objects(buffers);
    let (views, accessors) = (&json.views, &json.accessors);
    let num = |n: Option<u64>| n.and_then(|n| usize::try_from(n).ok());
    // Numbers still allowed to be read; every accessor read spends from it.
    let budget = std::cell::Cell::new(MAX_WORK);
    // An accessor's values as numbers, `comps` per element.
    let read = |idx: usize, limit: usize| -> Option<(Vec<f64>, usize)> {
        let a = accessors.get(idx)?;
        let view = views.get(num(a.buffer_view)?)?;
        let buf = buffers.get(num(view.buffer)?)?;
        let comps = match a.kind.as_deref()? {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => return None,
        };
        let (size, kind) = match num(a.component_type)? {
            5120 | 5121 => (1, 0),
            5122 | 5123 => (2, 1),
            5125 => (4, 2),
            5126 => (4, 3),
            _ => return None,
        };
        let count = num(a.count)?.min(limit).min(budget.get() / comps);
        budget.set(budget.get().saturating_sub(count.saturating_mul(comps)));
        let start = num(view.byte_offset).unwrap_or(0).checked_add(num(a.byte_offset).unwrap_or(0))?;
        let stride = num(view.byte_stride).filter(|s| *s > 0).unwrap_or(size * comps);
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
    'meshes: for (_, mesh) in &json.meshes.0 {
        for (_, prim) in &mesh.primitives.0 {
            if verts.len() >= MAX_VERTS || edges.len() >= MAX_EDGES || budget.get() == 0 {
                break 'meshes;
            }
            let Some(pos) = prim.attributes.0.as_ref().and_then(|a| num(a.position)) else { continue };
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
            let idx: Vec<usize> = match num(prim.indices) {
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
            match num(prim.mode).unwrap_or(4) {
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

    /// Regression: the JSON was parsed into a value tree (numbers about 16 times their text size) and its
    /// accessor and view arrays were copied. Arrays of plain values are now skipped without being stored,
    /// unknown members are never built, and a JSON with too many objects is refused before parsing.
    #[test]
    fn hostile_json_is_skipped_or_refused_without_building_it() {
        let uri = format!(
            "data:application/octet-stream;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(triangle_buffer())
        );
        // Millions of numbers where objects belong, and in an unknown member: skipped, the model still reads.
        let numbers = vec![0u8; 4_000_000];
        let mut d = doc(Some(uri.clone()));
        d["extras"] = serde_json::json!(numbers);
        let mut text = d.to_string();
        let zeros = "0,".repeat(4_000_000);
        text = text.replacen("\"accessors\":[", &format!("\"accessors\":[{zeros}"), 1);
        let doc_read = read_doc(text.as_bytes()).unwrap();
        assert_eq!(doc_read.accessors.0.len(), 2, "only the objects are kept");
        assert_eq!(doc_read.accessors.0.first().map(|(i, _)| *i), Some(4_000_000), "positions are kept");
        // The shifted positions no longer match the mesh's references: no geometry, no crash.
        assert!(parse(text.as_bytes()).is_none());
        let mut d = doc(Some(uri));
        d["extras"] = serde_json::json!(numbers);
        assert_eq!(parse(d.to_string().as_bytes()).map(|(v, e)| (v.len(), e.len())), Some((3, 3)));
        // Members of the wrong kind count as absent.
        let mut d = doc(None);
        d["accessors"][0]["count"] = serde_json::json!("3");
        d["meshes"][0]["primitives"][0]["attributes"] = serde_json::json!([1, 2]);
        assert!(read_doc(d.to_string().as_bytes()).is_some());
        // Too many objects: refused before parsing.
        let mut d = doc(None);
        d["extras"] = serde_json::json!(vec![serde_json::json!({}); MAX_OBJECTS + 1]);
        assert!(read_doc(d.to_string().as_bytes()).is_none());
        // Braces inside strings are not objects.
        assert_eq!(object_count(br#"{"a":"{{\"{"}"#), 1);
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
