//! Images: lossless first when the picture isn't already a JPEG (WebP lossless keeps every
//! pixel and is ~25% smaller than PNG), then JPEG (or WebP when there's transparency, or
//! when the user picked WebP), stepping quality and size down until the result fits. Huge photos are capped at 4096 px on the long side first.
//! Phone photos are often stored sideways with an EXIF "orientation" tag; ffmpeg doesn't
//! apply it to stills, so we read it ourselves and turn the picture upright.

use std::path::{Path, PathBuf};

use super::{file_size, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, Progress};
use crate::probe::MediaInfo;
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
    let v = info
        .video
        .as_ref()
        .ok_or_else(|| Error::Unsupported("unreadable image".into()))?;
    let orient = orientation(&info.path);
    let turn = orient_filter(orient);
    // A quarter turn swaps width and height.
    let (w, h) = if orient >= 5 {
        (v.height.max(1) as f64, v.width.max(1) as f64)
    } else {
        (v.width.max(1) as f64, v.height.max(1) as f64)
    };
    let base_scale = (MAX_SIDE / w.max(h)).min(1.0);
    // Transparency never becomes JPEG (JPEG can't hold it).
    let webp = info.has_alpha || ctx.image_format == ImageFormat::Webp;
    let out = with_ext(out_base, if webp { "webp" } else { "jpg" });

    // Lossless first (Auto), unless it's already a JPEG: identical pixels, often enough.
    if ctx.image_format == ImageFormat::Auto && v.codec != "mjpeg" {
        let lossless = with_ext(out_base, "webp");
        let (tw, th) = even(w * base_scale, h * base_scale);
        let mut a = still_args(info, turn, tw, th);
        a.extend(args([
            "-c:v",
            "libwebp",
            "-lossless",
            "1",
            "-compression_level",
            "6",
        ]));
        a.push(lossless.clone().into());
        ctx.ff.run(&a, 0.0, &mut |_| {}, ctx.cancel)?;
        if file_size(&lossless)? <= target {
            progress(1.0);
            return Ok(lossless);
        }
        let _ = std::fs::remove_file(&lossless);
    }

    // Every (scale, quality) pair, best-looking first. Size shrinks (roughly) along this
    // ladder, so a binary search finds the best one that fits in ~5 encodes instead of 20.
    let ladder: Vec<(f64, usize)> = SCALES
        .iter()
        .flat_map(|&s| (0..JPEG_Q.len()).map(move |q| (s, q)))
        .collect();
    let encode = |i: usize, dest: &Path| -> Result<u64> {
        let (s, qi) = ladder[i];
        let (tw, th) = even(w * base_scale * s, h * base_scale * s);
        let mut a = still_args(info, turn, tw, th);
        if webp {
            a.extend(args(["-c:v", "libwebp", "-quality"]));
            a.push(WEBP_Q[qi].to_string().into());
        } else {
            a.extend(args(["-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-q:v"]));
            a.push(JPEG_Q[qi].to_string().into());
        }
        a.push(dest.into());
        ctx.ff.run(&a, 0.0, &mut |_| {}, ctx.cancel)?;
        file_size(dest)
    };

    let tmp = tempfile::tempdir()?;
    let probe_out = tmp.path().join(out.file_name().unwrap_or_default());
    let steps = (ladder.len() as f32).log2().ceil() + 1.0;
    let (mut lo, mut hi) = (0usize, ladder.len() - 1);
    let mut best_fit: Option<usize> = None;
    let mut smallest = u64::MAX;
    let mut done = 0.0;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let size = encode(mid, &probe_out)?;
        done += 1.0;
        progress((done / steps).min(0.95));
        smallest = smallest.min(size);
        if size <= target {
            best_fit = Some(mid);
            std::fs::copy(&probe_out, &out)?;
            if mid == 0 {
                break;
            }
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
    }
    if best_fit.is_some() {
        progress(1.0);
        return Ok(out);
    }
    let _ = std::fs::remove_file(&out);
    Err(Error::DoesNotFit {
        what: name_of(&info.path),
        target,
        best: smallest,
    })
}

