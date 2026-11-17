//! Real encodes against tiny generated files. Skipped when ffmpeg isn't installed.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kompacter_core::size::{KB, MB};
use kompacter_core::{Engine, Error, Ffmpeg, MediaKind, Mode, Options, Trim};

fn have_ffmpeg() -> bool {
    Command::new(Ffmpeg::default().ffmpeg)
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn gen(args: &[&str], out: &Path) {
    let ok = Command::new(Ffmpeg::default().ffmpeg)
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
fn cancel_ffmpeg_between_progress_reports() {
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let args = [
        "-stats_period",
        "10",
        "-re",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x64:rate=10:duration=10",
        "-f",
        "null",
        "-",
    ]
    .map(Into::into);
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let cancel = &cancel;
        let trigger = scope.spawn(move || {
            // Give ffmpeg time to start, then cancel well before its next report.
            std::thread::sleep(Duration::from_secs(1));
            let requested_at = Instant::now();
            cancel.store(true, Ordering::Relaxed);
            requested_at
        });
        let result = Ffmpeg::default().run(&args, 10.0, &mut |_| {}, cancel);
        let finished_at = Instant::now();
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(finished_at.duration_since(trigger.join().unwrap()) < Duration::from_secs(3));
    });
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

fn decoded_luma_frames(path: &Path) -> Vec<u8> {
    let output = Command::new(Ffmpeg::default().ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-vf",
            "scale=1:1",
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "gray",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn trim_reencode_preserves_vfr_boundaries_and_audio_cues() {
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("boundaries.mp4");
    // The luma identifies each source frame. The first audio cue is inside the cut and
    // the second one is outside. There is only one keyframe, at the start of the file.
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=size=64x64:rate=10:duration=5",
            "-f",
            "lavfi",
            "-i",
            r"aevalsrc=if(between(t\,2.27\,2.37)+between(t\,3.58\,3.71)\,0.5*sin(2*PI*660*t)\,0):s=48000:d=5",
            "-vf",
            "geq=lum='32+4*N':cb=128:cr=128,select='not(mod(n,5))+not(mod(n+1,5))+not(mod(n+3,5))'",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "100",
            "-sc_threshold",
            "0",
            "-c:a",
            "aac",
        ],
        &src,
    );

    let engine = Engine::default();
    let mut opts = Options::new(250 * KB);
    opts.gpu = false;
    opts.trim = Some(Trim::new(2.13, 3.52).unwrap());
    opts.out_dir = Some(dir.path().join("fit"));
    let encoded = run(&engine, &src, &opts);
    let src_frames = decoded_luma_frames(&src);
    let fit_frames = decoded_luma_frames(&encoded.path);
    let expected = &src_frames[13..22]; // source times 2.200 through 3.500
    assert_eq!(fit_frames.len(), expected.len(), "{fit_frames:?}");
    for (actual, expected) in fit_frames.iter().zip(expected) {
        assert!(
            actual.abs_diff(*expected) <= 2,
            "frame luma {actual} != {expected}"
        );
    }

    let probe = Command::new(Ffmpeg::default().ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=best_effort_timestamp_time:format=duration",
            "-of",
            "json",
        ])
        .arg(&encoded.path)
        .output()
        .unwrap();
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let timestamps: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    let frames = timestamps["frames"].as_array().unwrap();
    let pts = |index: usize| {
        frames[index]["best_effort_timestamp_time"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap()
    };
    assert!((pts(0) - 0.070).abs() < 0.002, "first frame at {}", pts(0));
    assert!((pts(8) - 1.370).abs() < 0.002, "last frame at {}", pts(8));
    let duration: f64 = timestamps["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((duration - 1.390).abs() < 0.002, "container is {duration}s");

    let audio = Command::new(Ffmpeg::default().ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&encoded.path)
        .args([
            "-map", "0:a:0", "-ac", "1", "-ar", "48000", "-f", "s16le", "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(
        audio.status.success(),
        "{}",
        String::from_utf8_lossy(&audio.stderr)
    );
    let samples: Vec<_> = audio
        .stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&sample| i16::from_le_bytes(sample))
        .collect();
    let cue = |sample: &i16| sample.unsigned_abs() > 3000;
    let onset = samples.iter().position(cue).unwrap() as f64 / 48_000.0;
    let end = samples.iter().rposition(cue).unwrap() as f64 / 48_000.0;
    assert!((onset - 0.140).abs() < 0.005, "audio cue starts at {onset}");
    assert!((end - 0.240).abs() < 0.005, "audio cue ends at {end}");
    let audio_len = samples.len() as f64 / 48_000.0;
    assert!(
        (audio_len - 1.390).abs() <= 1024.0 / 48_000.0,
        "audio is {audio_len}s"
    );

    opts.mode = Mode::KeepQuality;
    opts.out_dir = Some(dir.path().join("copy"));
    let copied = run(&engine, &src, &opts);
    let copy_frames = decoded_luma_frames(&copied.path);
    // Stream copy starts at the preceding keyframe and preserves the original packets.
    assert_eq!(copy_frames, &src_frames[..22]);
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

#[test]
fn h265_and_av1_hit_the_target() {
    use kompacter_core::VideoCodec;
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clip.mp4");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=854x480:rate=30:duration=4",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=4",
            "-vf",
            "noise=alls=30:allf=t",
            "-c:v",
            "libx264",
            "-crf",
            "12",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-shortest",
        ],
        &src,
    );
    let engine = Engine::default();
    for codec in [VideoCodec::H265, VideoCodec::Av1] {
        let target = 400 * KB;
        let mut opts = Options::new(target);
        opts.gpu = false;
        opts.video_codec = codec;
        let res = run(&engine, &src, &opts);
        assert!(res.fits, "{codec:?}: {} bytes", res.output_bytes);
        let v = engine.probe(&res.path).unwrap().video.unwrap();
        let want = if codec == VideoCodec::H265 {
            "hevc"
        } else {
            "av1"
        };
        assert_eq!(v.codec, want);
    }
}

#[test]
fn seven_z_beats_zip_on_documents() {
    use kompacter_core::ArchiveFormat;
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("notes");
    std::fs::create_dir(&folder).unwrap();
    // Text compresses well: 600 KB of slightly varied lines.
    let text: String = (0..12_000)
        .map(|i| format!("line {i}: the quick brown fox {}\n", i % 97))
        .collect();
    std::fs::write(folder.join("a.txt"), &text).unwrap();
    std::fs::write(folder.join("b.txt"), text.replace("fox", "dog")).unwrap();
    let engine = Engine::default();
    // Just under the folder's size, so it has to be packed (and easily fits once packed).
    let limit = 1024 * KB;
    let size = |archive| {
        let mut o = Options::new(limit);
        o.archive = archive;
        let res = run(&engine, &folder, &o);
        assert!(res.fits);
        res.output_bytes
    };
    let (zip, sevenz) = (size(ArchiveFormat::Zip), size(ArchiveFormat::SevenZ));
    assert!(sevenz < zip, "7z {sevenz} vs zip {zip}");
}

/// A big photo as raw RGB pixels (noisy, so it doesn't compress on its own).
fn noisy_rgb(w: usize, h: usize) -> Vec<u8> {
    let mut x: u64 = 99;
    (0..w * h * 3)
        .map(|i| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((i % 256) as u8 / 2).wrapping_add((x >> 59) as u8)
        })
        .collect()
}

