use super::*;
use proptest::prelude::*;

fn db() -> FontDb {
    FontDb::bundled()
}

fn family(db: &FontDb, id: FontId) -> &'static str {
    db.face(id).map(|f| f.family).unwrap_or("?")
}

fn style(db: &FontDb, id: FontId) -> (&'static str, u16, bool) {
    let f = db.face(id).unwrap();
    (f.family, f.weight, f.italic)
}

#[test]
fn bundled_has_all_faces_and_families() {
    let db = db();
    assert_eq!(db.faces.len(), 15);
    assert_eq!(
        db.families(),
        vec!["Inter", "Liberation Sans", "Liberation Serif", "Liberation Mono", "Carlito"]
    );
    let inter: Vec<u16> = db.faces.iter().filter(|f| f.family == "Inter").map(|f| f.weight).collect();
    assert_eq!(inter, vec![400, 500, 600]);
    assert_eq!(style(&db, FontId(0)), ("Inter", 400, false));
}

#[test]
fn every_face_parses() {
    let db = db();
    for (i, face) in db.faces.iter().enumerate() {
        let id = FontId(i as u16);
        assert!(face.data.len() > 10_000, "{} too small", face.family);
        assert!(skrifa::FontRef::new(face.data).is_ok(), "{} {} fails to parse", face.family, face.weight);
        assert_eq!(db.data(id).len(), face.data.len());
        let info = db.info(id).unwrap();
        assert!(info.font.is_some());
        assert!(info.advances.len() > 200, "{} has {} glyphs", face.family, info.advances.len());
        assert!(db.units_per_em(id) >= 1000.0);
        for c in 'A'..='Z' {
            assert_ne!(db.glyph_id(id, c), 0, "{} lacks {c}", face.family);
        }
    }
}

#[test]
fn licences_bundled() {
    assert_eq!(LICENSES.len(), 3);
    for (_, text) in LICENSES {
        assert!(text.contains("SIL OPEN FONT LICENSE") || text.contains("Open Font License"));
    }
}

#[test]
fn aliases_resolve() {
    let db = db();
    let cases = [
        ("Arial", "Liberation Sans"),
        ("Helvetica", "Liberation Sans"),
        ("Verdana", "Liberation Sans"),
        ("Tahoma", "Liberation Sans"),
        ("sans-serif", "Liberation Sans"),
        ("Aptos", "Inter"),
        ("Segoe UI", "Inter"),
        ("system-ui", "Inter"),
        ("-apple-system", "Inter"),
        ("Calibri", "Carlito"),
        ("Times", "Liberation Serif"),
        ("Times New Roman", "Liberation Serif"),
        ("Georgia", "Liberation Serif"),
        ("Cambria", "Liberation Serif"),
        ("serif", "Liberation Serif"),
        ("Courier", "Liberation Mono"),
        ("Courier New", "Liberation Mono"),
        ("Consolas", "Liberation Mono"),
        ("monospace", "Liberation Mono"),
        ("Inter", "Inter"),
        ("Carlito", "Carlito"),
        ("Liberation Serif", "Liberation Serif"),
        ("Wingdings Unknown", "Inter"),
        ("", "Inter"),
    ];
    for (name, want) in cases {
        assert_eq!(family(&db, db.resolve(name, 400, false)), want, "{name}");
    }
}

#[test]
fn resolve_is_case_insensitive_and_unquotes() {
    let db = db();
    for name in ["ARIAL", "arial", "'Arial'", "\"Arial\"", "  Arial  ", "\"  arial \""] {
        assert_eq!(family(&db, db.resolve(name, 400, false)), "Liberation Sans", "{name}");
    }
    assert_eq!(family(&db, db.resolve("'Times   New\tRoman'", 400, false)), "Liberation Serif");
    assert_eq!(family(&db, db.resolve("\"Courier, New\", serif", 400, false)), "Liberation Serif");
}

#[test]
fn comma_lists_first_match_wins() {
    let db = db();
    assert_eq!(family(&db, db.resolve("Aptos, Calibri, sans-serif", 400, false)), "Inter");
    assert_eq!(family(&db, db.resolve("Calibri, Arial", 400, false)), "Carlito");
    assert_eq!(
        family(&db, db.resolve("NoSuchFont, 'Other One', Consolas, serif", 400, false)),
        "Liberation Mono"
    );
    assert_eq!(family(&db, db.resolve(",,, ,Georgia", 400, false)), "Liberation Serif");
    assert_eq!(family(&db, db.resolve("Nope, Nada", 700, false)), "Inter");
}

