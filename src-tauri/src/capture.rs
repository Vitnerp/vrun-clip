//! Turns raw clipboard captures into stored history items.
//!
//! This runs on a worker thread, never on the clipboard listener thread, so
//! heavy work (decoding a 4K bitmap, PNG encoding, thumbnails) never keeps
//! the system clipboard open or blocks other applications.

use crate::db::{InsertOutcome, NewItem, Store};
use crate::model::*;
use crate::settings::Settings;
use anyhow::{bail, Context, Result};
use image::{codecs::png, ImageEncoder, RgbaImage};
use std::sync::Mutex;

const PREVIEW_CHARS: usize = 300;
const THUMB_W: u32 = 320;
const THUMB_H: u32 = 200;

/// Stores a captured clip. Returns the item id and whether it is new.
pub fn process(store: &Mutex<Store>, settings: &Settings, clip: CapturedClip) -> Result<Option<(i64, bool)>> {
    let max_bytes = settings.max_item_mb as usize * 1024 * 1024;
    let source_app = clip.source_app.clone();

    let item = if let Some(files) = clip.files.filter(|f| !f.is_empty()) {
        let text = files.join("\n");
        let names: Vec<&str> = files
            .iter()
            .map(|p| p.rsplit(['\\', '/']).next().unwrap_or(p))
            .collect();
        let preview = if files.len() == 1 {
            names[0].to_string()
        } else {
            format!("{} files: {}", files.len(), names.join(", "))
        };
        NewItem {
            kind: Kind::Files,
            hash: hash(&[b"files", text.as_bytes()]),
            size: text.len() as i64,
            preview: truncate_chars(&preview, PREVIEW_CHARS),
            text,
            source_app,
            image_w: None,
            image_h: None,
            html: None,
            rtf: None,
            files: Some(files),
        }
    } else if let Some(text) = clip.text.filter(|t| !t.is_empty()) {
        let size = text.len() + clip.html.as_ref().map_or(0, Vec::len) + clip.rtf.as_ref().map_or(0, Vec::len);
        if size > max_bytes {
            log::info!("skipping text clip of {size} bytes (limit {max_bytes})");
            return Ok(None);
        }
        NewItem {
            kind: Kind::Text,
            hash: hash(&[b"text", text.as_bytes()]),
            preview: make_preview(&text),
            text,
            source_app,
            size: size as i64,
            image_w: None,
            image_h: None,
            html: clip.html,
            rtf: clip.rtf,
            files: None,
        }
    } else if let Some(raw) = clip.image {
        return process_image(store, settings, raw, source_app);
    } else {
        return Ok(None);
    };

    let uuid = uuid::Uuid::new_v4().to_string();
    let mut store = store.lock().unwrap();
    Ok(Some(match store.insert(&uuid, item)? {
        InsertOutcome::New(id) => (id, true),
        InsertOutcome::Existing(id) => (id, false),
    }))
}

fn process_image(
    store: &Mutex<Store>,
    settings: &Settings,
    raw: RawImage,
    source_app: Option<String>,
) -> Result<Option<(i64, bool)>> {
    let (rgba, original_png) = match raw {
        RawImage::Png(bytes) => {
            let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                .context("decode PNG from clipboard")?
                .to_rgba8();
            (img, Some(bytes))
        }
        RawImage::Dib(bytes) => (dib_to_rgba(&bytes)?, None),
        RawImage::Tiff(bytes) => {
            let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Tiff)
                .context("decode TIFF from clipboard")?
                .to_rgba8();
            (img, None)
        }
    };
    let (w, h) = rgba.dimensions();
    let hash = hash(&[b"image", &w.to_le_bytes(), &h.to_le_bytes(), rgba.as_raw()]);

    // Same picture again: just move it to the top, skip all encoding work.
    {
        let s = store.lock().unwrap();
        if let Some(id) = s.find_hash(&hash)? {
            s.touch(id)?;
            return Ok(Some((id, false)));
        }
    }

    let png_bytes = match original_png {
        Some(b) => b,
        None => encode_png(&rgba)?,
    };
    if png_bytes.len() > settings.max_item_mb as usize * 1024 * 1024 {
        log::info!("skipping image of {} bytes", png_bytes.len());
        return Ok(None);
    }
    let thumb = image::imageops::thumbnail(&rgba, fit(w, h).0, fit(w, h).1);
    let thumb_bytes = encode_png(&thumb)?;

    let uuid = uuid::Uuid::new_v4().to_string();
    let (image_path, thumb_path) = {
        let s = store.lock().unwrap();
        (s.image_path(&uuid), s.thumb_path(&uuid))
    };
    std::fs::write(&image_path, &png_bytes)?;
    std::fs::write(&thumb_path, &thumb_bytes)?;

    let item = NewItem {
        kind: Kind::Image,
        hash,
        text: String::new(),
        preview: format!("Image {w}×{h}"),
        source_app,
        size: png_bytes.len() as i64,
        image_w: Some(w as i64),
        image_h: Some(h as i64),
        html: None,
        rtf: None,
        files: None,
    };
    let mut s = store.lock().unwrap();
    Ok(Some(match s.insert(&uuid, item)? {
        InsertOutcome::New(id) => (id, true),
        InsertOutcome::Existing(id) => {
            // Lost a race with an identical capture; drop our files.
            let _ = std::fs::remove_file(image_path);
            let _ = std::fs::remove_file(thumb_path);
            (id, false)
        }
    }))
}