#[test]
fn pdf_photos_are_shrunk_and_it_stays_a_pdf() {
    use lopdf::{dictionary, Document, Object, Stream};
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("scan.pdf");
    let (w, h) = (2400, 1800);
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut img = Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image",
            "Width" => w as i64, "Height" => h as i64,
            "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
        },
        noisy_rgb(w, h),
    );
    img.compress().unwrap();
    let img_id = doc.add_object(img);
    let content = doc.add_object(Stream::new(
        dictionary! {},
        b"q 600 0 0 450 0 0 cm /Im1 Do Q".to_vec(),
    ));
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content,
        "MediaBox" => vec![0.into(), 0.into(), 600.into(), 450.into()],
        "Resources" => dictionary! { "XObject" => dictionary! { "Im1" => img_id } },
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1,
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    doc.save(&src).unwrap();

    let size = std::fs::metadata(&src).unwrap().len();
    let engine = Engine::default();
    let res = run(&engine, &src, &Options::new(size / 4));
    assert!(res.fits, "{} of {size}", res.output_bytes);
    assert_eq!(res.path.extension().unwrap(), "pdf");
    let out = Document::load(&res.path).unwrap();
    assert_eq!(out.get_pages().len(), 1);
}

#[test]
fn office_pictures_are_shrunk_and_it_stays_a_document() {
    use std::io::Write;
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let photo = dir.path().join("photo.jpg");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=3000x2000:rate=1:duration=1",
            "-vf",
            "noise=alls=60:allf=t",
            "-frames:v",
            "1",
            "-q:v",
            "2",
        ],
        &photo,
    );
    let src = dir.path().join("report.docx");
    let mut z = zip::ZipWriter::new(std::fs::File::create(&src).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    z.start_file("[Content_Types].xml", opts).unwrap();
    z.write_all(b"<Types/>").unwrap();
    z.start_file("word/document.xml", opts).unwrap();
    z.write_all(b"<w:document/>").unwrap();
    z.start_file("word/media/image1.jpeg", opts).unwrap();
    z.write_all(&std::fs::read(&photo).unwrap()).unwrap();
    z.finish().unwrap();

    let size = std::fs::metadata(&src).unwrap().len();
    let engine = Engine::default();
    let res = run(&engine, &src, &Options::new(size / 3));
    assert!(res.fits, "{} of {size}", res.output_bytes);
    assert_eq!(res.path.extension().unwrap(), "docx");
    let mut out = zip::ZipArchive::new(std::fs::File::open(&res.path).unwrap()).unwrap();
    assert!(out.by_name("word/document.xml").is_ok());
    assert!(out.by_name("word/media/image1.jpeg").is_ok());
}

