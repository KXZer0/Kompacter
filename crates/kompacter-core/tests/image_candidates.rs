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

#[test]
fn animated_png_stays_intact_or_returns_an_error() {
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
    assert!(matches!(
        compress(&src, info.size_bytes - 1, ImageFormat::Auto),
        Err(Error::Unsupported(_))
    ));
}
