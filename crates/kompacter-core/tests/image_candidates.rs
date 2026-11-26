//! Real still-image encodes. The Settings demo also covers small, readable text.

use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::AtomicBool;

use kompacter_core::{Engine, Error, Ffmpeg, ImageFormat, MediaKind, Options, Outcome};

fn ffmpeg() -> Command {
    let mut cmd = Command::new(Ffmpeg::default().ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    cmd
}

fn have_ffmpeg() -> bool {
    ffmpeg()
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn checked(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn gen(args: &[&str], dest: &Path) {
    checked(ffmpeg().args(args).arg(dest).output().unwrap());
}

fn compress(src: &Path, target: u64, format: ImageFormat) -> Result<Outcome, Error> {
    let mut opts = Options::new(target);
    opts.image_format = format;
    Engine::default().compress(src, &opts, &mut |_| {}, &AtomicBool::new(false))
}

fn pixels(path: &Path, format: &str) -> Vec<u8> {
    checked(
        ffmpeg()
            .arg("-i")
            .arg(path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", format, "-"])
            .output()
            .unwrap(),
    )
}

#[test]
fn settings_frame_fits_without_skipping_native_resolution() {
    if !have_ffmpeg() {
        return;
    }
    let demo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/media/settings.gif");
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("settings.png");
    checked(
        ffmpeg()
            .arg("-i")
            .arg(demo)
            .args([
                "-vf",
                r"select=eq(n\,40)",
                "-frames:v",
                "1",
                "-pix_fmt",
                "rgb24",
            ])
            .arg(&input)
            .output()
            .unwrap(),
    );

    // At 24 KiB both 960×720/q10 and 720×540/q6 fit. The failed
    // 720×540/q3 candidate used to rule out 960×720/q10.
    let target = 24_576;
    let out = compress(&input, target, ImageFormat::Auto).unwrap();
    let info = Engine::default().probe(&out.path).unwrap().video.unwrap();
    assert!(!out.untouched);
    assert_eq!(out.path.extension().unwrap(), "jpg");
    assert!(out.output_bytes <= target);
    assert_eq!((info.width, info.height), (960, 720));
}

#[test]
fn native_lossless_preserves_odd_dimensions_and_decoded_pixels() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("odd.png");
    gen(
        &["-f", "lavfi", "-i", "testsrc=size=129x97", "-frames:v", "1"],
        &src,
    );
    let target = std::fs::metadata(&src).unwrap().len() - 1;
    let out = compress(&src, target, ImageFormat::Auto).unwrap();
    let info = Engine::default().probe(&out.path).unwrap().video.unwrap();
    assert_eq!(out.path.extension().unwrap(), "webp");
    assert_eq!((info.width, info.height), (129, 97));
    assert_eq!(pixels(&out.path, "rgb24"), pixels(&src, "rgb24"));
}

#[test]
fn jpeg_preference_retains_alpha() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("alpha.png");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=256x128",
            "-vf",
            "format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(lt(X,128),0,128)'",
            "-frames:v",
            "1",
            "-compression_level",
            "0",
        ],
        &src,
    );
    let src_size = std::fs::metadata(&src).unwrap().len();
    let out = compress(&src, src_size - 1, ImageFormat::Jpeg).unwrap();
    let info = Engine::default().probe(&out.path).unwrap();
    assert_eq!(out.path.extension().unwrap(), "webp");
    assert!(info.has_alpha);
    let info = info.video.unwrap();
    assert_eq!((info.width, info.height), (256, 128));
    let alpha = |path: &Path| {
        pixels(path, "rgba")
            .into_iter()
            .skip(3)
            .step_by(4)
            .collect::<Vec<_>>()
    };
    assert_eq!(alpha(&out.path), alpha(&src));
}

fn have_encoder(name: &str) -> bool {
    ffmpeg()
        .args(["-h", &format!("encoder={name}")])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("Encoder {name}")))
}

#[test]
fn animated_png_becomes_animated_webp() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("animated.png");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=3:duration=1",
            "-f",
            "apng",
        ],
        &src,
    );
    let info = Engine::default().probe(&src).unwrap();
    assert_eq!(info.kind, MediaKind::Image);
    let out = compress(&src, info.size_bytes, ImageFormat::Auto).unwrap();
    assert!(out.untouched);
    assert_eq!(out.path, src);

    let out = compress(&src, info.size_bytes - 1, ImageFormat::Auto).unwrap();
    assert!(out.output_bytes < info.size_bytes);
    let data = std::fs::read(&out.path).unwrap();
    if have_encoder("libwebp_anim") {
        assert_eq!(out.path.extension().unwrap(), "webp");
        assert!(chunk(&data, b"ANIM").is_some(), "not an animated WebP");
    } else {
        // Its first frame, as a still.
        assert!(Engine::default().probe(&out.path).unwrap().video.is_some());
    }
}