#[test]
fn bold_and_italic_matching() {
    let db = db();
    assert_eq!(style(&db, db.resolve("Arial", 700, false)), ("Liberation Sans", 700, false));
    assert_eq!(style(&db, db.resolve("Arial", 400, true)), ("Liberation Sans", 400, true));
    assert_eq!(style(&db, db.resolve("Arial", 700, true)), ("Liberation Sans", 700, true));
    assert_eq!(style(&db, db.resolve("Arial", 900, false)), ("Liberation Sans", 700, false));
    assert_eq!(style(&db, db.resolve("Arial", 100, false)), ("Liberation Sans", 400, false));
    // Italic falls back to upright when the family has no italic.
    assert_eq!(style(&db, db.resolve("Consolas", 400, true)), ("Liberation Mono", 400, false));
    assert_eq!(style(&db, db.resolve("Consolas", 700, true)), ("Liberation Mono", 700, false));
    // Style wins over weight (CSS order): Carlito has no bold italic.
    assert_eq!(style(&db, db.resolve("Calibri", 700, true)), ("Carlito", 400, true));
    assert_eq!(style(&db, db.resolve("Times New Roman", 700, true)), ("Liberation Serif", 400, true));
    // Inter nearest weight.
    assert_eq!(style(&db, db.resolve("Aptos", 400, false)), ("Inter", 400, false));
    assert_eq!(style(&db, db.resolve("Aptos", 500, false)), ("Inter", 500, false));
    assert_eq!(style(&db, db.resolve("Aptos", 450, false)), ("Inter", 500, false));
    assert_eq!(style(&db, db.resolve("Aptos", 600, false)), ("Inter", 600, false));
    assert_eq!(style(&db, db.resolve("Aptos", 700, false)), ("Inter", 600, false));
    assert_eq!(style(&db, db.resolve("Aptos", 300, false)), ("Inter", 400, false));
    assert_eq!(style(&db, db.resolve("Aptos", 0, false)), ("Inter", 400, false));
    assert_eq!(style(&db, db.resolve("Aptos", u16::MAX, false)), ("Inter", 600, false));
    // Inter has no italic: upright.
    assert_eq!(style(&db, db.resolve("Segoe UI", 400, true)), ("Inter", 400, false));
}

#[test]
fn style_words_in_family_names() {
    let db = db();
    assert_eq!(style(&db, db.resolve("Segoe UI Semibold", 400, false)), ("Inter", 600, false));
    assert_eq!(style(&db, db.resolve("Segoe UI Light", 400, false)), ("Inter", 400, false));
    assert_eq!(style(&db, db.resolve("Arial Black", 400, false)), ("Liberation Sans", 700, false));
    assert_eq!(style(&db, db.resolve("Arial Bold Italic", 400, false)), ("Liberation Sans", 700, true));
    assert_eq!(style(&db, db.resolve("Calibri Light", 400, false)), ("Carlito", 400, false));
    assert_eq!(style(&db, db.resolve("Aptos (Body)", 400, false)), ("Inter", 400, false));
    assert_eq!(family(&db, db.resolve("Aptos Mono", 400, false)), "Liberation Mono");
    assert_eq!(family(&db, db.resolve("Aptos Serif", 400, false)), "Liberation Serif");
    // An explicitly requested weight overrides the name's hint.
    assert_eq!(style(&db, db.resolve("Segoe UI Light", 600, false)), ("Inter", 600, false));
    // A lone style word is not a family.
    assert_eq!(family(&db, db.resolve("Bold, Courier", 400, false)), "Liberation Mono");
}

#[test]
fn bundled_family_for_works() {
    assert_eq!(bundled_family_for("ARIAL"), Some("Liberation Sans"));
    assert_eq!(bundled_family_for("Calibri Light"), Some("Carlito"));
    assert_eq!(bundled_family_for("Segoe UI"), Some("Inter"));
    assert_eq!(bundled_family_for("Nope"), None);
}

#[test]
fn synthesis_flags() {
    let db = db();
    let carlito_italic = db.resolve("Calibri", 700, true);
    assert_eq!(db.synthesis(carlito_italic, 700, true), Synthesis { bold: true, oblique: false });
    let mono = db.resolve("Consolas", 400, true);
    assert_eq!(db.synthesis(mono, 400, true), Synthesis { bold: false, oblique: true });
    let sans = db.resolve("Arial", 700, true);
    assert_eq!(db.synthesis(sans, 700, true), Synthesis::default());
    assert_eq!(db.synthesis(FontId(999), 700, true), Synthesis::default());
}

#[test]
fn metrics_sane() {
    let db = db();
    for i in 0..db.faces.len() {
        let id = FontId(i as u16);
        let m = db.metrics(id, 16.0);
        assert!(m.ascent > 10.0 && m.ascent < 20.0, "{i}: {m:?}");
        assert!(m.descent > 2.0 && m.descent < 8.0, "{i}: {m:?}");
        assert!(m.line_gap >= 0.0 && m.line_gap < 8.0, "{i}: {m:?}");
        assert!(m.line_height() > 16.0 && m.line_height() < 26.0, "{i}: {m:?}");
        assert!(m.underline_pos > 0.0 && m.underline_pos < m.descent + 1.0, "{i}: {m:?}");
        assert!(m.underline_thickness > 0.3 && m.underline_thickness < 3.0, "{i}: {m:?}");
        let m2 = db.metrics(id, 32.0);
        assert!((m2.ascent - 2.0 * m.ascent).abs() < 1e-3);
    }
    // Liberation Sans is metric-compatible with Arial: ascent 0.905 em, descent 0.212 em.
    let arial = db.resolve("Arial", 400, false);
    let m = db.metrics(arial, 100.0);
    assert!((m.ascent - 90.5).abs() < 0.5, "{m:?}");
    assert!((m.descent - 21.2).abs() < 0.5, "{m:?}");
}

