//! PDFs and Office documents (Word, PowerPoint, Excel, OpenDocument). They're usually big
//! because of the photos inside, so those are resized and recompressed; the result stays a
//! normal document that opens the same way. Steps down until it fits.
//!
//! PDF: only plain 8-bit color or gray photos are touched (masks, CMYK, palettes and the
//! like are left alone), and a picture is only replaced when the new one is smaller.
//! Office files are zips: JPEG pictures inside are recompressed and everything is repacked
//! at maximum compression.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use lopdf::{Document, Object};

use super::{file_size, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, Progress};

/// (longest side in px, JPEG `-q:v`), best-looking first.
const STEPS: [(u32, u32); 5] = [(2000, 4), (1600, 7), (1280, 10), (1024, 14), (800, 18)];
/// Pictures smaller than this aren't worth touching.
const MIN_BYTES: usize = 24 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Doc {
    Pdf,
    Office,
}

/// Documents we know how to shrink, by extension.
pub(crate) fn kind(path: &Path) -> Option<Doc> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => Some(Doc::Pdf),
        "docx" | "pptx" | "xlsx" | "odt" | "odp" | "ods" => Some(Doc::Office),
        _ => None,
    }
}

/// Shrink the document's pictures until it fits `target`.
pub(crate) fn fit(
    ctx: &Ctx,
    path: &Path,
    doc: Doc,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| "pdf".into());
    let out = with_ext(out_base, &ext);
    let tmp = tempfile::tempdir()?;
    let mut smallest = u64::MAX;
    for (i, &(side, q)) in STEPS.iter().enumerate() {
        match doc {
            Doc::Pdf => shrink_pdf(ctx, path, &out, tmp.path(), side, q)?,
            Doc::Office => shrink_office(ctx, path, &out, tmp.path(), side, q)?,
        }
        progress((i + 1) as f32 / STEPS.len() as f32);
        let size = file_size(&out)?;
        if size <= target {
            return Ok(out);
        }
        smallest = smallest.min(size);
    }
    let _ = std::fs::remove_file(&out);
    Err(Error::DoesNotFit {
        what: name_of(path),
        target,
        best: smallest,
    })
}

/// Re-encode one picture as JPEG with ffmpeg, no bigger than `side` on its longest side.
/// `input` is a file ffmpeg can read (JPEG, or PPM/PGM for raw pixels).
fn jpeg(
    ctx: &Ctx,
    input: &Path,
    out: &Path,
    size: (u32, u32),
    gray: bool,
    side: u32,
    q: u32,
) -> Result<(u32, u32, Vec<u8>)> {
    let (w, h) = size;
    let k = (side as f64 / w.max(h).max(1) as f64).min(1.0);
    let even = |x: f64| ((x / 2.0).round() as u32 * 2).max(2);
    let (tw, th) = (even(w as f64 * k), even(h as f64 * k));
    let mut a = args(["-i".into(), input.as_os_str().to_owned()]);
    a.extend(args(["-frames:v", "1", "-vf"]));
    a.push(format!("scale={tw}:{th}:flags=lanczos").into());
    let pix = if gray { "gray" } else { "yuvj420p" };
    a.extend(args(["-c:v", "mjpeg", "-pix_fmt", pix, "-q:v"]));
    a.push(q.to_string().into());
    a.push(out.into());
    ctx.ff.run(&a, 0.0, &mut |_| {}, ctx.cancel)?;
    Ok((tw, th, std::fs::read(out)?))
}

fn int(o: Option<&Object>) -> Option<i64> {
    match o? {
        Object::Integer(i) => Some(*i),
        Object::Real(r) => Some(*r as i64),
        _ => None,
    }
}