#[test]
fn glb_textures_are_shrunk_and_it_stays_a_model() {
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let tex = dir.path().join("tex.png");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=2048x2048:rate=1:duration=1",
            "-vf",
            "noise=alls=40:allf=t",
            "-frames:v",
            "1",
        ],
        &tex,
    );
    let png = std::fs::read(&tex).unwrap();
    // One triangle (3 float positions) then the texture, in one binary chunk.
    let positions: Vec<u8> = [0f32, 0., 0., 1., 0., 0., 0., 1., 0.]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let mut bin = positions.clone();
    let tex_off = bin.len();
    bin.extend(&png);
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let json = serde_json::json!({
        "asset": {"version": "2.0"},
        "buffers": [{"byteLength": bin.len()}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": positions.len()},
            {"buffer": 0, "byteOffset": tex_off, "byteLength": png.len()}
        ],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                       "min": [0, 0, 0], "max": [1, 1, 0]}],
        "images": [{"bufferView": 1, "mimeType": "image/png"}],
        "textures": [{"source": 0}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}]
    });
    let mut text = serde_json::to_vec(&json).unwrap();
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    let mut glb = Vec::new();
    glb.extend(b"glTF");
    glb.extend(2u32.to_le_bytes());
    glb.extend(((12 + 8 + text.len() + 8 + bin.len()) as u32).to_le_bytes());
    glb.extend((text.len() as u32).to_le_bytes());
    glb.extend(b"JSON");
    glb.extend(&text);
    glb.extend((bin.len() as u32).to_le_bytes());
    glb.extend(b"BIN\0");
    glb.extend(&bin);
    let src = dir.path().join("chair.glb");
    std::fs::write(&src, &glb).unwrap();

    let size = glb.len() as u64;
    let res = run(&Engine::default(), &src, &Options::new(size / 4));
    assert!(res.fits, "{} of {size}", res.output_bytes);
    assert_eq!(res.path.extension().unwrap(), "glb");

    // Still a valid GLB: header length, JSON, views inside the binary chunk, mesh intact.
    let out = std::fs::read(&res.path).unwrap();
    let u32_at = |at: usize| u32::from_le_bytes(out[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!(&out[0..4], b"glTF");
    assert_eq!(u32_at(8), out.len());
    let json_len = u32_at(12);
    let json: serde_json::Value = serde_json::from_slice(&out[20..20 + json_len]).unwrap();
    let bin_at = 20 + json_len;
    assert_eq!(&out[bin_at + 4..bin_at + 8], b"BIN\0");
    let bin = &out[bin_at + 8..bin_at + 8 + u32_at(bin_at)];
    assert_eq!(
        json["buffers"][0]["byteLength"].as_u64().unwrap() as usize,
        bin.len()
    );
    assert_eq!(json["images"][0]["mimeType"], "image/jpeg");
    let view = |i: usize| {
        let v = &json["bufferViews"][i];
        let off = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        &bin[off..off + v["byteLength"].as_u64().unwrap() as usize]
    };
    assert_eq!(view(0), &positions[..]);
    assert_eq!(&view(1)[..2], &[0xFF, 0xD8]); // a JPEG
}

#[test]
fn folder_preview_shows_the_tree_with_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Memes");
    std::fs::create_dir_all(root.join("clips/old")).unwrap();
    std::fs::write(root.join("a.mp4"), vec![1u8; 300_000]).unwrap();
    std::fs::write(root.join("clips/b.mp4"), vec![1u8; 200_000]).unwrap();
    std::fs::write(root.join("clips/old/c.png"), vec![1u8; 100_000]).unwrap();
    std::fs::write(root.join("notes.txt"), vec![b'x'; 1_000]).unwrap();
    let rows = Engine::default().folder_preview(&root, 300 * KB).unwrap();
    let names: Vec<(u32, &str)> = rows.iter().map(|r| (r.depth, r.name.as_str())).collect();
    assert_eq!(
        names,
        vec![
            (0, "Memes"),
            (1, "a.mp4"),
            (1, "clips"),
            (2, "b.mp4"),
            (2, "old"),
            (3, "c.png"),
            (1, "notes.txt"),
        ]
    );
    assert_eq!(rows[0].before, 601_000);
    assert_eq!(rows[2].before, 300_000); // clips = b + old/c
    assert!(rows[0].after < 300 * KB, "estimate {} fits", rows[0].after);
    assert_eq!(rows[6].after, 1_000); // text isn't shrunk
}

