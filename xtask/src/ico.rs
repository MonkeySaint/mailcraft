//! `cargo xtask ico`: rasterises `assets/app-icon/mailcraft.svg` (resvg) into the files desktops and
//! installers want: freedesktop hicolor PNGs + scalable SVG, a 1024 px PNG, a Windows ICO and a macOS ICNS.
//! Both containers are written here (PNG-compressed entries). `MAILCRAFT_ICO_OUT=DIR` writes into DIR instead
//! of `assets/app-icon/`.

use std::path::{Path, PathBuf};

use crate::{TaskResult, rel};

pub const SOURCE: &str = "mailcraft.svg";
/// hicolor PNG sizes.
pub const HICOLOR: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];
/// ICO entry sizes.
pub const ICO: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
/// ICNS entries: (type, size). ic07–ic10 hold PNG data for 128, 256, 512 and 1024 (512@2x).
pub const ICNS: [(&[u8; 4], u32); 4] = [(b"ic07", 128), (b"ic08", 256), (b"ic09", 512), (b"ic10", 1024)];

/// Every file `ico` writes into `dir`.
pub fn outputs(dir: &Path) -> Vec<PathBuf> {
    let hicolor = dir.join("hicolor");
    let mut out: Vec<PathBuf> =
        HICOLOR.iter().map(|s| hicolor.join(format!("{s}x{s}")).join("apps").join("mailcraft.png")).collect();
    out.push(hicolor.join("scalable").join("apps").join("mailcraft.svg"));
    out.push(dir.join("mailcraft-1024.png"));
    out.push(dir.join("mailcraft.ico"));
    out.push(dir.join("mailcraft.icns"));
    out
}

/// Renders the SVG at `size`×`size` and encodes it as PNG.
pub fn render_png(svg: &[u8], size: u32) -> Result<Vec<u8>, String> {
    use resvg::{tiny_skia, usvg};
    let tree =
        usvg::Tree::from_data(svg, &usvg::Options::default()).map_err(|e| format!("invalid SVG: {e}"))?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or_else(|| format!("cannot allocate {size}px"))?;
    let s = tree.size();
    let scale = size as f32 / s.width().max(s.height()).max(1.0);
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|e| format!("cannot encode PNG: {e}"))
}