fn shrink_pdf(ctx: &Ctx, src: &Path, out: &Path, tmp: &Path, side: u32, q: u32) -> Result<()> {
    let bad = |e: lopdf::Error| Error::Unsupported(format!("can't read this PDF: {e}"));
    let mut doc = Document::load(src).map_err(bad)?;
    let ids: Vec<_> = doc.objects.keys().copied().collect();
    for (n, id) in ids.into_iter().enumerate() {
        if ctx.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        // Read what we need first (the color space may be a reference to another object).
        let Some(Object::Stream(s)) = doc.objects.get(&id) else {
            continue;
        };
        let d = &s.dict;
        let is_image = matches!(d.get(b"Subtype"), Ok(Object::Name(n)) if n == b"Image");
        if !is_image || s.content.len() < MIN_BYTES || d.get(b"ImageMask").is_ok() {
            continue;
        }
        let (Some(w), Some(h)) = (int(d.get(b"Width").ok()), int(d.get(b"Height").ok())) else {
            continue;
        };
        if int(d.get(b"BitsPerComponent").ok()) != Some(8) || w <= 0 || h <= 0 {
            continue;
        }
        let space = match d.get(b"ColorSpace") {
            Ok(Object::Reference(r)) => doc.get_object(*r).ok(),
            other => other.ok(),
        };
        let channels = match space {
            Some(Object::Name(n)) if n == b"DeviceRGB" => 3,
            Some(Object::Name(n)) if n == b"DeviceGray" => 1,
            Some(Object::Array(a)) if matches!(a.first(), Some(Object::Name(n)) if n == b"ICCBased") =>
            {
                let icc = match a.get(1) {
                    Some(Object::Reference(r)) => doc.get_object(*r).ok(),
                    _ => None,
                };
                match icc {
                    Some(Object::Stream(st)) => match int(st.dict.get(b"N").ok()) {
                        Some(3) => 3,
                        Some(1) => 1,
                        _ => continue,
                    },
                    _ => continue,
                }
            }
            _ => continue,
        };
        let filters: Vec<Vec<u8>> = s
            .filters()
            .map(|f| f.into_iter().map(<[u8]>::to_vec).collect())
            .unwrap_or_default();
        let input;
        if filters == [b"DCTDecode".to_vec()] {
            input = tmp.join(format!("in{n}.jpg"));
            std::fs::write(&input, &s.content)?;
        } else if filters.is_empty() || filters == [b"FlateDecode".to_vec()] {
            // Raw pixels: hand them to ffmpeg as a PPM/PGM picture.
            let expect = (w * h) as usize * channels;
            let Ok(px) = s.decompressed_content_with_limit(expect + 16) else {
                continue;
            };
            if px.len() != expect {
                continue;
            }
            let (magic, ext) = if channels == 3 {
                ("P6", "ppm")
            } else {
                ("P5", "pgm")
            };
            input = tmp.join(format!("in{n}.{ext}"));
            let mut f = std::fs::File::create(&input)?;
            f.write_all(format!("{magic}\n{w} {h}\n255\n").as_bytes())?;
            f.write_all(&px)?;
        } else {
            continue;
        }
        let before = s.content.len();
        let made = tmp.join(format!("out{n}.jpg"));
        let Ok((tw, th, bytes)) = jpeg(
            ctx,
            &input,
            &made,
            (w as u32, h as u32),
            channels == 1,
            side,
            q,
        ) else {
            continue;
        };
        if bytes.len() >= before {
            continue;
        }
        if let Some(Object::Stream(s)) = doc.objects.get_mut(&id) {
            s.dict.set("Width", tw as i64);
            s.dict.set("Height", th as i64);
            s.dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
            s.dict.remove(b"DecodeParms");
            s.set_content(bytes);
        }
    }
    doc.compress();
    doc.save(out)?;
    Ok(())
}

fn shrink_office(ctx: &Ctx, src: &Path, out: &Path, tmp: &Path, side: u32, q: u32) -> Result<()> {
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;
    let bad =
        |e: zip::result::ZipError| Error::Unsupported(format!("can't read this document: {e}"));
    let mut zin = zip::ZipArchive::new(std::fs::File::open(src)?).map_err(bad)?;
    let mut zout = zip::ZipWriter::new(std::fs::File::create(out)?);
    for i in 0..zin.len() {
        if ctx.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut entry = zin.by_index(i).map_err(bad)?;
        let name = entry.name().to_string();
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        drop(entry);
        let lower = name.to_ascii_lowercase();
        if (lower.ends_with(".jpg") || lower.ends_with(".jpeg")) && data.len() >= MIN_BYTES {
            let input = tmp.join(format!("in{i}.jpg"));
            std::fs::write(&input, &data)?;
            // Size: ask ffmpeg's probe instead of decoding ourselves.
            if let Ok(Some(info)) = ctx.ff.probe_json(&input) {
                let dims = info["streams"][0]["width"]
                    .as_u64()
                    .zip(info["streams"][0]["height"].as_u64());
                if let Some((w, h)) = dims {
                    let made = tmp.join(format!("out{i}.jpg"));
                    if let Ok((_, _, bytes)) =
                        jpeg(ctx, &input, &made, (w as u32, h as u32), false, side, q)
                    {
                        if bytes.len() < data.len() {
                            data = bytes;
                        }
                    }
                }
            }
        }
        // OpenDocument needs "mimetype" first and uncompressed; everything else: smallest.
        let opts = if name == "mimetype" {
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
        } else {
            SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .compression_level(Some(9))
        };
        zout.start_file(name.as_str(), opts).map_err(bad)?;
        zout.write_all(&data)?;
    }
    zout.finish().map_err(bad)?;
    Ok(())
}