#[test]
fn metrics_bad_inputs() {
    let db = db();
    for size in [0.0, -5.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let m = db.metrics(FontId(0), size);
        assert_eq!(m.ascent, 0.0);
        assert_eq!(db.advance(FontId(0), 'a', size), 0.0);
        assert!(db.outline(FontId(0), 1, size).is_empty());
    }
    let m = db.metrics(FontId(9999), 10.0);
    assert!((m.ascent - 8.0).abs() < 1e-6);
    assert_eq!(db.glyph_id(FontId(9999), 'a'), 0);
    assert_eq!(db.data(FontId(9999)).len(), 0);
}

#[test]
fn advances_and_monospace() {
    let db = db();
    let mono = db.resolve("Courier New", 400, false);
    let w = db.advance(mono, 'i', 10.0);
    // Courier New: 0.6 em for every glyph.
    assert!((w - 6.0).abs() < 0.05, "{w}");
    for c in "MWil.0 ".chars() {
        assert!((db.advance(mono, c, 10.0) - w).abs() < 1e-4, "{c}");
    }
    let sans = db.resolve("Arial", 400, false);
    assert!(db.advance(sans, 'W', 12.0) > db.advance(sans, 'i', 12.0));
    // Arial: space 0.278 em (569/2048).
    assert!((db.advance(sans, ' ', 100.0) - 27.78).abs() < 0.1);
    assert!((db.advance(sans, 'a', 24.0) - 2.0 * db.advance(sans, 'a', 12.0)).abs() < 1e-4);
    let g = db.glyph_id(sans, 'a');
    assert_eq!(db.glyph_advance(sans, g, 12.0), db.advance(sans, 'a', 12.0));
    // Bold is wider than regular.
    let bold = db.resolve("Arial", 700, false);
    assert!(db.measure(bold, "Hello world", 12.0) > db.measure(sans, "Hello world", 12.0));
}

#[test]
fn zero_width_chars() {
    let db = db();
    for c in ['\u{200B}', '\u{200D}', '\u{FEFF}', '\u{00AD}', '\n', '\t', '\0', '\u{FE0F}'] {
        assert_eq!(db.advance(FontId(0), c, 12.0), 0.0, "{:?}", c);
        assert_eq!(db.fallback_for(FontId(0), c), FontId(0));
    }
    assert_eq!(db.measure(FontId(0), "a\u{200B}b", 12.0), db.measure(FontId(0), "ab", 12.0));
}

#[test]
fn measure_sums_and_is_monotonic() {
    let db = db();
    let text = "The quick brown fox — jumps over 13 lazy dogs! Ünïcødé ok.";
    for i in 0..db.faces.len() {
        let id = FontId(i as u16);
        let mut prev = 0.0;
        let mut acc = 0.0;
        for (n, c) in text.char_indices() {
            let w = db.measure(id, text.get(..n + c.len_utf8()).unwrap(), 14.0);
            assert!(w >= prev, "face {i} not monotonic at {n}");
            acc += db.advance(id, c, 14.0);
            assert!((w - acc).abs() < 1e-2);
            prev = w;
        }
        assert_eq!(db.measure(id, "", 14.0), 0.0);
    }
}

#[test]
fn outline_of_a() {
    let db = db();
    for i in 0..db.faces.len() {
        let id = FontId(i as u16);
        let g = db.glyph_id(id, 'A');
        let path = db.outline(id, g, 20.0);
        assert!(path.len() > 3, "face {i}");
        assert!(matches!(path.first(), Some(PathCmd::MoveTo(..))));
        assert!(path.contains(&PathCmd::Close));
        // y-down: the apex of 'A' lies above the baseline, i.e. negative y; nothing far below the baseline.
        let ys: Vec<f32> = path
            .iter()
            .flat_map(|c| match *c {
                PathCmd::MoveTo(_, y) | PathCmd::LineTo(_, y) => vec![y],
                PathCmd::QuadTo(_, y1, _, y) => vec![y1, y],
                PathCmd::CubicTo(_, y1, _, y2, _, y) => vec![y1, y2, y],
                PathCmd::Close => vec![],
            })
            .collect();
        let min = ys.iter().cloned().fold(f32::MAX, f32::min);
        let max = ys.iter().cloned().fold(f32::MIN, f32::max);
        assert!(min < -10.0 && min > -20.0, "face {i}: apex {min}");
        assert!(max > -0.5 && max < 0.5, "face {i}: base {max}");
    }
    // Space and out-of-range glyphs have no outline.
    let sp = db.glyph_id(FontId(0), ' ');
    assert!(db.outline(FontId(0), sp, 20.0).is_empty());
    assert!(db.outline(FontId(0), u32::MAX, 20.0).is_empty());
    assert!(db.outline(FontId(999), 1, 20.0).is_empty());
}

