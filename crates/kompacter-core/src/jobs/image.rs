//! Fit a still image by preserving native dimensions where possible, then trying
//! smaller resolutions. HEIC/HEIF and AVIF are first decoded to PNG so ffmpeg can
//! assemble the tiles and apply the container orientation.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::{file_size, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, Progress};
use crate::probe::{is_heif_like, probe, MediaInfo};
use crate::ImageFormat;

const MAX_SIDE: f64 = 4096.0;
const SCALES: [f64; 5] = [1.0, 0.75, 0.5, 0.35, 0.25];
/// mjpeg `-q:v` (lower is better).
const JPEG_Q: [u32; 4] = [3, 6, 10, 15];
/// libwebp `-quality` (higher is better).
const WEBP_Q: [u32; 4] = [85, 70, 55, 40];

pub(crate) fn fit(
    ctx: &Ctx,
    info: &MediaInfo,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let source = image_features(&info.path)?;
    if source.animated {
        return Err(Error::Unsupported(
            "this animated image cannot be compressed without dropping frames".into(),
        ));
    }
    if info.video.as_ref().is_some_and(|v| v.hdr.is_some()) {
        return Err(Error::Unsupported(
            "this image's HDR colors cannot be preserved in the selected format".into(),
        ));
    }
    if !is_heif_like(&info.path) {
        return fit_still(ctx, info, source, true, target, out_base, progress);
    }
    let tmp = tempfile::tempdir()?;
    let png = tmp.path().join("photo.png");
    let mut a = args(["-i".into(), info.path.clone().into_os_string()]);
    a.extend(args(["-frames:v", "1", "-c:v", "png"]));
    a.push(png.clone().into());
    ctx.ff.run(&a, 0.0, &mut |_| {}, ctx.cancel)?;
    let decoded = probe(ctx.ff, &png)?;
    // Already a lossy photo: a lossless copy would only be bigger.
    match fit_still(
        ctx,
        &decoded,
        image_features(&png)?,
        false,
        target,
        out_base,
        progress,
    ) {
        Err(Error::DoesNotFit { target, best, .. }) => Err(Error::DoesNotFit {
            what: name_of(&info.path),
            target,
            best,
        }),
        other => other,
    }
}

fn fit_still(
    ctx: &Ctx,
    info: &MediaInfo,
    source: ImageFeatures,
    try_webp_native: bool,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let v = info
        .video
        .as_ref()
        .ok_or_else(|| Error::Unsupported("unreadable image".into()))?;
    let orient = source.orientation.unwrap_or(1);
    let turn = orient_filter(orient);
    // ffprobe already applies the display matrix when reporting the dimensions.
    // Use our EXIF transform instead of ffmpeg's autorotation when both exist.
    let (w, h) = if orient >= 5 && !v.rotated {
        (v.height.max(1), v.width.max(1))
    } else {
        (v.width.max(1), v.height.max(1))
    };
    let base_scale = (MAX_SIDE / f64::from(w.max(h))).min(1.0);

    // WebP's ffmpeg encoder does not write ICC profiles. The JPEG encoder can
    // copy ICC side data, but it cannot carry PNG gamma/chromaticity or alpha.
    if source.png_color
        || v.hdr.is_some()
        || (source.icc && (info.has_alpha || ctx.image_format == ImageFormat::Webp))
    {
        return Err(Error::Unsupported(
            "this image's color metadata cannot be preserved in the selected format".into(),
        ));
    }

    let webp = info.has_alpha || ctx.image_format == ImageFormat::Webp;
    let out = with_ext(out_base, if webp { "webp" } else { "jpg" });
    let tmp = tempfile::tempdir_in(out.parent().unwrap_or_else(|| Path::new(".")))?;
    let mut smallest = info.size_bytes;

    // Native-sized, unprofiled 8-bit RGB can pass through WebP's lossless mode
    // without a size change or chroma subsampling.
    if try_webp_native
        && ctx.image_format == ImageFormat::Auto
        && !source.icc
        && !info.has_alpha
        && v.pix_fmt == "rgb24"
        && base_scale == 1.0
    {
        let candidate = tmp.path().join("native.webp");
        let mut a = still_args(info, turn, None);
        a.extend(args([
            "-c:v",
            "libwebp",
            "-pix_fmt",
            "bgra",
            "-lossless",
            "1",
            "-compression_level",
            "6",
        ]));
        a.push(candidate.clone().into());
        ctx.ff.run(&a, 0.0, &mut |_| {}, ctx.cancel)?;
        let size = file_size(&candidate)?;
        if size <= target {
            let lossless = with_ext(out_base, "webp");
            if ctx.cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            std::fs::rename(&candidate, &lossless)?;
            progress(1.0);
            return Ok(lossless);
        }
        smallest = smallest.min(size);
    }

    let candidate = tmp.path().join(out.file_name().unwrap_or_default());
    let mut attempted = 0;
    let mut previous = None;
    let qualities = if webp { WEBP_Q } else { JPEG_Q };

    // Resolution takes priority so detail such as small text remains legible.
    // The first fitting quality within a resolution is its best. A smaller
    // resolution's high-quality encode can be larger than a bigger one's
    // low-quality encode, so never use a miss to skip another resolution.
    for scale in SCALES {
        let size = dimensions(w, h, base_scale * scale);
        if previous == Some(size) {
            continue;
        }
        previous = Some(size);
        for quality in qualities {
            let mut a = still_args(info, turn, Some(size));
            if webp {
                a.extend(args(["-c:v", "libwebp", "-quality"]));
            } else {
                a.extend(args(["-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-q:v"]));
            }
            a.push(quality.to_string().into());
            a.push(candidate.clone().into());
            ctx.ff.run(&a, 0.0, &mut |_| {}, ctx.cancel)?;
            let bytes = file_size(&candidate)?;
            if bytes <= target {
                if source.icc && !image_features(&candidate)?.icc {
                    return Err(Error::Unsupported(
                        "the image's color profile was not retained by the encoder".into(),
                    ));
                }
                if ctx.cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                std::fs::rename(&candidate, &out)?;
                progress(1.0);
                return Ok(out);
            }
            smallest = smallest.min(bytes);
            attempted += 1;
            progress(attempted as f32 / (SCALES.len() * qualities.len() + 1) as f32);
        }
    }

    Err(Error::DoesNotFit {
        what: name_of(&info.path),
        target,
        best: smallest,
    })
}

