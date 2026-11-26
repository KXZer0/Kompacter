//! Fit a still image by preserving native dimensions where possible, then trying
//! smaller resolutions. HEIC/HEIF and AVIF are first decoded to PNG so ffmpeg can
//! assemble the tiles and apply the container orientation.
//!
//! Colour is kept rather than refused: ffmpeg's WebP encoder drops ICC profiles and
//! its JPEG encoder doesn't always copy them, so the source's profile is read here and
//! written back into the result. HDR stills are tone-mapped to SDR when this ffmpeg can.
//! Animated PNG/WebP become GIFs, which autoplay everywhere they're posted, through the
//! same pipeline as clips; when no GIF fits, their first frame is compressed as a still.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;

use super::video::can_retry;
use super::{file_size, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, command, Progress};
use crate::hdr;
use crate::probe::{is_heif_like, probe, MediaInfo, VideoStream};
use crate::ImageFormat;

const MAX_SIDE: f64 = 4096.0;
const SCALES: [f64; 5] = [1.0, 0.75, 0.5, 0.35, 0.25];
/// mjpeg `-q:v` (lower is better).
const JPEG_Q: [u32; 4] = [3, 6, 10, 15];
/// libwebp `-quality` (higher is better).
const WEBP_Q: [u32; 4] = [85, 70, 55, 40];
/// Largest ICC profile carried over. Real ones are a few KiB.
const MAX_ICC: u64 = 4 << 20;
/// ICC bytes per JPEG APP2 segment: 65535 minus the length, id and sequence bytes.
const ICC_SEGMENT: usize = 65_519;