fn even(w: f64, h: f64) -> (u32, u32) {
    let e = |v: f64| ((v.round() as u32) / 2 * 2).max(2);
    (e(w), e(h))
}

/// One still frame of the picture, turned upright (`turn`) and scaled to `tw`×`th`.
/// `-noautorotate`: ffmpeg never turns it on its own (newer builds might), we do it once.
fn still_args(info: &MediaInfo, turn: Option<&str>, tw: u32, th: u32) -> Vec<std::ffi::OsString> {
    let mut a = args([
        "-noautorotate".into(),
        "-i".into(),
        info.path.clone().into_os_string(),
    ]);
    a.extend(args(["-frames:v", "1", "-vf"]));
    let scale = format!("scale={tw}:{th}:flags=lanczos");
    a.push(match turn {
        Some(t) => format!("{t},{scale}").into(),
        None => scale.into(),
    });
    a
}

/// EXIF orientation (1–8) of a JPEG photo; 1 (upright) when there is none.
fn orientation(path: &Path) -> u8 {
    use std::io::Read;
    let mut head = Vec::new();
    let ok = std::fs::File::open(path)
        .and_then(|f| f.take(256 * 1024).read_to_end(&mut head))
        .is_ok();
    if ok {
        exif_orientation(&head).unwrap_or(1)
    } else {
        1
    }
}

/// Finds the Orientation tag (0x0112) in a JPEG's EXIF block.
fn exif_orientation(d: &[u8]) -> Option<u8> {
    if d.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    while i + 4 <= d.len() {
        if d[i] != 0xFF {
            return None;
        }
        let marker = d[i + 1];
        let len = u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
        if marker == 0xDA {
            return None; // image data starts; no EXIF before it
        }
        if marker == 0xE1 && d.get(i + 4..i + 10)? == b"Exif\0\0" {
            let t = d.get(i + 10..(i + 2 + len).min(d.len()))?;
            let le = match t.get(0..2)? {
                b"II" => true,
                b"MM" => false,
                _ => return None,
            };
            let u16_at = |o: usize| {
                t.get(o..o + 2).map(|b| {
                    if le {
                        u16::from_le_bytes([b[0], b[1]])
                    } else {
                        u16::from_be_bytes([b[0], b[1]])
                    }
                })
            };
            let u32_at = |o: usize| {
                t.get(o..o + 4).map(|b| {
                    let a = [b[0], b[1], b[2], b[3]];
                    if le {
                        u32::from_le_bytes(a)
                    } else {
                        u32::from_be_bytes(a)
                    }
                })
            };
            let ifd = u32_at(4)? as usize;
            for k in 0..u16_at(ifd)? as usize {
                let e = ifd + 2 + k * 12;
                if u16_at(e)? == 0x0112 {
                    let v = u16_at(e + 8)?;
                    return (1..=8).contains(&v).then_some(v as u8);
                }
            }
            return None;
        }
        i += 2 + len;
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

    /// A minimal JPEG start with an EXIF block holding just the orientation tag.
    fn jpeg_with(orient: u16, little: bool) -> Vec<u8> {
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
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(&tiff);
        let mut d = vec![0xFF, 0xD8, 0xFF, 0xE1];
        d.extend(((app1.len() + 2) as u16).to_be_bytes());
        d.extend(app1);
        d.extend([0xFF, 0xDA, 0, 2]);
        d
    }

    #[test]
    fn reads_orientation_both_byte_orders() {
        assert_eq!(exif_orientation(&jpeg_with(6, true)), Some(6));
        assert_eq!(exif_orientation(&jpeg_with(8, false)), Some(8));
        assert_eq!(exif_orientation(&jpeg_with(1, true)), Some(1));
        assert_eq!(exif_orientation(b"not a jpeg"), None);
        assert_eq!(orient_filter(6), Some("transpose=1"));
        assert_eq!(orient_filter(1), None);
    }
}