fn dimensions(w: u32, h: u32, scale: f64) -> (u32, u32) {
    if scale == 1.0 {
        return (w, h);
    }
    let scaled = |side: u32| {
        let pixels = (f64::from(side) * scale).round() as u32;
        (pixels / 2 * 2).clamp(1, side)
    };
    (scaled(w), scaled(h))
}

fn still_args(
    info: &MediaInfo,
    turn: Option<&str>,
    size: Option<(u32, u32)>,
) -> Vec<std::ffi::OsString> {
    let mut a = Vec::new();
    if turn.is_some() {
        a.push("-noautorotate".into());
    }
    a.extend(args(["-i".into(), info.path.clone().into_os_string()]));
    a.extend(args(["-frames:v", "1"]));
    let mut filters = Vec::new();
    if let Some(t) = turn {
        filters.push(t.to_owned());
    }
    if let Some((w, h)) = size {
        filters.push(format!("scale={w}:{h}:flags=lanczos"));
    }
    if !filters.is_empty() {
        a.push("-vf".into());
        a.push(filters.join(",").into());
    }
    a
}

#[derive(Default)]
struct ImageFeatures {
    animated: bool,
    icc: bool,
    png_color: bool,
    orientation: Option<u8>,
}

fn image_features(path: &Path) -> Result<ImageFeatures> {
    let mut f = BufReader::new(File::open(path)?);
    let mut features = ImageFeatures::default();
    let mut signature = [0; 12];
    if f.read_exact(&mut signature).is_err() {
        return Ok(features);
    }
    if signature.starts_with(b"\x89PNG\r\n\x1a\n") {
        f.seek(SeekFrom::Start(8))?;
        let (mut srgb, mut custom_color) = (false, false);
        let mut chunk = [0; 8];
        while f.read_exact(&mut chunk).is_ok() {
            let len = u32::from_be_bytes(chunk[..4].try_into().unwrap());
            match &chunk[4..] {
                b"IDAT" | b"IEND" => break,
                b"acTL" => features.animated = true,
                b"iCCP" => features.icc = true,
                b"sRGB" => srgb = true,
                b"gAMA" | b"cHRM" => custom_color = true,
                b"eXIf" => {
                    features.orientation = read_orientation(&mut f, len)?;
                    f.seek(SeekFrom::Current(4))?;
                    continue;
                }
                b"cICP" => features.png_color = true,
                _ => {}
            }
            f.seek(SeekFrom::Current(i64::from(len) + 4))?;
        }
        features.png_color |= custom_color && !srgb && !features.icc;
    } else if &signature[..4] == b"RIFF" && &signature[8..] == b"WEBP" {
        // VP8X is the first chunk of extended WebP. Its flags describe the
        // animation and ICC chunks regardless of the decoder's frame support.
        let mut chunk = [0; 8];
        while f.read_exact(&mut chunk).is_ok() {
            let len = u32::from_le_bytes(chunk[4..].try_into().unwrap());
            match &chunk[..4] {
                b"VP8X" if len >= 1 => {
                    let mut flags = [0; 1];
                    f.read_exact(&mut flags)?;
                    features.animated = flags[0] & 0x02 != 0;
                    features.icc = flags[0] & 0x20 != 0;
                    if features.animated || flags[0] & 0x08 == 0 {
                        break;
                    }
                    f.seek(SeekFrom::Current(i64::from(len - 1) + i64::from(len % 2)))?;
                }
                b"EXIF" => {
                    features.orientation = read_orientation(&mut f, len)?;
                    break;
                }
                _ => {
                    f.seek(SeekFrom::Current(i64::from(len) + i64::from(len % 2)))?;
                }
            }
        }
    } else if signature.starts_with(&[0xff, 0xd8]) {
        f.seek(SeekFrom::Start(2))?;
        let mut marker = [0; 2];
        while f.read_exact(&mut marker).is_ok() && marker[0] == 0xff {
            if marker[1] == 0xff {
                // JPEG allows padding with an extra 0xff between markers.
                f.seek(SeekFrom::Current(-1))?;
                continue;
            }
            if matches!(marker[1], 0xda | 0xd9) {
                break;
            }
            let mut len = [0; 2];
            f.read_exact(&mut len)?;
            let Some(len) = u16::from_be_bytes(len).checked_sub(2) else {
                break;
            };
            if marker[1] == 0xe2 && len >= 12 {
                let mut id = [0; 12];
                f.read_exact(&mut id)?;
                if &id == b"ICC_PROFILE\0" {
                    features.icc = true;
                }
                f.seek(SeekFrom::Current(i64::from(len - 12)))?;
            } else if marker[1] == 0xe1 {
                features.orientation =
                    read_orientation(&mut f, u32::from(len))?.or(features.orientation);
            } else {
                f.seek(SeekFrom::Current(i64::from(len)))?;
            }
        }
    }
    Ok(features)
}