pub(crate) fn fit(
    ctx: &Ctx,
    info: &MediaInfo,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let mut source = image_features(&info.path)?;
    source.hdr |= info.video.as_ref().is_some_and(|v| v.hdr.is_some());
    if source.animated {
        // Keep it moving when we can; when no GIF fits (or it's too long for one),
        // its first frame is compressed as a still instead.
        match fit_gif(ctx, info, target, out_base, &mut *progress) {
            Err(err @ Error::Ffmpeg(_)) if source.webp && can_retry(&err) => {
                // ffmpeg before 8.0 has no animated WebP decoder; its first frame
                // can't be read either.
                return Err(Error::Unsupported(
                    "this ffmpeg can't read animated WebP".into(),
                ));
            }
            Err(err)
                if can_retry(&err)
                    || matches!(err, Error::TooLong { .. } | Error::Unsupported(_)) => {}
            result => return result,
        }
    }
    if !is_heif_like(&info.path) {
        return fit_still(ctx, info, &source, true, target, out_base, progress);
    }
    let tmp = tempfile::tempdir()?;
    let png = tmp.path().join("photo.png");
    let mut a = args(["-i".into(), info.path.clone().into_os_string()]);
    a.extend(args(["-frames:v", "1", "-c:v", "png"]));
    a.push(png.clone().into());
    ctx.ff.run(&a, 0.0, &mut |_| {}, ctx.cancel)?;
    let decoded = probe(ctx.ff, &png)?;
    let mut features = image_features(&png)?;
    features.hdr |= source.hdr;
    if features.icc.is_none() {
        features.icc = heif_icc(&info.path);
    }
    // Already a lossy photo: a lossless copy would only be bigger.
    match fit_still(ctx, &decoded, &features, false, target, out_base, progress) {
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
    source: &ImageFeatures,
    try_webp_native: bool,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    // HDR is tone-mapped to SDR when this ffmpeg can (transparency wins: the chain
    // drops it). An oddly tagged file can still make the filters refuse; then it is
    // encoded as it is, like any other picture.
    if source.hdr && !info.has_alpha && (ctx.tonemap)() {
        let tonemapped = encode_still(
            ctx,
            info,
            source,
            try_webp_native,
            true,
            target,
            out_base,
            &mut *progress,
        );
        match tonemapped {
            Err(err @ Error::Ffmpeg(_)) if can_retry(&err) => {}
            result => return result,
        }
    }
    encode_still(
        ctx,
        info,
        source,
        try_webp_native,
        false,
        target,
        out_base,
        progress,
    )
}

#[allow(clippy::too_many_arguments)]
fn encode_still(
    ctx: &Ctx,
    info: &MediaInfo,
    source: &ImageFeatures,
    try_webp_native: bool,
    tonemap: bool,
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
    let (w, h) = upright_size(v, orient);
    let base_scale = (MAX_SIDE / f64::from(w.max(h))).min(1.0);
    // Tone-mapped pixels are plain BT.709 now, which the old profile doesn't describe.
    let icc = source.icc.as_deref().filter(|_| !tonemap);

    let webp = info.has_alpha || ctx.image_format == ImageFormat::Webp;
    let out = with_ext(out_base, if webp { "webp" } else { "jpg" });
    let tmp = tempfile::tempdir_in(out.parent().unwrap_or_else(|| Path::new(".")))?;
    let mut smallest = info.size_bytes;

    // Native-sized 8-bit RGB can pass through WebP's lossless mode without a size
    // change or chroma subsampling. Its ICC profile is written back afterwards.
    if try_webp_native
        && !tonemap
        && ctx.image_format == ImageFormat::Auto
        && !info.has_alpha
        && v.pix_fmt == "rgb24"
        && base_scale == 1.0
    {
        let candidate = tmp.path().join("native.webp");
        let mut a = still_args(info, turn, None, false);
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
        let size = finish(&candidate, icc, true)?;
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
            let mut a = still_args(info, turn, Some(size), tonemap);
            if webp {
                a.extend(args(["-c:v", "libwebp", "-quality"]));
            } else {
                a.extend(args(["-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-q:v"]));
            }
            a.push(quality.to_string().into());
            a.push(candidate.clone().into());
            ctx.ff.run(&a, 0.0, &mut |_| {}, ctx.cancel)?;
            // Measured with the profile in, since it's part of what gets uploaded.
            let bytes = finish(&candidate, icc, webp)?;
            if bytes <= target {
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

/// An animated PNG or WebP as a GIF, through the clip pipeline (palette, then size and
/// frame rate stepped down). ffprobe often reports no duration for these, so it is
/// added up from the frames.
fn fit_gif(
    ctx: &Ctx,
    info: &MediaInfo,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let mut info = info.clone();
    if info.duration.is_nan() || info.duration <= 0.0 {
        info.duration = frames_duration(ctx, &info).unwrap_or(0.0);
    }
    super::gif::fit(ctx, &info, None, target, out_base, progress)
}

/// Seconds of an animation: its frames' durations added up, or frames / frame rate.
fn frames_duration(ctx: &Ctx, info: &MediaInfo) -> Option<f64> {
    let out = command(&ctx.ff.ffprobe)
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "packet=duration_time", "-of", "csv=p=0"])
        .arg(&info.path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let frames: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let summed: f64 = frames
        .iter()
        .filter_map(|l| l.trim_end_matches(',').parse::<f64>().ok())
        .sum();
    if summed > 0.0 {
        return Some(summed);
    }
    let fps = info.video.as_ref()?.fps;
    (fps > 0.0 && !frames.is_empty()).then(|| frames.len() as f64 / fps)
}

/// Width and height once turned upright. ffprobe already applies the display matrix
/// when reporting the dimensions; our EXIF transform replaces ffmpeg's autorotation.
fn upright_size(v: &VideoStream, orient: u8) -> (u32, u32) {
    // A quarter turn swaps width and height.
    if orient >= 5 && !v.rotated {
        (v.height.max(1), v.width.max(1))
    } else {
        (v.width.max(1), v.height.max(1))
    }
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
    tonemap: bool,
) -> Vec<std::ffi::OsString> {
    let mut a = Vec::new();
    if turn.is_some() {
        a.push("-noautorotate".into());
    }
    a.extend(args(["-i".into(), info.path.clone().into_os_string()]));
    a.extend(args(["-frames:v", "1"]));
    let mut filters = Vec::new();
    if tonemap {
        filters.push(hdr::TONEMAP.to_owned());
    }
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

/// Writes the source's ICC profile into an encoded WebP or JPEG (unless the encoder
/// already kept it) and returns the final size.
fn finish(path: &Path, icc: Option<&[u8]>, webp: bool) -> Result<u64> {
    if let Some(icc) = icc {
        let data = std::fs::read(path)?;
        let tagged = if webp {
            webp_with_icc(&data, icc)
        } else {
            jpeg_with_icc(&data, icc)
        };
        if let Some(tagged) = tagged {
            std::fs::write(path, tagged)?;
        }
    }
    file_size(path)
}

/// The WebP with `icc` in an ICCP chunk. A simple (VP8/VP8L) file becomes the
/// extended format, which starts with a VP8X header carrying the canvas size and flags.
fn webp_with_icc(data: &[u8], icc: &[u8]) -> Option<Vec<u8>> {
    if data.get(..4)? != b"RIFF" || data.get(8..12)? != b"WEBP" || icc.is_empty() {
        return None;
    }
    let mut chunks: Vec<(&[u8], &[u8])> = Vec::new();
    let mut i = 12;
    while i + 8 <= data.len() {
        let len = u32::from_le_bytes(data[i + 4..i + 8].try_into().ok()?) as usize;
        chunks.push((&data[i..i + 4], data.get(i + 8..i + 8 + len)?));
        i += 8 + len + len % 2;
    }
    let mut vp8x = match *chunks.first()? {
        (b"VP8X", p) if p.len() >= 10 => p.to_vec(),
        (b"VP8 ", p) => {
            // Frame tag, start code, then 14-bit width and height.
            if p.get(3..6)? != [0x9d, 0x01, 0x2a] {
                return None;
            }
            let w = u16::from_le_bytes(p.get(6..8)?.try_into().ok()?) & 0x3fff;
            let h = u16::from_le_bytes(p.get(8..10)?.try_into().ok()?) & 0x3fff;
            vp8x_header(u32::from(w), u32::from(h), false)
        }
        (b"VP8L", p) => {
            // Signature, then width-1 and height-1 (14 bits each) and the alpha bit.
            if *p.first()? != 0x2f {
                return None;
            }
            let bits = u32::from_le_bytes(p.get(1..5)?.try_into().ok()?);
            let (w, h) = ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1);
            vp8x_header(w, h, (bits >> 28) & 1 == 1)
        }
        _ => return None,
    };
    vp8x[0] |= 0x20;
    if chunks.iter().any(|(id, _)| *id == b"ALPH") {
        vp8x[0] |= 0x10;
    }
    chunks.retain(|(id, _)| *id != b"VP8X" && *id != b"ICCP");

    let mut body = b"WEBP".to_vec();
    let mut push = |id: &[u8], payload: &[u8]| {
        body.extend(id);
        body.extend((payload.len() as u32).to_le_bytes());
        body.extend(payload);
        if payload.len() % 2 == 1 {
            body.push(0);
        }
    };
    push(b"VP8X", &vp8x);
    push(b"ICCP", icc);
    for (id, payload) in chunks {
        push(id, payload);
    }
    let mut out = b"RIFF".to_vec();
    out.extend(u32::try_from(body.len()).ok()?.to_le_bytes());
    out.extend(body);
    Some(out)
}

/// VP8X payload: flags, three reserved bytes, canvas width-1 and height-1 (24-bit LE).
fn vp8x_header(w: u32, h: u32, alpha: bool) -> Vec<u8> {
    let mut p = vec![if alpha { 0x10 } else { 0 }, 0, 0, 0];
    p.extend(&(w.max(1) - 1).to_le_bytes()[..3]);
    p.extend(&(h.max(1) - 1).to_le_bytes()[..3]);
    p
}

/// The JPEG with `icc` in APP2 `ICC_PROFILE` segments right after its JFIF/EXIF
/// headers; `None` when it already has a profile (or isn't a JPEG).
fn jpeg_with_icc(data: &[u8], icc: &[u8]) -> Option<Vec<u8>> {
    if data.get(..2)? != [0xff, 0xd8] || icc.is_empty() {
        return None;
    }
    let segments = jpeg_segments(data);
    if segments
        .iter()
        .any(|&(_, marker, p)| marker == 0xe2 && p.starts_with(b"ICC_PROFILE\0"))
    {
        return None;
    }
    let count = u8::try_from(icc.len().div_ceil(ICC_SEGMENT)).ok()?;
    let at = segments
        .iter()
        .take_while(|&&(_, marker, _)| matches!(marker, 0xe0 | 0xe1))
        .last()
        .map_or(2, |&(start, _, p)| start + 4 + p.len());
    let mut out = Vec::with_capacity(data.len() + icc.len() + 18 * usize::from(count));
    out.extend(&data[..at]);
    for (seq, part) in icc.chunks(ICC_SEGMENT).enumerate() {
        out.extend([0xff, 0xe2]);
        out.extend(((part.len() + 16) as u16).to_be_bytes());
        out.extend(b"ICC_PROFILE\0");
        out.extend([seq as u8 + 1, count]);
        out.extend(part);
    }
    out.extend(&data[at..]);
    Some(out)
}

/// The marker segments before a JPEG's image data: (offset, marker, payload).
fn jpeg_segments(d: &[u8]) -> Vec<(usize, u8, &[u8])> {
    let mut out = Vec::new();
    let mut i = 2;
    while i + 1 < d.len() && d[i] == 0xff {
        let marker = d[i + 1];
        if marker == 0xff {
            i += 1; // fill byte
            continue;
        }
        if matches!(marker, 0xda | 0xd9) {
            break;
        }
        let Some(len) = d
            .get(i + 2..i + 4)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
        else {
            break;
        };
        let Some(payload) = d.get(i + 4..i + 2 + len) else {
            break;
        };
        out.push((i, marker, payload));
        i += 2 + len;
    }
    out
}

/// A JPEG's ICC profile from its APP2 parts (sequence number, count, bytes), put back
/// in order. `None` unless every part is there.
fn jpeg_icc(mut parts: Vec<(u8, u8, Vec<u8>)>) -> Option<Vec<u8>> {
    parts.sort_by_key(|&(seq, _, _)| seq);
    let count = parts.first()?.1;
    let complete = parts.len() == usize::from(count)
        && parts
            .iter()
            .enumerate()
            .all(|(i, &(seq, n, _))| usize::from(seq) == i + 1 && n == count);
    complete.then(|| parts.into_iter().flat_map(|(_, _, p)| p).collect())
}

/// The decompressed profile in a PNG iCCP chunk: name, NUL, method 0, zlib data.
fn png_icc(p: &[u8]) -> Option<Vec<u8>> {
    let nul = p.iter().position(|&b| b == 0)?;
    if *p.get(nul + 1)? != 0 {
        return None;
    }
    let mut icc = Vec::new();
    flate2::read::ZlibDecoder::new(p.get(nul + 2..)?)
        .take(MAX_ICC)
        .read_to_end(&mut icc)
        .ok()?;
    (!icc.is_empty()).then_some(icc)
}

/// The ICC profile in a HEIF/AVIF `colr` box (`prof` or `rICC`), in case ffmpeg's
/// decoded PNG didn't keep it.
fn heif_icc(path: &Path) -> Option<Vec<u8>> {
    let mut head = Vec::new();
    File::open(path)
        .ok()?
        .take(4 << 20)
        .read_to_end(&mut head)
        .ok()?;
    [b"colrprof", b"colrrICC"].iter().find_map(|needle| {
        let at = head.windows(8).position(|w| w == *needle)?;
        let start = at.checked_sub(4)?;
        let size = u32::from_be_bytes(head.get(start..at)?.try_into().ok()?) as usize;
        let icc = head.get(at + 8..start.checked_add(size)?)?;
        (!icc.is_empty()).then(|| icc.to_vec())
    })
}

/// Reads a chunk's `len` bytes, or skips them when it's too big to be a real profile.
fn read_payload(f: &mut (impl Read + Seek), len: u32) -> Result<Option<Vec<u8>>> {
    if u64::from(len) > MAX_ICC {
        f.seek(SeekFrom::Current(i64::from(len)))?;
        return Ok(None);
    }
    let mut p = vec![0; len as usize];
    f.read_exact(&mut p)?;
    Ok(Some(p))
}

#[derive(Default)]
struct ImageFeatures {
    animated: bool,
    /// A WebP file (animated ones need a newer ffmpeg to decode).
    webp: bool,
    /// The embedded ICC profile, written back into the result.
    icc: Option<Vec<u8>>,
    /// PQ or HLG colours (a PNG's cICP chunk here, ffprobe's tags in `fit`).
    hdr: bool,
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
        // gAMA/cHRM without sRGB or a profile are read as sRGB, as most viewers do.
        f.seek(SeekFrom::Start(8))?;
        let mut chunk = [0; 8];
        while f.read_exact(&mut chunk).is_ok() {
            let len = u32::from_be_bytes(chunk[..4].try_into().unwrap());
            match &chunk[4..] {
                b"IDAT" | b"IEND" => break,
                b"acTL" => features.animated = true,
                b"iCCP" => {
                    features.icc = read_payload(&mut f, len)?.and_then(|p| png_icc(&p));
                    f.seek(SeekFrom::Current(4))?;
                    continue;
                }
                b"cICP" if len == 4 => {
                    // Primaries, transfer, matrix, range; transfer 16 is PQ, 18 HLG.
                    let mut cicp = [0; 4];
                    f.read_exact(&mut cicp)?;
                    features.hdr = matches!(cicp[1], 16 | 18);
                    f.seek(SeekFrom::Current(4))?;
                    continue;
                }
                b"eXIf" => {
                    features.orientation = read_orientation(&mut f, len)?;
                    f.seek(SeekFrom::Current(4))?;
                    continue;
                }
                _ => {}
            }
            f.seek(SeekFrom::Current(i64::from(len) + 4))?;
        }
    } else if &signature[..4] == b"RIFF" && &signature[8..] == b"WEBP" {
        features.webp = true;
        let mut chunk = [0; 8];
        while f.read_exact(&mut chunk).is_ok() {
            let len = u32::from_le_bytes(chunk[4..].try_into().unwrap());
            let pad = i64::from(len % 2);
            match &chunk[..4] {
                // Extended WebP starts with VP8X, whose flags say if it's animated.
                b"VP8X" if len >= 1 => {
                    let mut flags = [0; 1];
                    f.read_exact(&mut flags)?;
                    features.animated = flags[0] & 0x02 != 0;
                    f.seek(SeekFrom::Current(i64::from(len - 1) + pad))?;
                }
                b"ICCP" => {
                    features.icc = read_payload(&mut f, len)?.filter(|p| !p.is_empty());
                    f.seek(SeekFrom::Current(pad))?;
                }
                b"EXIF" => {
                    features.orientation = read_orientation(&mut f, len)?;
                    f.seek(SeekFrom::Current(pad))?;
                }
                _ => {
                    f.seek(SeekFrom::Current(i64::from(len) + pad))?;
                }
            }
        }
    } else if signature.starts_with(&[0xff, 0xd8]) {
        f.seek(SeekFrom::Start(2))?;
        let mut icc_parts = Vec::new();
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
            if marker[1] == 0xe2 && len >= 14 {
                // A profile can span several APP2 segments, each numbered.
                let mut id = [0; 14];
                f.read_exact(&mut id)?;
                if &id[..12] == b"ICC_PROFILE\0" {
                    let mut part = vec![0; usize::from(len - 14)];
                    f.read_exact(&mut part)?;
                    icc_parts.push((id[12], id[13], part));
                } else {
                    f.seek(SeekFrom::Current(i64::from(len - 14)))?;
                }
            } else if marker[1] == 0xe1 {
                features.orientation =
                    read_orientation(&mut f, u32::from(len))?.or(features.orientation);
            } else {
                f.seek(SeekFrom::Current(i64::from(len)))?;
            }
        }
        features.icc = jpeg_icc(icc_parts);
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

    fn riff(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (id, p) in chunks {
            body.extend(*id);
            body.extend((p.len() as u32).to_le_bytes());
            body.extend(*p);
            if p.len() % 2 == 1 {
                body.push(0);
            }
        }
        [
            b"RIFF".as_slice(),
            &(body.len() as u32).to_le_bytes(),
            &body,
        ]
        .concat()
    }

    #[test]
    fn simple_webp_becomes_extended_with_a_profile() {
        // VP8 key frame header for 300x7: tag, start code, 14-bit sizes.
        let vp8 = [
            0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x2c, 0x01, 0x07, 0x00, 0xaa,
        ];
        let icc = [1, 2, 3]; // odd length: padded
        let out = webp_with_icc(&riff(&[(b"VP8 ", &vp8)]), &icc).unwrap();
        let mut flagged = vp8x_header(300, 7, false);
        flagged[0] = 0x20;
        assert_eq!(
            out,
            riff(&[(b"VP8X", &flagged), (b"ICCP", &icc), (b"VP8 ", &vp8)])
        );
        assert_eq!(&flagged[4..], [43, 1, 0, 6, 0, 0]);

        // VP8L: 0x2f, then (w-1) | (h-1) << 14 | alpha << 28.
        let bits: u32 = 99 | (49 << 14) | (1 << 28);
        let vp8l = [&[0x2f][..], &bits.to_le_bytes()].concat();
        let out = webp_with_icc(&riff(&[(b"VP8L", &vp8l)]), &icc).unwrap();
        let mut header = vp8x_header(100, 50, true);
        header[0] |= 0x20;
        assert_eq!(header[0], 0x30);
        assert_eq!(
            out,
            riff(&[(b"VP8X", &header), (b"ICCP", &icc), (b"VP8L", &vp8l)])
        );

        // Already extended (with alpha): the flag is set, an old profile replaced.
        let ext = vp8x_header(10, 10, false);
        let src = riff(&[
            (b"VP8X", &ext),
            (b"ICCP", b"old"),
            (b"ALPH", &[0]),
            (b"VP8 ", &vp8),
        ]);
        let out = webp_with_icc(&src, &icc).unwrap();
        let mut header = ext.clone();
        header[0] = 0x30;
        assert_eq!(
            out,
            riff(&[
                (b"VP8X", &header),
                (b"ICCP", &icc),
                (b"ALPH", &[0]),
                (b"VP8 ", &vp8)
            ])
        );
        assert_eq!(webp_with_icc(b"not a webp file", &icc), None);
    }

    #[test]
    fn jpeg_profile_is_split_after_the_headers_and_reassembled() {
        let app0 = [0xff, 0xe0, 0x00, 0x04, b'J', b'F'];
        let rest = [
            0xff, 0xdb, 0x00, 0x03, 0x00, 0xff, 0xda, 0x00, 0x02, 0xff, 0xd9,
        ];
        let jpeg = [&[0xff, 0xd8][..], &app0, &rest].concat();
        let icc: Vec<u8> = (0..ICC_SEGMENT * 2 + 10).map(|i| i as u8).collect();
        let out = jpeg_with_icc(&jpeg, &icc).unwrap();
        assert_eq!(out[..8], jpeg[..8]);
        let segments = jpeg_segments(&out);
        let parts: Vec<_> = segments
            .iter()
            .filter(|s| s.1 == 0xe2)
            .map(|s| (s.2[12], s.2[13], s.2[14..].to_vec()))
            .collect();
        assert_eq!(segments[1].1, 0xe2, "right after APP0");
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].2.len(), ICC_SEGMENT);
        assert!(parts.iter().all(|p| p.1 == 3));
        assert!(out.ends_with(&rest));
        // Already profiled: left alone.
        assert_eq!(jpeg_with_icc(&out, &icc), None);

        // Parts in any order come back in sequence; a missing part means no profile.
        let mut shuffled = parts.clone();
        shuffled.swap(0, 2);
        assert_eq!(jpeg_icc(shuffled.clone()), Some(icc));
        shuffled.pop();
        assert_eq!(jpeg_icc(shuffled), None);
        assert_eq!(jpeg_icc(Vec::new()), None);
    }

    #[test]
    fn png_profile_is_inflated() {
        use std::io::Write;
        let icc = b"profile bytes".repeat(20);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&icc).unwrap();
        let chunk = [b"ICC Profile\0\0".as_slice(), &z.finish().unwrap()].concat();
        assert_eq!(png_icc(&chunk), Some(icc));
        assert_eq!(png_icc(b"no nul"), None);
    }
}
