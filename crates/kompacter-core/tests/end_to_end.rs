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
    assert!(thumbs.rgb.len() >= 6, "got {} thumbnails", thumbs.rgb.len());
    assert_eq!(thumbs.height, 48);
    assert_eq!(
        thumbs.rgb[0].len(),
        (thumbs.width * thumbs.height * 3) as usize
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

#[test]
fn long_video_splits_into_parts_that_each_fit() {
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("long.mp4");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=30:duration=24",
            "-vf",
            "noise=alls=30:allf=t",
            "-c:v",
            "libx264",
            "-crf",
            "16",
            "-preset",
            "ultrafast",
        ],
        &src,
    );
    let engine = Engine::default();
    let info = engine.probe(&src).unwrap();
    let target = 700 * KB;
    let est = engine.estimate(&info, &Options::new(target)).unwrap();
    let n = est.parts.expect("should suggest splitting");
    assert!((2..=10).contains(&n), "parts {n}");

    let mut opts = Options::new(target);
    opts.split = true;
    opts.gpu = false;
    let res = run(&engine, &src, &opts);
    assert_eq!(res.parts.len(), n as usize);
    let mut total = 0.0;
    for p in &res.parts {
        assert!(
            std::fs::metadata(p).unwrap().len() <= target,
            "{} too big",
            p.display()
        );
        assert!(p.to_string_lossy().contains("(part "), "{}", p.display());
        total += engine.probe(p).unwrap().duration;
    }
    assert!((total - 24.0).abs() < 1.5, "parts add up to {total} s");
}

#[test]
fn big_folder_splits_into_zips_that_each_fit() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("docs");
    std::fs::create_dir(&folder).unwrap();
    // Three files that don't compress (pseudo-random bytes), 300 KB each.
    let mut x: u64 = 12345;
    for name in ["a.bin", "b.bin", "c.bin"] {
        let data: Vec<u8> = (0..300 * 1024)
            .map(|_| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (x >> 56) as u8
            })
            .collect();
        std::fs::write(folder.join(name), data).unwrap();
    }
    let engine = Engine::default();
    let target = 700 * KB;
    let mut opts = Options::new(target);
    opts.split = true;
    let res = run(&engine, &folder, &opts);
    assert!(res.parts.len() >= 2, "got {} parts", res.parts.len());
    for p in &res.parts {
        assert!(std::fs::metadata(p).unwrap().len() <= target);
    }
}

#[test]
fn sideways_phone_photo_comes_out_upright() {
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.jpg");
    // Stored landscape (1600×800), noisy so it has to be shrunk.
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1600x800:rate=1:duration=1",
            "-vf",
            "noise=alls=60:allf=t",
            "-frames:v",
            "1",
            "-q:v",
            "2",
        ],
        &plain,
    );
    // Add an EXIF block saying "turn 90° clockwise to view" (orientation 6), like phones do.
    let jpeg = std::fs::read(&plain).unwrap();
    let mut tiff = b"II".to_vec();
    tiff.extend(42u16.to_le_bytes());
    tiff.extend(8u32.to_le_bytes());
    tiff.extend(1u16.to_le_bytes());
    tiff.extend(0x0112u16.to_le_bytes());
    tiff.extend(3u16.to_le_bytes());
    tiff.extend(1u32.to_le_bytes());
    tiff.extend(6u16.to_le_bytes());
    tiff.extend([0, 0]);
    tiff.extend(0u32.to_le_bytes());
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend(tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend([0xFF, 0xE1]);
    out.extend(((app1.len() + 2) as u16).to_be_bytes());
    out.extend(app1);
    out.extend(&jpeg[2..]);
    let src = dir.path().join("phone.jpg");
    std::fs::write(&src, out).unwrap();

    let engine = Engine::default();
    let size = std::fs::metadata(&src).unwrap().len();
    let res = run(&engine, &src, &Options::new(size / 3));
    let v = engine.probe(&res.path).unwrap().video.unwrap();
    assert!(
        v.height > v.width,
        "still sideways: {}×{}",
        v.width,
        v.height
    );
}