#[test]
fn short_clip_becomes_a_gif_that_fits() {
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clip.mp4");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=30:duration=6",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ],
        &src,
    );
    let engine = Engine::new(Default::default());
    let mut opts = Options::new(MB);
    opts.mode = Mode::Gif;
    opts.trim = Some(Trim::new(1.0, 5.0).unwrap());
    opts.out_dir = Some(dir.path().join("out"));
    let out = run(&engine, &src, &opts);
    assert!(out.fits, "{} bytes", out.output_bytes);
    assert_eq!(out.path.extension().unwrap(), "gif");
    let info = engine.probe(&out.path).unwrap();
    let v = info.video.expect("a GIF has video");
    assert!(v.width <= 640 && v.width >= 120, "width {}", v.width);
    assert!((3.5..=4.5).contains(&info.duration), "{} s", info.duration);

    // Too long for a GIF: says so instead of making a bad one.
    let mut long = Options::new(MB);
    long.mode = Mode::Gif;
    long.out_dir = opts.out_dir.clone();
    let src_long = dir.path().join("long.mp4");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=10:duration=40",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ],
        &src_long,
    );
    let err = engine
        .compress(&src_long, &long, &mut |_| {}, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(err, kompacter_core::error::Error::TooLong { .. }));
}

#[test]
fn portrait_video_stays_upright() {
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let land = dir.path().join("land.mp4");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=30:duration=4",
            "-vf",
            "noise=alls=40:allf=t",
            "-c:v",
            "libx264",
            "-crf",
            "10",
            "-preset",
            "ultrafast",
        ],
        &land,
    );
    // Same pixels, flagged as "rotate a quarter turn", like a phone held upright.
    let src = dir.path().join("portrait.mp4");
    let flagged = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-display_rotation", "90", "-i"])
        .arg(&land)
        .args(["-c", "copy"])
        .arg(&src)
        .status()
        .is_ok_and(|s| s.success());
    if !flagged {
        return eprintln!("this ffmpeg can't write a rotation flag, skipping");
    }

    let engine = Engine::default();
    let v = engine.probe(&src).unwrap().video.unwrap();
    assert_eq!((v.width, v.height), (720, 1280));

    // Small enough that the plan has to scale down, which is where it used to squash.
    let mut opts = Options::new(300 * KB);
    opts.out_dir = Some(dir.path().join("out"));
    let res = run(&engine, &src, &opts);
    assert!(res.fits);
    let out = engine.probe(&res.path).unwrap().video.unwrap();
    assert!(
        out.height < 1280,
        "expected a downscale, got {}p",
        out.height
    );
    let (want, got) = (720.0 / 1280.0, out.width as f64 / out.height as f64);
    assert!(
        (want - got).abs() < 0.02,
        "squashed: {}x{}",
        out.width,
        out.height
    );
}