#[test]
fn fallback_to_other_face() {
    let db = db();
    let inter = FontId(0);
    assert_eq!(db.fallback_for(inter, 'a'), inter);
    // Find a char Inter lacks but some other bundled face has.
    let missing = ['\u{05D0}', '\u{0E01}', '\u{2318}', '\u{25CA}', '\u{FB01}', '\u{2591}', '\u{263A}']
        .into_iter()
        .find(|&c| !db.has_glyph(inter, c) && (0..15).any(|i| db.has_glyph(FontId(i), c)));
    let c = missing.expect("some char only in Liberation/Carlito");
    let fb = db.fallback_for(inter, c);
    assert_ne!(fb, inter);
    assert!(db.has_glyph(fb, c));
    let want = db.glyph_advance(fb, db.glyph_id(fb, c), 12.0);
    assert!(want > 0.0);
    assert!((db.advance(inter, c, 12.0) - want).abs() < 1e-4);
    // Bold run prefers a bold fallback face.
    let bold_sans = db.resolve("Arial", 700, false);
    let c2 = ['\u{05D0}', '\u{0E01}', '\u{2318}']
        .into_iter()
        .find(|&c| !db.has_glyph(bold_sans, c) && (0..15).any(|i| db.has_glyph(FontId(i), c)));
    if let Some(c2) = c2 {
        let fb2 = db.fallback_for(bold_sans, c2);
        assert!(db.has_glyph(fb2, c2));
    }
}

#[test]
fn notdef_when_no_face_has_char() {
    let db = db();
    let c = '\u{1F600}';
    assert!((0..15).all(|i| !db.has_glyph(FontId(i), c)));
    assert_eq!(db.fallback_for(FontId(3), c), FontId(3));
    let notdef = db.glyph_advance(FontId(3), 0, 12.0);
    assert!(notdef > 0.0);
    assert_eq!(db.advance(FontId(3), c, 12.0), notdef);
    assert_eq!(db.advance(FontId(3), '中', 12.0), notdef);
}

#[test]
fn cache_tracks_replaced_and_added_faces() {
    let mut db = db();
    let mono_data = db.data(db.resolve("Courier New", 400, false));
    let w_before = db.advance(FontId(0), 'i', 10.0);
    // Replace face 0's data behind the cache's back: the stale entry must not be used.
    if let Some(f) = db.faces.get_mut(0) {
        f.data = mono_data;
    }
    assert!((db.advance(FontId(0), 'i', 10.0) - 6.0).abs() < 0.05);
    assert!((w_before - 6.0).abs() > 0.5);
    // Faces pushed directly (no cache slot) still work.
    db.faces.push(Face { family: "Raw", weight: 400, italic: false, data: mono_data, index: 0 });
    let raw = FontId(15);
    assert!((db.advance(raw, 'W', 10.0) - 6.0).abs() < 0.05);
    assert_eq!(family(&db, db.resolve("raw", 400, false)), "Raw");
    // add_face keeps the cache in step.
    let id =
        db.add_face(Face { family: "Extra", weight: 700, italic: true, data: mono_data, index: 0 }).unwrap();
    assert_eq!(id, FontId(16));
    assert_eq!(db.resolve("'Extra', serif", 400, false), id);
}

#[test]
fn empty_db_is_safe() {
    let db = FontDb::default();
    assert_eq!(db.resolve("Arial", 700, true), FontId(0));
    assert_eq!(db.glyph_id(FontId(0), 'a'), 0);
    assert_eq!(db.advance(FontId(0), 'a', 10.0), 5.0);
    assert_eq!(db.measure(FontId(0), "ab", 10.0), 10.0);
    assert!(db.outline(FontId(0), 0, 10.0).is_empty());
    assert_eq!(db.fallback_for(FontId(0), 'a'), FontId(0));
    assert!(db.families().is_empty());
}

#[test]
fn garbage_face_data_is_safe() {
    let mut db = FontDb::default();
    let id = db
        .add_face(Face { family: "Junk", weight: 400, italic: false, data: b"not a font at all", index: 0 })
        .unwrap();
    let m = db.metrics(id, 10.0);
    assert!((m.ascent - 8.0).abs() < 1e-4);
    assert_eq!(db.glyph_id(id, 'a'), 0);
    assert_eq!(db.advance(id, 'a', 10.0), 5.0);
    assert!(db.outline(id, 0, 10.0).is_empty());
}

#[test]
fn fontdb_is_send_sync_clone() {
    fn check<T: Send + Sync + Clone>() {}
    check::<FontDb>();
    let db = db();
    let _ = db.advance(FontId(3), 'x', 10.0);
    let copy = db.clone();
    assert_eq!(copy.advance(FontId(3), 'x', 10.0), db.advance(FontId(3), 'x', 10.0));
}

#[test]
fn split_families_parsing() {
    use crate::resolve::split_families;
    assert_eq!(split_families("A, 'B C', \"D,E\" , ,f"), vec!["a", "b c", "d,e", "f"]);
    assert_eq!(split_families("'unterminated, x"), vec!["unterminated, x"]);
    assert!(split_families("").is_empty());
    let long = "x,".repeat(10_000);
    assert!(split_families(&long).len() <= 64);
}