fn fit(w: u32, h: u32) -> (u32, u32) {
    if w <= THUMB_W && h <= THUMB_H {
        return (w.max(1), h.max(1));
    }
    let scale = (THUMB_W as f64 / w as f64).min(THUMB_H as f64 / h as f64);
    (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1))
}

pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(img.as_raw().len() / 4);
    png::PngEncoder::new_with_quality(&mut out, png::CompressionType::Fast, png::FilterType::Adaptive)
        .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgba8)?;
    Ok(out)
}

fn hash(parts: &[&[u8]]) -> String {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p);
    }
    h.finalize().to_hex().to_string()
}

fn truncate_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// One line preview: whitespace collapsed, limited length.
fn make_preview(text: &str) -> String {
    let mut out = String::with_capacity(PREVIEW_CHARS + 4);
    let mut count = 0;
    for word in text.split_whitespace() {
        if count > 0 {
            out.push(' ');
            count += 1;
        }
        for c in word.chars() {
            if count >= PREVIEW_CHARS {
                out.push('…');
                return out;
            }
            out.push(c);
            count += 1;
        }
    }
    out
}

fn rd_u16(b: &[u8], o: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(b.get(o..o + 2).context("DIB too short")?.try_into()?))
}
fn rd_u32(b: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(b.get(o..o + 4).context("DIB too short")?.try_into()?))
}

/// Converts a Windows DIB (CF_DIB / CF_DIBV5 payload) to RGBA.
/// Supports 24 bpp and 32 bpp (BI_RGB and BI_BITFIELDS), which covers
/// screenshots, browsers, Office and the Snipping Tool.
pub fn dib_to_rgba(b: &[u8]) -> Result<RgbaImage> {
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    let header_size = rd_u32(b, 0)? as usize;
    let width = rd_u32(b, 4)? as i32;
    let height = rd_u32(b, 8)? as i32;
    let bpp = rd_u16(b, 14)? as u32;
    let compression = rd_u32(b, 16)?;
    let clr_used = rd_u32(b, 32)? as usize;
    if width <= 0 || height == 0 || width > 32768 || height.abs() > 32768 {
        bail!("unsupported DIB size {width}x{height}");
    }
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    let bottom_up = height > 0;

    let mut offset = header_size;
    let (mut rm, mut gm, mut bm, mut am) = (0x00FF0000u32, 0x0000FF00u32, 0x000000FFu32, 0u32);
    if compression == BI_BITFIELDS {
        rm = rd_u32(b, 40)?;
        gm = rd_u32(b, 44)?;
        bm = rd_u32(b, 48)?;
        if header_size >= 56 {
            am = rd_u32(b, 52)?;
        }
        if header_size == 40 {
            offset += 12; // masks follow a plain BITMAPINFOHEADER
        }
    } else if compression != BI_RGB {
        bail!("unsupported DIB compression {compression}");
    }
    if bpp <= 8 {
        bail!("palette DIBs are not supported");
    }
    offset += clr_used * 4;

    let stride = (w * bpp as usize).div_ceil(32) * 4;
    let pixels = b.get(offset..).context("DIB has no pixel data")?;
    if pixels.len() < stride * h {
        bail!("DIB pixel data truncated");
    }

    let mut out = vec![0u8; w * h * 4];
    match bpp {
        24 => {
            for y in 0..h {
                let src_row = if bottom_up { h - 1 - y } else { y };
                let row = &pixels[src_row * stride..];
                for x in 0..w {
                    let s = &row[x * 3..x * 3 + 3];
                    out[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[s[2], s[1], s[0], 255]);
                }
            }
        }
        32 => {
            let (rs, gs, bs, as_) = (shift(rm), shift(gm), shift(bm), shift(am));
            // With BI_RGB the 4th byte is officially unused; many apps leave
            // it 0. Treat all-zero alpha as fully opaque.
            let use_alpha = if compression == BI_RGB {
                am = 0xFF000000;
                (0..h).any(|y| (0..w).any(|x| pixels[y * stride + x * 4 + 3] != 0))
            } else {
                am != 0
            };
            let as_ = if compression == BI_RGB { shift(am) } else { as_ };
            for y in 0..h {
                let src_row = if bottom_up { h - 1 - y } else { y };
                let row = &pixels[src_row * stride..];
                for x in 0..w {
                    let v = u32::from_le_bytes(row[x * 4..x * 4 + 4].try_into().unwrap());
                    let a = if use_alpha { channel(v, am, as_) } else { 255 };
                    let d = (y * w + x) * 4;
                    out[d] = channel(v, rm, rs);
                    out[d + 1] = channel(v, gm, gs);
                    out[d + 2] = channel(v, bm, bs);
                    out[d + 3] = a;
                }
            }
        }
        _ => bail!("unsupported DIB bit depth {bpp}"),
    }
    RgbaImage::from_raw(w as u32, h as u32, out).context("bad image buffer")
}