// --- Colour metadata: kept where possible, never a reason to refuse. ---

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// The PNG with an extra chunk right after IHDR.
fn png_with(png: &[u8], kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut c = (payload.len() as u32).to_be_bytes().to_vec();
    let mut body = kind.to_vec();
    body.extend(payload);
    c.extend(&body);
    c.extend(crc32(&body).to_be_bytes());
    [&png[..33], &c, &png[33..]].concat()
}

fn iccp(icc: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(icc).unwrap();
    [b"test\0\0".as_slice(), &z.finish().unwrap()].concat()
}

/// Stand-in profile bytes; nothing here interprets them.
fn profile(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

/// The JPEG with `icc` split into numbered APP2 segments of at most `per` bytes.
fn jpeg_with_icc(jpeg: &[u8], icc: &[u8], per: usize) -> Vec<u8> {
    let parts: Vec<&[u8]> = icc.chunks(per).collect();
    let mut out = jpeg[..2].to_vec();
    for (i, part) in parts.iter().enumerate() {
        out.extend([0xff, 0xe2]);
        out.extend(((part.len() + 16) as u16).to_be_bytes());
        out.extend(b"ICC_PROFILE\0");
        out.extend([i as u8 + 1, parts.len() as u8]);
        out.extend(*part);
    }
    out.extend(&jpeg[2..]);
    out
}

/// A WebP chunk's payload.
fn chunk<'a>(webp: &'a [u8], id: &[u8; 4]) -> Option<&'a [u8]> {
    assert_eq!(&webp[..4], b"RIFF");
    assert_eq!(&webp[8..12], b"WEBP");
    let riff = u32::from_le_bytes(webp[4..8].try_into().unwrap()) as usize;
    assert_eq!(riff + 8, webp.len(), "RIFF size");
    let mut i = 12;
    while i + 8 <= webp.len() {
        let len = u32::from_le_bytes(webp[i + 4..i + 8].try_into().unwrap()) as usize;
        if &webp[i..i + 4] == id {
            return Some(&webp[i + 8..i + 8 + len]);
        }
        i += 8 + len + len % 2;
    }
    None
}

/// A JPEG's ICC profile, its APP2 parts joined in order.
fn jpeg_icc(jpeg: &[u8]) -> Option<Vec<u8>> {
    let mut parts = Vec::new();
    let mut i = 2;
    while jpeg[i] == 0xff && !matches!(jpeg[i + 1], 0xda | 0xd9) {
        let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        let seg = &jpeg[i + 4..i + 2 + len];
        if jpeg[i + 1] == 0xe2 && seg.starts_with(b"ICC_PROFILE\0") {
            parts.push((seg[12], seg[14..].to_vec()));
        }
        i += 2 + len;
    }
    parts.sort();
    (!parts.is_empty()).then(|| parts.into_iter().flat_map(|(_, p)| p).collect())
}

/// One noisy test frame (`filter` is appended to the noise) with extra output flags.
fn noisy(size: &str, filter: &str, out_args: &[&str], dest: &Path) {
    let vf = format!("noise=alls=40:allf=t{filter}");
    let input = format!("testsrc=size={size}");
    let mut a = vec!["-f", "lavfi", "-i", &input, "-vf", &vf, "-frames:v", "1"];
    a.extend(out_args);
    gen(&a, dest);
}

#[test]
fn png_with_only_gamma_and_chromaticities_compresses() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.png");
    noisy("320x240", "", &[], &plain);
    let mut chrm = Vec::new();
    for v in [31270u32, 32900, 64000, 33000, 30000, 60000, 15000, 6000] {
        chrm.extend(v.to_be_bytes());
    }
    let png = png_with(
        &std::fs::read(&plain).unwrap(),
        b"gAMA",
        &45455u32.to_be_bytes(),
    );
    let src = dir.path().join("gamma.png");
    std::fs::write(&src, png_with(&png, b"cHRM", &chrm)).unwrap();

    let target = std::fs::metadata(&src).unwrap().len() / 3;
    let out = compress(&src, target, ImageFormat::Auto).unwrap();
    assert!(out.output_bytes <= target);
    assert!(Engine::default().probe(&out.path).unwrap().video.is_some());
}

#[test]
fn transparent_png_with_profile_becomes_webp_with_profile() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.png");
    noisy(
        "256x128",
        ",format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(lt(X,128),0,200)'",
        &[],
        &plain,
    );
    let icc = profile(3001);
    let src = dir.path().join("screenshot.png");
    std::fs::write(
        &src,
        png_with(&std::fs::read(&plain).unwrap(), b"iCCP", &iccp(&icc)),
    )
    .unwrap();

    let target = std::fs::metadata(&src).unwrap().len() / 2;
    let out = compress(&src, target, ImageFormat::Auto).unwrap();
    assert_eq!(out.path.extension().unwrap(), "webp");
    assert!(out.output_bytes <= target);
    assert_eq!(
        std::fs::metadata(&out.path).unwrap().len(),
        out.output_bytes
    );
    let data = std::fs::read(&out.path).unwrap();
    assert_eq!(chunk(&data, b"ICCP"), Some(icc.as_slice()));
    assert_eq!(
        chunk(&data, b"VP8X").unwrap()[0] & 0x30,
        0x30,
        "ICC + alpha"
    );
    let info = Engine::default().probe(&out.path).unwrap();
    assert!(info.has_alpha);
    assert_eq!(pixels(&out.path, "rgba").len(), 256 * 128 * 4);
}