#[test]
fn weight_rank_follows_css() {
    use crate::resolve::weight_rank;
    let pick = |want: u16, have: &[u16]| *have.iter().min_by_key(|&&h| weight_rank(want, h)).unwrap();
    assert_eq!(pick(400, &[300, 500, 700]), 500);
    assert_eq!(pick(400, &[300, 600]), 300);
    assert_eq!(pick(500, &[400, 600]), 400);
    assert_eq!(pick(300, &[200, 400]), 200);
    assert_eq!(pick(300, &[400, 500]), 400);
    assert_eq!(pick(700, &[600, 800]), 800);
    assert_eq!(pick(700, &[400, 600]), 600);
}

/// A tiny deterministic generator for byte-mutation fuzzing of real font data.
fn mutate(seed: u64, base: &[u8], edits: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut out = base.to_vec();
    for _ in 0..edits {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        if out.is_empty() {
            break;
        }
        let i = (x as usize) % out.len();
        out[i] = (x >> 32) as u8;
    }
    // Occasionally truncate.
    if seed.is_multiple_of(3) {
        let keep = (x as usize) % (out.len() + 1);
        out.truncate(keep);
    }
    out
}

fn exercise(db: &FontDb, id: FontId) {
    let m = db.metrics(id, 12.0);
    assert!(m.ascent.is_finite() && m.descent.is_finite() && m.line_height().is_finite());
    for c in ['A', 'a', ' ', 'é', '\u{05D0}', '中', '\u{1F600}'] {
        let w = db.advance(id, c, 12.0);
        assert!(w.is_finite() && w >= 0.0);
        let g = db.glyph_id(id, c);
        let _ = db.outline(id, g, 12.0);
        let _ = db.fallback_for(id, c);
    }
    for g in [0, 1, 2, 3, 36, 1000, u32::MAX] {
        let _ = db.outline(id, g, 12.0);
    }
    assert!(db.measure(id, "Hello", 12.0).is_finite());
}

#[test]
fn mutated_fonts_never_panic() {
    let base = FontDb::bundled();
    for seed in 0..60u64 {
        let src = base.data(FontId((seed % 15) as u16));
        // Leak is fine in tests: faces need 'static data.
        let data: &'static [u8] =
            Box::leak(mutate(seed, src, 1 + (seed as usize % 7) * 40).into_boxed_slice());
        let mut db = FontDb::default();
        let id = db.add_face(Face { family: "Fuzz", weight: 400, italic: false, data, index: 0 }).unwrap();
        exercise(&db, id);
    }
}

proptest! {
    #[test]
    fn resolve_never_panics(s in ".{0,200}", w in any::<u16>(), it in any::<bool>()) {
        let db = FontDb::bundled();
        let id = db.resolve(&s, w, it);
        prop_assert!(db.face(id).is_some());
    }

    #[test]
    fn resolve_list_syntax_never_panics(s in "[ ,'\"a-zA-Z()-]{0,120}", w in 0u16..1200) {
        let db = FontDb::bundled();
        prop_assert!(db.face(db.resolve(&s, w, false)).is_some());
    }

    #[test]
    fn measure_never_panics(s in any::<String>(), size in any::<f32>(), face in 0u16..20) {
        let db = FontDb::bundled();
        let w = db.measure(FontId(face), &s, size);
        prop_assert!(w.is_finite() && w >= 0.0);
    }

    #[test]
    fn random_bytes_as_font(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let data: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        let mut db = FontDb::default();
        let id = db.add_face(Face { family: "Rand", weight: 400, italic: false, data, index: 0 }).unwrap();
        exercise(&db, id);
    }

    #[test]
    fn outline_any_glyph(face in 0u16..15, glyph in 0u32..4000, size in 0.0f32..200.0) {
        let db = FontDb::bundled();
        for cmd in db.outline(FontId(face), glyph, size) {
            let ok = match cmd {
                PathCmd::MoveTo(a, b) | PathCmd::LineTo(a, b) => a.is_finite() && b.is_finite(),
                PathCmd::QuadTo(a, b, c, d) => [a, b, c, d].iter().all(|v| v.is_finite()),
                PathCmd::CubicTo(a, b, c, d, e, f) => [a, b, c, d, e, f].iter().all(|v| v.is_finite()),
                PathCmd::Close => true,
            };
            prop_assert!(ok);
        }
    }
}

// ---- Review regressions ----