fn shift(mask: u32) -> u32 {
    if mask == 0 { 0 } else { mask.trailing_zeros() }
}

fn channel(v: u32, mask: u32, shift: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let max = mask >> shift;
    let val = (v & mask) >> shift;
    if max == 255 { val as u8 } else { ((val * 255 + max / 2) / max) as u8 }
}

/// Builds a bottom-up 32 bpp BI_RGB DIB (CF_DIB) from RGBA pixels.
pub fn rgba_to_dib(img: &RgbaImage) -> Vec<u8> {
    let (w, h) = img.dimensions();
    let mut out = Vec::with_capacity(40 + (w * h * 4) as usize);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes()); // positive = bottom-up
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bpp
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(w * h * 4).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // resolution, colors
    let raw = img.as_raw();
    for y in (0..h as usize).rev() {
        for px in raw[y * w as usize * 4..(y + 1) * w as usize * 4].as_chunks::<4>().0 {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_roundtrip_32bpp() {
        let mut img = RgbaImage::new(3, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(2, 1, image::Rgba([0, 0, 255, 128]));
        let back = dib_to_rgba(&rgba_to_dib(&img)).unwrap();
        assert_eq!(back.dimensions(), (3, 2));
        assert_eq!(back.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(back.get_pixel(2, 1).0, [0, 0, 255, 128]);
    }

    #[test]
    fn dib_zero_alpha_is_opaque() {
        let mut dib = rgba_to_dib(&RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 0])));
        let n = dib.len();
        dib[n - 1] = 0;
        let back = dib_to_rgba(&dib).unwrap();
        assert_eq!(back.get_pixel(1, 1).0, [10, 20, 30, 255]);
    }

    #[test]
    fn dib_24bpp_padded_rows() {
        // 1x2 image, 24 bpp: stride padded to 4 bytes. Bottom-up.
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&2i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&24u16.to_le_bytes());
        dib.extend_from_slice(&[0u8; 24]);
        dib.extend_from_slice(&[1, 2, 3, 0]); // bottom row: B G R pad
        dib.extend_from_slice(&[4, 5, 6, 0]); // top row
        let img = dib_to_rgba(&dib).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [6, 5, 4, 255]);
        assert_eq!(img.get_pixel(0, 1).0, [3, 2, 1, 255]);
    }

    #[test]
    fn preview_collapses_whitespace() {
        assert_eq!(make_preview("  a\n\n b\tc  "), "a b c");
        assert!(make_preview(&"x".repeat(1000)).ends_with('…'));
    }

    #[test]
    fn process_text_and_dedupe() {
        let store = Mutex::new(Store::open_in_memory(&std::env::temp_dir()).unwrap());
        let s = Settings::default();
        let clip = || CapturedClip { text: Some("hello".into()), ..Default::default() };
        let (id1, new1) = process(&store, &s, clip()).unwrap().unwrap();
        let (id2, new2) = process(&store, &s, clip()).unwrap().unwrap();
        assert!(new1 && !new2);
        assert_eq!(id1, id2);
    }
}