fn ffmpeg_has(encoder: &str) -> bool {
    Command::new(Ffmpeg::default().ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(encoder))
}

fn zip_names(path: &Path) -> Vec<String> {
    let z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut names: Vec<String> = z.file_names().map(|n| n.unwrap().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn animated_gif_stays_a_gif() {
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("reaction.gif");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=20:duration=6",
            "-vf",
            "noise=alls=30:allf=t",
        ],
        &src,
    );
    let size = std::fs::metadata(&src).unwrap().len();
    let engine = Engine::default();
    assert_eq!(engine.probe(&src).unwrap().kind, MediaKind::Video);
    let res = run(&engine, &src, &Options::new(size / 3));
    assert!(res.fits, "{} of {size}", res.output_bytes);
    assert_eq!(res.path.extension().unwrap(), "gif");
}

#[test]
fn zip_media_inside_is_shrunk_and_the_layout_kept() {
    use std::io::Write;
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let photo = dir.path().join("photo.png");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=2400x1600",
            "-vf",
            "noise=alls=50",
            "-frames:v",
            "1",
        ],
        &photo,
    );
    // Loose files at the top and a hidden one: all of it comes back, in place.
    let src = dir.path().join("holiday.zip");
    let mut z = zip::ZipWriter::new(std::fs::File::create(&src).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    z.start_file("pics/photo.png", opts).unwrap();
    z.write_all(&std::fs::read(&photo).unwrap()).unwrap();
    z.start_file("readme.txt", opts).unwrap();
    z.write_all(b"hello").unwrap();
    z.start_file(".config", opts).unwrap();
    z.write_all(b"x=1").unwrap();
    z.finish().unwrap();

    let size = std::fs::metadata(&src).unwrap().len();
    let res = run(&Engine::default(), &src, &Options::new(size / 4));
    assert!(res.fits, "{} of {size}", res.output_bytes);
    assert_eq!(res.path.extension().unwrap(), "zip");
    assert_eq!(
        zip_names(&res.path),
        [".config", "pics/photo.jpg", "readme.txt"]
    );
}

#[test]
fn seven_z_media_inside_is_shrunk_and_it_stays_7z() {
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("shots");
    std::fs::create_dir(&folder).unwrap();
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=2400x1600",
            "-vf",
            "noise=alls=50",
            "-frames:v",
            "1",
        ],
        &folder.join("a.png"),
    );
    std::fs::write(folder.join("notes.txt"), "note ".repeat(100)).unwrap();
    let engine = Engine::default();
    // Make the 7z with Kompacter itself (a roomy limit: everything kept as-is).
    let mut o = Options::new(u64::MAX / 4);
    o.archive = kompacter_core::ArchiveFormat::SevenZ;
    o.out_dir = Some(dir.path().join("made"));
    let made = run(&engine, &folder, &o).path;
    assert_eq!(made.extension().unwrap(), "7z");

    let size = std::fs::metadata(&made).unwrap().len();
    let mut o = Options::new(size / 4);
    o.out_dir = Some(dir.path().join("out"));
    let res = run(&engine, &made, &o);
    assert!(res.fits, "{} of {size}", res.output_bytes);
    assert_eq!(res.path.extension().unwrap(), "7z");
}