/// ICO container (ICONDIR + ICONDIRENTRY × n + PNG data). Entries: (size, png).
pub fn ico(entries: &[(u32, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let count = u16::try_from(entries.len()).map_err(|_| "too many ICO entries")?;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&count.to_le_bytes());
    let mut offset = 6usize.saturating_add(16usize.saturating_mul(entries.len()));
    for (size, png) in entries {
        if *size == 0 || *size > 256 {
            return Err(format!("ICO entries must be 1–256 px, not {size}"));
        }
        // 256 is stored as 0.
        let dim = u8::try_from(*size % 256).map_err(|_| "bad size")?;
        out.extend_from_slice(&[dim, dim, 0, 0]); // width, height, palette colours, reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&u32::try_from(png.len()).map_err(|_| "PNG too large")?.to_le_bytes());
        out.extend_from_slice(&u32::try_from(offset).map_err(|_| "ICO too large")?.to_le_bytes());
        offset = offset.saturating_add(png.len());
    }
    for (_, png) in entries {
        out.extend_from_slice(png);
    }
    Ok(out)
}

/// ICNS container (`icns` + length, then type + length + data per entry; big-endian).
pub fn icns(entries: &[(&[u8; 4], Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    for (kind, png) in entries {
        let len = u32::try_from(png.len().saturating_add(8)).map_err(|_| "ICNS entry too large")?;
        body.extend_from_slice(*kind);
        body.extend_from_slice(&len.to_be_bytes());
        body.extend_from_slice(png);
    }
    let total = u32::try_from(body.len().saturating_add(8)).map_err(|_| "ICNS too large")?;
    let mut out = Vec::with_capacity(body.len().saturating_add(8));
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, data).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Writes every output for `svg` into `dir`.
pub fn generate(svg: &[u8], dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut written = Vec::new();
    let mut cache: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut png = |size: u32| -> Result<Vec<u8>, String> {
        if let Some((_, p)) = cache.iter().find(|(s, _)| *s == size) {
            return Ok(p.clone());
        }
        let p = render_png(svg, size)?;
        cache.push((size, p.clone()));
        Ok(p)
    };
    let hicolor = dir.join("hicolor");
    for size in HICOLOR {
        let path = hicolor.join(format!("{size}x{size}")).join("apps").join("mailcraft.png");
        write(&path, &png(size)?)?;
        written.push(path);
    }
    let scalable = hicolor.join("scalable").join("apps").join("mailcraft.svg");
    write(&scalable, svg)?;
    written.push(scalable);
    let big = dir.join("mailcraft-1024.png");
    write(&big, &png(1024)?)?;
    written.push(big);
    let ico_entries = ICO.iter().map(|s| Ok((*s, png(*s)?))).collect::<Result<Vec<_>, String>>()?;
    let ico_path = dir.join("mailcraft.ico");
    write(&ico_path, &ico(&ico_entries)?)?;
    written.push(ico_path);
    let icns_entries = ICNS.iter().map(|(k, s)| Ok((*k, png(*s)?))).collect::<Result<Vec<_>, String>>()?;
    let icns_path = dir.join("mailcraft.icns");
    write(&icns_path, &icns(&icns_entries)?)?;
    written.push(icns_path);
    Ok(written)
}

pub fn run(root: &Path) -> TaskResult {
    let icon_dir = root.join("assets").join("app-icon");
    let source = icon_dir.join(SOURCE);
    let svg = std::fs::read(&source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    let out_dir = match std::env::var_os("MAILCRAFT_ICO_OUT") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => icon_dir,
    };
    let written = generate(&svg, &out_dir)?;
    for w in &written {
        println!("  wrote {}", rel(root, w));
    }
    println!("ico: {} files from {}", written.len(), rel(root, &source));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="12" fill="#0E7C86"/></svg>"##;

    #[test]
    fn renders_pngs() {
        let png = render_png(SVG, 16).unwrap();
        assert_eq!(png.get(..8), Some(&b"\x89PNG\r\n\x1a\n"[..]));
        // IHDR width/height.
        assert_eq!(png.get(16..24), Some(&[0, 0, 0, 16, 0, 0, 0, 16][..]));
        assert!(render_png(b"<nope", 16).is_err());
    }

    #[test]
    fn ico_layout() {
        let data = ico(&[(16, vec![1, 2, 3]), (256, vec![4, 5])]).unwrap();
        assert_eq!(data.get(..6), Some(&[0, 0, 1, 0, 2, 0][..]));
        // Entry 1: 16×16, 3 bytes at offset 6 + 32 = 38.
        assert_eq!(data.get(6..8), Some(&[16, 16][..]));
        assert_eq!(data.get(14..22), Some(&[3, 0, 0, 0, 38, 0, 0, 0][..]));
        // Entry 2: 256 stored as 0, 2 bytes at offset 41.
        assert_eq!(data.get(22..24), Some(&[0, 0][..]));
        assert_eq!(data.get(30..38), Some(&[2, 0, 0, 0, 41, 0, 0, 0][..]));
        assert_eq!(data.get(38..), Some(&[1, 2, 3, 4, 5][..]));
        assert!(ico(&[(512, vec![])]).is_err());
    }

    #[test]
    fn icns_layout() {
        let data = icns(&[(b"ic07", vec![9, 9])]).unwrap();
        assert_eq!(data, b"icns\x00\x00\x00\x12ic07\x00\x00\x00\x0a\x09\x09".to_vec());
    }

    #[test]
    fn generates_every_output() {
        let dir = tempfile::tempdir().unwrap();
        let written = generate(SVG, dir.path()).unwrap();
        assert_eq!(written, outputs(dir.path()));
        for w in &written {
            assert!(w.is_file(), "{}", w.display());
        }
        let icns_data = std::fs::read(dir.path().join("mailcraft.icns")).unwrap();
        let len = u32::from_be_bytes(icns_data[4..8].try_into().unwrap()) as usize;
        assert_eq!(len, icns_data.len());
    }
}