#[test]
fn bidi_isolates_and_other_format_chars_are_zero_width() {
    let db = db();
    // LRI/RLI/FSI/PDI, Arabic letter mark, deprecated format controls, Mongolian variation selectors, and
    // the interlinear annotation / tag characters: none of the bundled faces map them, and they must not
    // measure as a .notdef box.
    let chars = [
        '\u{2066}',
        '\u{2067}',
        '\u{2068}',
        '\u{2069}',
        '\u{061C}',
        '\u{206A}',
        '\u{206F}',
        '\u{180B}',
        '\u{180F}',
        '\u{FFF9}',
        '\u{FFFB}',
        '\u{E0001}',
        '\u{E0020}',
        '\u{E007F}',
    ];
    for c in chars {
        assert!(is_zero_width(c), "{c:?}");
        for i in 0..15u16 {
            assert_eq!(db.advance(FontId(i), c, 12.0), 0.0, "face {i} {c:?}");
        }
    }
    assert_eq!(db.measure(FontId(3), "\u{2068}abc\u{2069}", 12.0), db.measure(FontId(3), "abc", 12.0));
    // Ordinary characters are not swept up.
    for c in ['a', ' ', '\u{00A0}', '\u{2065}', '\u{FFFC}', '\u{FFFD}', '\u{E0080}'] {
        assert!(!is_zero_width(c), "{c:?}");
    }
}

#[test]
fn custom_family_with_non_ascii_or_odd_spacing_resolves() {
    let mut db = db();
    let data = db.data(FontId(10));
    let ebene = db
        .add_face(Face { family: "\u{c9}b\u{e8}ne  Sans", weight: 400, italic: false, data, index: 0 })
        .unwrap();
    assert_eq!(db.resolve("\u{e9}b\u{e8}ne sans", 400, false), ebene);
    assert_eq!(db.resolve("'\u{c9}B\u{c8}NE   SANS', serif", 400, false), ebene);
    assert_eq!(db.resolve("\u{c9}b\u{e8}ne Sans Bold", 400, false), ebene);
    assert_eq!(
        resolve::match_family("\u{e9}b\u{e8}ne sans", &db.families()).map(|m| m.family),
        Some("\u{c9}b\u{e8}ne  Sans")
    );
}

#[test]
fn split_style_words_are_recognised() {
    let db = db();
    assert_eq!(style(&db, db.resolve("Segoe UI Semi Bold", 400, false)), ("Inter", 600, false));
    assert_eq!(style(&db, db.resolve("Segoe UI Demi Bold", 400, false)), ("Inter", 600, false));
    assert_eq!(style(&db, db.resolve("Arial Extra Bold", 400, false)), ("Liberation Sans", 700, false));
    assert_eq!(style(&db, db.resolve("Calibri Semi Light", 400, false)), ("Carlito", 400, false));
    assert_eq!(style(&db, db.resolve("Aptos Extra Bold Italic", 400, false)), ("Inter", 600, false));
    assert_eq!(
        style(&db, db.resolve("Times New Roman Semi Bold Italic", 400, false)),
        ("Liberation Serif", 400, true)
    );
    // A bare modifier is not a style word.
    assert_eq!(style(&db, db.resolve("Arial Semi, Consolas", 400, false)), ("Liberation Mono", 400, false));
}

#[test]
fn css_escapes_in_family_lists() {
    use crate::resolve::split_families;
    // An escaped quote inside a quoted name must not end the string and swallow the rest of the list.
    assert_eq!(split_families("\"Foo\\\"Bar\", Arial"), vec!["foo\"bar", "arial"]);
    assert_eq!(family(&db(), db().resolve("\"Foo\\\"Bar\", Arial", 400, false)), "Liberation Sans");
    // Escaped comma and space in an unquoted name.
    assert_eq!(split_families("Times\\ New\\ Roman, x\\,y"), vec!["times new roman", "x,y"]);
    assert_eq!(family(&db(), db().resolve("Times\\ New\\ Roman", 400, false)), "Liberation Serif");
    // Hex escapes (with the optional trailing space consumed) decode to their code points.
    assert_eq!(split_families("\\41 rial"), vec!["arial"]);
    assert_eq!(
        split_families("\\5FAE\\8F6F\\96C5\\9ED1, Arial"),
        vec!["\u{5fae}\u{8f6f}\u{96c5}\u{9ed1}", "arial"]
    );
    // Invalid hex escapes become U+FFFD; a trailing lone backslash is dropped.
    assert_eq!(split_families("\\0 x"), vec!["\u{fffd}x"]);
    assert_eq!(split_families("\\D800 x"), vec!["\u{fffd}x"]);
    assert_eq!(split_families("\\110000 x"), vec!["\u{fffd}x"]);
    assert_eq!(split_families("arial\\"), vec!["arial"]);
}

proptest! {
    #[test]
    fn escaped_family_lists_never_panic(s in r#"[\\ ,'"a-fA-F0-9xyz\n\t()-]{0,160}"#, w in any::<u16>()) {
        let db = FontDb::bundled();
        let names = crate::resolve::split_families(&s);
        prop_assert!(names.len() <= 64);
        for n in &names {
            prop_assert!(!n.is_empty() && n.chars().count() <= 128);
            let _ = crate::resolve::match_family(n, &db.families());
        }
        prop_assert!(db.face(db.resolve(&s, w, true)).is_some());
    }

    #[test]
    fn same_family_is_symmetric(a in ".{0,40}", b in ".{0,40}") {
        prop_assert_eq!(crate::resolve::same_family(&a, &b), crate::resolve::same_family(&b, &a));
        prop_assert!(crate::resolve::same_family(&a, &a));
    }
}

fn asset(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts").join(name)
}