#[test]
fn epub_pngs_are_shrunk_and_keep_their_names() {
    use std::io::Write;
    if !have_ffmpeg() {
        return eprintln!("ffmpeg missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let pic = dir.path().join("cover.png");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=2400x3200",
            "-vf",
            "noise=alls=40",
            "-frames:v",
            "1",
        ],
        &pic,
    );
    let src = dir.path().join("book.epub");
    let mut z = zip::ZipWriter::new(std::fs::File::create(&src).unwrap());
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("mimetype", stored).unwrap();
    z.write_all(b"application/epub+zip").unwrap();
    z.start_file("OEBPS/cover.png", stored).unwrap();
    z.write_all(&std::fs::read(&pic).unwrap()).unwrap();
    z.finish().unwrap();

    let size = std::fs::metadata(&src).unwrap().len();
    let res = run(&Engine::default(), &src, &Options::new(size / 3));
    assert!(res.fits, "{} of {size}", res.output_bytes);
    assert_eq!(res.path.extension().unwrap(), "epub");
    let mut out = zip::ZipArchive::new(std::fs::File::open(&res.path).unwrap()).unwrap();
    assert_eq!(out.by_index(0).unwrap().name().unwrap(), "mimetype");
    let mut png = Vec::new();
    std::io::Read::read_to_end(&mut out.by_name("OEBPS/cover.png").unwrap(), &mut png).unwrap();
    assert_eq!(&png[1..4], b"PNG");
}

#[test]
fn avif_photo_is_read_as_a_picture() {
    if !have_ffmpeg() || !ffmpeg_has("libaom-av1") {
        return eprintln!("ffmpeg or libaom missing, skipping");
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("photo.avif");
    gen(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1600x1200",
            "-vf",
            "noise=alls=30",
            "-frames:v",
            "1",
            "-c:v",
            "libaom-av1",
            "-still-picture",
            "1",
            "-crf",
            "8",
            "-cpu-used",
            "8",
        ],
        &src,
    );
    let engine = Engine::default();
    let info = engine.probe(&src).unwrap();
    assert_eq!(info.kind, MediaKind::Image, "{info:?}");
    let size = info.size_bytes;
    let res = run(&engine, &src, &Options::new(size / 3));
    assert!(res.fits, "{} of {size}", res.output_bytes);
    assert_eq!(res.path.extension().unwrap(), "jpg");
}

#[test]
fn text_that_zip_cant_fit_goes_7z() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("server.log");
    // Log-like text: repetitive enough for both, but LZMA finds far more than deflate.
    let mut text = String::new();
    let mut x: u64 = 7;
    let words = [
        "GET",
        "POST",
        "/api/user",
        "/api/items",
        "200",
        "404",
        "ok",
        "slow",
    ];
    for i in 0..60_000u64 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let w = words[(x >> 33) as usize % words.len()];
        text.push_str(&format!(
            "2026-11-17 12:{:02}:{:02} {w} id={} {}\n",
            i % 60,
            (x >> 40) % 60,
            (x >> 20) % 5000,
            w
        ));
    }
    std::fs::write(&src, &text).unwrap();
    let size = text.len() as u64;
    let engine = Engine::default();
    let zipped = run(&engine, &src, &Options::new(size - 1));
    assert_eq!(zipped.path.extension().unwrap(), "zip");
    let res = run(&engine, &src, &Options::new(zipped.output_bytes - 1));
    assert!(
        res.fits,
        "{} vs zip {}",
        res.output_bytes, zipped.output_bytes
    );
    assert_eq!(res.path.extension().unwrap(), "7z");
}
