//! Real encodes against tiny generated files. Skipped when ffmpeg isn't installed.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use kompacter_core::size::{KB, MB};
use kompacter_core::{Engine, MediaKind, Mode, Options, Trim};

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

fn gen(args: &[&str], out: &Path) {
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .arg(out)
        .status()
        .unwrap()
        .success();
    assert!(ok, "failed to generate {}", out.display());
}

fn run(engine: &Engine, input: &Path, opts: &Options) -> kompacter_core::Outcome {
    engine
        .compress(input, opts, &mut |_| {}, &AtomicBool::new(false))
        .unwrap()
}

#[test]
fn video_fits_target_and_trim_works() {
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clip.mp4");
    // 8 s of noisy 720p video so it compresses badly on purpose.
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=30:duration=8",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=8",
            "-vf",
            "noise=alls=40:allf=t",
            "-c:v",
            "libx264",
            "-crf",
            "10",
            "-preset",
            "ultrafast",
            "-g",
            "30",
            "-c:a",
            "aac",
            "-shortest",
        ],
        &src,
    );

    let engine = Engine::default();
    let info = engine.probe(&src).unwrap();
    assert_eq!(info.kind, MediaKind::Video);

    let target = 600 * KB;
    assert!(
        info.size_bytes > target,
        "source should start over the limit"
    );
    let mut opts = Options::new(target);
    opts.out_dir = Some(dir.path().join("out"));
    let res = run(&engine, &src, &opts);
    assert!(
        res.fits && res.output_bytes <= target,
        "got {} bytes",
        res.output_bytes
    );

    let thumbs = engine.thumbnails(&info, 8, 48).unwrap();
    assert!(thumbs.len() >= 6, "got {} thumbnails", thumbs.len());
    assert!(
        thumbs.iter().all(|t| t.starts_with(&[0xFF, 0xD8])),
        "not JPEGs"
    );

    opts.trim = Some(Trim::new(2.0, 5.0).unwrap());
    opts.mode = Mode::KeepQuality;
    let cut = run(&engine, &src, &opts);
    let cut_info = engine.probe(&cut.path).unwrap();
    assert!(cut_info.duration < 4.5, "cut is {}s", cut_info.duration);
}

#[test]
fn image_audio_and_folder() {
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let album = dir.path().join("album");
    std::fs::create_dir(&album).unwrap();
    let img = album.join("photo.png");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=2000x1500",
            "-vf",
            "noise=alls=60",
            "-frames:v",
            "1",
        ],
        &img,
    );
    let song = album.join("song.wav");
    gen(
        &["-f", "lavfi", "-i", "sine=frequency=330:duration=20"],
        &song,
    );
    std::fs::write(album.join("notes.txt"), "hello ".repeat(5000)).unwrap();

    let engine = Engine::default();
    let out = Some(dir.path().join("out"));

    let mut o = Options::new(300 * KB);
    o.out_dir = out.clone();
    let r = run(&engine, &img, &o);
    assert!(r.fits && r.path.extension().unwrap() == "jpg");

    let r = run(&engine, &song, &o);
    assert!(r.fits && r.path.extension().unwrap() == "mp3");

    let mut o = Options::new(MB / 2);
    o.out_dir = out;
    let r = run(&engine, &album, &o);
    assert!(r.fits && r.path.extension().unwrap() == "zip", "{r:?}");
}