/// An index where "Zed Sans" is installed (backed by bundled files so the test needs no system fonts).
fn zed_index() -> std::sync::Arc<system::SystemFonts> {
    use system::SystemFace;
    let face = |weight, italic, file: &str| SystemFace {
        family: "Zed Sans".into(),
        weight,
        italic,
        path: asset(file),
        index: 0,
    };
    std::sync::Arc::new(system::SystemFonts::from_faces(vec![
        face(400, false, "LiberationMono-Regular.ttf"),
        face(700, false, "LiberationMono-Bold.ttf"),
        SystemFace { family: "zed  sans".into(), ..face(400, false, "LiberationMono-Regular.ttf") },
        SystemFace { family: ".Hidden".into(), ..face(400, false, "Inter-Regular.ttf") },
        SystemFace { family: "Gone Font".into(), ..face(400, false, "missing.ttf") },
        SystemFace { family: "Arial".into(), ..face(400, false, "LiberationSerif-Regular.ttf") },
    ]))
}

#[test]
fn installed_fonts_are_loaded_on_first_use_and_drawn_in_their_own_face() {
    let sys = zed_index();
    assert_eq!(sys.families(), &["Arial", "Gone Font", "Zed Sans"]);
    assert!(sys.has_family("ZED SANS") && !sys.has_family("Hidden"));
    let db = FontDb::bundled().with_system(sys);
    let bundled_faces = db.faces.len();
    assert!(!db.families().contains(&"Zed Sans"), "nothing is loaded before a document asks");
    let id = db.resolve("'Zed Sans', sans-serif", 400, false);
    assert!(usize::from(id.0) >= bundled_faces);
    assert_eq!(family(&db, id), "Zed Sans");
    // Liberation Mono metrics: every glyph is 0.6 em.
    assert!((db.advance(id, 'i', 10.0) - 6.0).abs() < 0.05);
    assert!(!db.outline(id, db.glyph_id(id, 'a'), 20.0).is_empty());
    let bold = db.resolve("Zed Sans", 700, false);
    assert_eq!(style(&db, bold), ("Zed Sans", 700, false));
    // Style words still apply to installed families.
    assert_eq!(db.resolve("Zed Sans Bold", 400, false), bold);
    // An installed font beats the bundled stand-in for its name.
    assert_eq!(family(&db, db.resolve("Arial", 400, false)), "Arial");
    // Unreadable files fall back to the stand-in rules; unknown names to Inter.
    assert_eq!(family(&db, db.resolve("Gone Font, serif", 400, false)), "Liberation Serif");
    assert_eq!(family(&db, db.resolve("Nope", 400, false)), "Inter");
    assert!(db.has_own_face("zed sans") && db.has_own_face("Inter") && !db.has_own_face("Calibri"));
    // A clone keeps the loaded faces under the same ids.
    let copy = db.clone();
    assert_eq!(copy.resolve("Zed Sans", 400, false), id);
    assert_eq!(copy.data(id).len(), db.data(id).len());
    assert_eq!(db.fallback_for(id, 'a'), id);
}

#[test]
fn bundled_db_ignores_installed_fonts() {
    let db = FontDb::bundled();
    assert!(db.system_fonts().is_none());
    assert_eq!(family(&db, db.resolve("Zed Sans", 400, false)), "Inter");
    assert!(!db.has_own_face("Zed Sans"));
}