#[test]
fn lossless_webp_keeps_the_profile() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.png");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=129x97",
            "-frames:v",
            "1",
            "-compression_level",
            "0",
        ],
        &plain,
    );
    let icc = profile(1200);
    let src = dir.path().join("profiled.png");
    std::fs::write(
        &src,
        png_with(&std::fs::read(&plain).unwrap(), b"iCCP", &iccp(&icc)),
    )
    .unwrap();

    let target = std::fs::metadata(&src).unwrap().len() - 1;
    let out = compress(&src, target, ImageFormat::Auto).unwrap();
    assert_eq!(out.path.extension().unwrap(), "webp");
    // VP8L turned into the extended format: its canvas is the picture's size.
    let data = std::fs::read(&out.path).unwrap();
    assert_eq!(chunk(&data, b"ICCP"), Some(icc.as_slice()));
    let vp8x = chunk(&data, b"VP8X").unwrap();
    assert_eq!(vp8x[0] & 0x20, 0x20);
    assert_eq!(&vp8x[4..10], [128, 0, 0, 96, 0, 0]);
    assert!(chunk(&data, b"VP8L").is_some());
    assert_eq!(pixels(&out.path, "rgb24"), pixels(&src, "rgb24"));
}

/// A noisy JPEG photo with `icc` in APP2 segments of `per` bytes.
fn profiled_jpeg(dir: &Path, icc: &[u8], per: usize) -> std::path::PathBuf {
    let plain = dir.join("plain.jpg");
    noisy("320x240", "", &["-q:v", "2"], &plain);
    let src = dir.join("photo.jpg");
    std::fs::write(
        &src,
        jpeg_with_icc(&std::fs::read(&plain).unwrap(), icc, per),
    )
    .unwrap();
    src
}

#[test]
fn photo_with_profile_keeps_it_as_webp() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let icc = profile(3000);
    let src = profiled_jpeg(dir.path(), &icc, 1000);
    let target = std::fs::metadata(&src).unwrap().len() / 2;
    let out = compress(&src, target, ImageFormat::Webp).unwrap();
    assert_eq!(out.path.extension().unwrap(), "webp");
    assert!(out.output_bytes <= target);
    // A lossy VP8 file, now extended with the profile, still decodes.
    let data = std::fs::read(&out.path).unwrap();
    assert_eq!(chunk(&data, b"ICCP"), Some(icc.as_slice()));
    let vp8x = chunk(&data, b"VP8X").unwrap();
    assert_eq!(vp8x[0], 0x20);
    assert!(chunk(&data, b"VP8 ").is_some());
    let info = Engine::default().probe(&out.path).unwrap().video.unwrap();
    let (w, h) = (info.width as usize, info.height as usize);
    assert_eq!(
        u32::from_le_bytes([vp8x[4], vp8x[5], vp8x[6], 0]) as usize + 1,
        w
    );
    assert_eq!(
        u32::from_le_bytes([vp8x[7], vp8x[8], vp8x[9], 0]) as usize + 1,
        h
    );
    assert_eq!(pixels(&out.path, "rgb24").len(), w * h * 3);
}

#[test]
fn photo_with_large_profile_keeps_it_as_jpeg() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // More than two APP2 segments' worth, stored in an odd split to be reassembled.
    let icc = profile(150_000);
    let src = profiled_jpeg(dir.path(), &icc, 60_000);
    let target = std::fs::metadata(&src).unwrap().len() - 1;
    let out = compress(&src, target, ImageFormat::Auto).unwrap();
    assert_eq!(out.path.extension().unwrap(), "jpg");
    assert!(out.output_bytes <= target);
    let data = std::fs::read(&out.path).unwrap();
    assert_eq!(jpeg_icc(&data), Some(icc));
    assert!(Engine::default().probe(&out.path).unwrap().video.is_some());
}

#[test]
fn hdr_still_compresses() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hdr.png");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180",
            "-frames:v",
            "1",
            "-vf",
            "noise=alls=30:allf=t,format=rgb48be,\
             setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=gbr",
            "-color_primaries",
            "bt2020",
            "-color_trc",
            "smpte2084",
        ],
        &src,
    );
    let engine = Engine::default();
    assert!(engine.probe(&src).unwrap().video.unwrap().hdr.is_some());
    let target = std::fs::metadata(&src).unwrap().len() / 4;
    let out = compress(&src, target, ImageFormat::Auto).unwrap();
    assert!(out.output_bytes <= target);
    let info = engine.probe(&out.path).unwrap().video.unwrap();
    assert_eq!((info.width, info.height), (320, 180));
    if engine.can_tonemap() {
        assert_eq!(info.hdr, None);
    }
}