/// EXIF orientation (1–8) from the first TIFF directory in an EXIF block.
fn read_orientation(f: &mut (impl Read + Seek), len: u32) -> Result<Option<u8>> {
    let prefix_len = len.min(64 * 1024);
    let mut prefix = vec![0; prefix_len as usize];
    f.read_exact(&mut prefix)?;
    f.seek(SeekFrom::Current(i64::from(len - prefix_len)))?;
    let tiff = prefix.strip_prefix(b"Exif\0\0").unwrap_or(&prefix);
    Ok(exif_orientation(tiff))
}

fn exif_orientation(t: &[u8]) -> Option<u8> {
    let le = match t.get(..4)? {
        b"II\x2a\0" => true,
        b"MM\0\x2a" => false,
        _ => return None,
    };
    let u16_at = |offset| {
        let bytes: [u8; 2] = t.get(offset..offset + 2)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let offset = t.get(4..8)?.try_into().ok()?;
    let ifd = if le {
        u32::from_le_bytes(offset)
    } else {
        u32::from_be_bytes(offset)
    };
    let ifd = usize::try_from(ifd).ok()?;
    let entries = t.get(ifd..)?;
    let count = u16_at(ifd)?;
    let (entries, _) = entries.get(2..)?.as_chunks::<12>();
    for entry in entries.iter().take(count as usize) {
        let (tag, short, value) = if le {
            ([0x12, 0x01], [3, 0], entry[8])
        } else {
            ([0x01, 0x12], [0, 3], entry[9])
        };
        if entry[..2] == tag && entry[2..4] == short {
            return (1..=8).contains(&value).then_some(value);
        }
    }
    None
}

/// ffmpeg filter that turns a photo with this EXIF orientation upright.
fn orient_filter(o: u8) -> Option<&'static str> {
    match o {
        2 => Some("hflip"),
        3 => Some("hflip,vflip"),
        4 => Some("vflip"),
        5 => Some("transpose=0"),
        6 => Some("transpose=1"),
        7 => Some("transpose=3"),
        8 => Some("transpose=2"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TIFF directory holding just the EXIF orientation tag.
    fn tiff_with(orient: u16, little: bool) -> Vec<u8> {
        let mut tiff: Vec<u8> = Vec::new();
        let w16 = |v: u16| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let w32 = |v: u32| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        tiff.extend(if little { b"II" } else { b"MM" });
        tiff.extend(w16(42));
        tiff.extend(w32(8)); // first IFD right after the header
        tiff.extend(w16(1)); // one entry
        tiff.extend(w16(0x0112));
        tiff.extend(w16(3)); // SHORT
        tiff.extend(w32(1));
        tiff.extend(w16(orient));
        tiff.extend([0, 0]);
        tiff.extend(w32(0));
        tiff
    }

    #[test]
    fn reads_orientation_both_byte_orders() {
        assert_eq!(exif_orientation(&tiff_with(6, true)), Some(6));
        assert_eq!(exif_orientation(&tiff_with(8, false)), Some(8));
        assert_eq!(exif_orientation(&tiff_with(1, true)), Some(1));
        assert_eq!(exif_orientation(b"not a jpeg"), None);
        assert_eq!(orient_filter(6), Some("transpose=1"));
        assert_eq!(orient_filter(1), None);
    }
}