#[test]
fn scanning_a_font_folder_indexes_faces() {
    let dir = std::env::temp_dir().join(format!("mailcraft-fonts-scan-{}", std::process::id()));
    let sub = dir.join("a").join("b");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::copy(asset("Inter-SemiBold.ttf"), sub.join("Inter-SemiBold.ttf")).unwrap();
    std::fs::copy(asset("LiberationSerif-Italic.ttf"), dir.join("serif.TTF")).unwrap();
    std::fs::write(dir.join("broken.otf"), b"not a font").unwrap();
    std::fs::write(dir.join("readme.txt"), b"hello").unwrap();
    // A collection header that claims more faces than it holds.
    let mut ttc = b"ttcf\0\x01\0\0\xff\xff\xff\xff".to_vec();
    ttc.extend_from_slice(&[0u8; 64]);
    std::fs::write(dir.join("junk.ttc"), ttc).unwrap();
    let files = system::font_files(std::slice::from_ref(&dir));
    assert_eq!(files.len(), 4);
    let sys = system::scan(std::slice::from_ref(&dir));
    assert_eq!(sys.families(), &["Inter", "Liberation Serif"]);
    let serif = sys.faces().iter().find(|f| f.family == "Liberation Serif").unwrap();
    assert!(serif.italic && serif.weight == 400);
    let inter = sys.faces().iter().find(|f| f.family == "Inter").unwrap();
    assert_eq!((inter.weight, inter.italic), (600, false));
    assert!(system::scan(&[dir.join("missing")]).families().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn faces_in_rejects_junk() {
    let p = std::path::Path::new("x.ttf");
    assert!(system::faces_in(b"", p).is_empty());
    assert!(system::faces_in(b"ttcf", p).is_empty());
    assert!(system::faces_in(&[0u8; 300], p).is_empty());
    let inter = std::fs::read(asset("Inter-Regular.ttf")).unwrap();
    let faces = system::faces_in(&inter, p);
    assert_eq!(faces.len(), 1);
    assert_eq!(faces[0].family, "Inter");
}

/// Regression: one message naming hundreds of installed families loads at most `MAX_LOADED_FAMILIES` of
/// them whole; the rest are drawn with their stand-ins (the data stays bounded).
#[test]
fn installed_families_loaded_by_one_body_are_capped() {
    use system::{MAX_LOADED_FAMILIES, SystemFace};
    let n = MAX_LOADED_FAMILIES + 20;
    let faces: Vec<SystemFace> = (0..n)
        .flat_map(|i| {
            [(400, "LiberationMono-Regular.ttf"), (700, "LiberationMono-Bold.ttf")].map(|(weight, file)| {
                SystemFace {
                    family: format!("Zed {i:03}"),
                    weight,
                    italic: false,
                    path: asset(file),
                    index: 0,
                }
            })
        })
        .collect();
    let db = FontDb::bundled().with_system(std::sync::Arc::new(system::SystemFonts::from_faces(faces)));
    // Every family named in one font-family list, as a hostile message body would.
    let list: Vec<String> = (0..n).map(|i| format!("'Zed {i:03}'")).collect();
    for name in &list {
        let _ = db.resolve(name, 700, false);
    }
    let _ = db.resolve(&list.join(", "), 400, false);
    let installed = db.families().iter().filter(|f| f.starts_with("Zed ")).count();
    assert_eq!(installed, MAX_LOADED_FAMILIES);
    assert_eq!(family(&db, db.resolve("Zed 000", 700, false)), "Zed 000");
    assert_eq!(family(&db, db.resolve(&format!("Zed {:03}", n - 1), 400, false)), "Inter", "past the cap");
}

/// Font list previews load only the regular face of a family; text in the family then loads the rest.
#[test]
fn previews_load_only_the_regular_face() {
    let sys = zed_index();
    let db = FontDb::bundled().with_system(std::sync::Arc::clone(&sys));
    let bundled = db.faces.len();
    let loaded = |db: &FontDb| {
        (bundled..usize::from(u16::MAX)).take_while(|i| db.face(FontId(*i as u16)).is_some()).count()
    };
    let id = db.resolve_regular("Zed Sans");
    assert_eq!(style(&db, id), ("Zed Sans", 400, false));
    assert!(!sys.is_loaded("Zed Sans"), "the family is not loaded whole for a preview");
    assert_eq!(loaded(&db), 1);
    // Bold text in the family loads the bold face (the regular one keeps its id).
    let bold = db.resolve("Zed Sans", 700, false);
    assert_eq!(style(&db, bold), ("Zed Sans", 700, false));
    assert_eq!(db.resolve("Zed Sans", 400, false), id);
    assert_eq!(loaded(&db), 2);
    assert!(sys.is_loaded("Zed Sans"));
    // A family loaded whole previews in its regular face without loading anything else.
    assert_eq!(db.resolve_regular("Zed Sans"), id);
    assert_eq!(family(&db, db.resolve_regular("Nope")), "Inter");
}

/// A copy of a bundled font file with its OS/2 `fsType` set.
fn with_fs_type(file: &str, fs_type: u16) -> &'static [u8] {
    let mut data = std::fs::read(asset(file)).unwrap();
    let tables = usize::from(u16::from_be_bytes([data[4], data[5]]));
    let rec = (0..tables).map(|i| 12 + 16 * i).find(|&r| &data[r..r + 4] == b"OS/2").unwrap();
    let off = u32::from_be_bytes(data[rec + 8..rec + 12].try_into().unwrap()) as usize;
    data[off + 8..off + 10].copy_from_slice(&fs_type.to_be_bytes());
    Box::leak(data.into_boxed_slice())
}

#[test]
fn restricted_faces_are_not_embeddable_and_have_a_bundled_stand_in() {
    let mut db = FontDb::bundled();
    let face = |family, weight, fs_type| Face {
        family,
        weight,
        italic: false,
        data: with_fs_type(
            if weight == 700 { "LiberationSerif-Bold.ttf" } else { "LiberationSerif-Regular.ttf" },
            fs_type,
        ),
        index: 0,
    };
    let open = db.add_face(face("Georgia", 400, 0x0008)).unwrap();
    let locked = db.add_face(face("Cambria", 700, 0x0002)).unwrap();
    let odd = db.add_face(face("Nameless", 400, 0x0200)).unwrap();
    assert!(db.embeddable(open) && db.embeddable_face(open) == open);
    assert!(!db.embeddable(locked) && !db.embeddable(odd));
    let sub = db.embeddable_face(locked);
    assert_eq!(style(&db, sub), ("Liberation Serif", 700, false), "the serif stand-in, same weight");
    assert_eq!(family(&db, db.embeddable_face(odd)), "Inter");
    assert!(db.embeddable(db.resolve("Inter", 400, false)));
    assert!(db.embeddable(FontId(60_000)));
}
