//! Exercise video output selection and publication with generated media.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use kompacter_core::{Engine, Error, Ffmpeg, Mode, Options};

fn have_ffmpeg() -> bool {
    let ff = Ffmpeg::default();
    [ff.ffmpeg, ff.ffprobe].iter().all(|program| {
        Command::new(program)
            .arg("-version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

fn generate(path: &Path, video_encoder: &str) {
    let result = Command::new(Ffmpeg::default().ffmpeg)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=30:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-c:v",
            video_encoder,
            "-threads:v",
            "2",
            "-c:a",
            "aac",
            "-b:a",
            "48k",
            "-movflags",
            "+faststart",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn unprobeable_mp4_is_reported_even_under_the_cap() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("truncated.mp4");
    std::fs::write(&path, b"\0\0\0\x18ftypisom\0\0\0\0isommp41").unwrap();
    let result = Engine::default().compress(
        &path,
        &Options::new(1024),
        &mut |_| {},
        &AtomicBool::new(false),
    );
    assert!(matches!(result, Err(Error::Probe { .. })), "{result:?}");
}

#[test]
fn corrupt_compatible_video_is_never_returned_untouched() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("corrupt.mp4");
    generate(&input, "libx264");
    let truncated_size = std::fs::metadata(&input).unwrap().len() - 100;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&input)
        .unwrap()
        .set_len(truncated_size)
        .unwrap();

    let engine = Engine::default();
    let info = engine.probe(&input).unwrap();
    let mut opts = Options::new(200_000);
    opts.gpu = false;
    assert!(engine.estimate(&info, &opts).unwrap().fits_as_is);
    match engine.compress(&input, &opts, &mut |_| {}, &AtomicBool::new(false)) {
        Ok(result) => {
            assert!(result.fits && !result.untouched);
            let verified = Command::new(Ffmpeg::default().ffmpeg)
                .args(["-v", "error", "-xerror", "-i"])
                .arg(result.path)
                .args(["-f", "null", "-"])
                .status()
                .unwrap();
            assert!(verified.success());
        }
        Err(Error::Ffmpeg(_)) => (),
        Err(err) => panic!("unexpected error: {err}"),
    }
}

fn packet_hashes(path: &Path, stream: &str) -> Vec<String> {
    let result = Command::new(Ffmpeg::default().ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_packets",
            "-show_data_hash",
            "sha256",
            "-show_entries",
            "packet=data_hash",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(result.status.success());
    let stdout = String::from_utf8(result.stdout).unwrap();
    let packets: Vec<_> = stdout
        .lines()
        .flat_map(|line| line.split(','))
        .filter(|part| part.starts_with("SHA256:"))
        .map(str::to_owned)
        .collect();
    assert!(!packets.is_empty());
    packets
}

#[test]
fn keeps_verified_mp4_at_exact_cap_and_remuxes_compatible_packets() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mp4 = dir.path().join("source.mp4");
    generate(&mp4, "libx264");
    let engine = Engine::default();
    let info = engine.probe(&mp4).unwrap();
    let mut opts = Options::new(info.size_bytes);
    opts.gpu = false;
    assert!(engine.estimate(&info, &opts).unwrap().fits_as_is);
    let cancel = AtomicBool::new(false);
    let result = engine.compress(&mp4, &opts, &mut |_| {}, &cancel).unwrap();
    assert!(result.untouched && result.fits);
    assert_eq!(result.output_bytes, info.size_bytes);
    assert_eq!(result.path, mp4);

    opts.target_bytes -= 1;
    assert!(!engine.estimate(&info, &opts).unwrap().fits_as_is);
    let smaller = engine.compress(&mp4, &opts, &mut |_| {}, &cancel).unwrap();
    assert!(!smaller.untouched && smaller.fits);

    // This input has compatible packets in a different actual container.
    let mkv = dir.path().join("source.mkv");
    let remux = Command::new(Ffmpeg::default().ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&mp4)
        .args(["-c", "copy"])
        .arg(&mkv)
        .status()
        .unwrap();
    assert!(remux.success());
    opts.target_bytes = 200_000;
    let info = engine.probe(&mkv).unwrap();
    assert!(!engine.estimate(&info, &opts).unwrap().fits_as_is);
    let result = engine.compress(&mkv, &opts, &mut |_| {}, &cancel).unwrap();
    assert!(!result.untouched && result.fits);
    assert_eq!(result.path.extension().unwrap(), "mp4");
    for stream in ["v:0", "a:0"] {
        assert_eq!(
            packet_hashes(&mkv, stream),
            packet_hashes(&result.path, stream)
        );
    }

    opts.mode = Mode::KeepQuality;
    let result = engine.compress(&mkv, &opts, &mut |_| {}, &cancel).unwrap();
    assert!(result.untouched);
    assert_eq!(result.path, mkv);
}

#[test]
fn replacing_incompatible_video_preserves_compatible_aac() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("mpeg4.mp4");
    generate(&input, "mpeg4");
    let mut opts = Options::new(200_000);
    opts.gpu = false;

    let engine = Engine::default();
    let info = engine.probe(&input).unwrap();
    assert!(info.size_bytes < opts.target_bytes);
    assert!(!engine.estimate(&info, &opts).unwrap().fits_as_is);
    let result = engine
        .compress(&input, &opts, &mut |_| {}, &AtomicBool::new(false))
        .unwrap();
    assert!(result.fits && !result.untouched);
    assert_eq!(
        engine.probe(&result.path).unwrap().video.unwrap().codec,
        "h264"
    );
    assert_eq!(
        packet_hashes(&input, "a:0"),
        packet_hashes(&result.path, "a:0")
    );
}

#[test]
fn cancellation_keeps_previous_output_and_cleans_staging() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("cancel.mp4");
    generate(&input, "mpeg4");
    let previous = dir.path().join("cancel.kompacter.mp4");
    std::fs::write(&previous, b"previous").unwrap();
    let cancel = AtomicBool::new(false);
    let mut opts = Options::new(200_000);
    opts.gpu = false;
    let result = Engine::default().compress(
        &input,
        &opts,
        &mut |_| cancel.store(true, Ordering::Relaxed),
        &cancel,
    );
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(std::fs::read(previous).unwrap(), b"previous");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn failed_hardware_encode_retries_same_codec_and_measures_overshoot() {
    use std::os::unix::fs::PermissionsExt;

    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("fallback.mp4");
    generate(&input, "mpeg4");
    let wrapped = dir.path().join("ffmpeg");
    let log = dir.path().join("attempts");
    let ff = Ffmpeg::default();
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    std::fs::write(
        &wrapped,
        format!(
            r#"#!/bin/sh
log={}
real={}
while_arg=
for arg do
    case "$arg" in
        color=black:*) probe=1 ;;
        h264_nvenc|h264_qsv|h264_videotoolbox|h264_amf) hardware=1 ;;
        libx264) x264=1 ;;
    esac
    if [ "$while_arg" = "-pass:v" ]; then pass=$arg; fi
    if [ "$while_arg" = "-b:v" ]; then rate=$arg; fi
    while_arg=$arg
done
if [ "$hardware" = 1 ]; then
    if [ "$probe" = 1 ]; then exit 0; fi
    printf '%s\n' "hardware failed" >> "$log"
    printf '%s\n' "simulated hardware failure" >&2
    exit 1
fi
if [ "$x264" = 1 ]; then
    printf 'x264 %s %s\n' "$pass" "$rate" >> "$log"
    "$real" "$@" || exit $?
    if [ "$pass" = 2 ] && [ ! -e "$log.overshot" ]; then
        touch "$log.overshot"
        dd if=/dev/zero bs=1000 count=250 >> "$while_arg" 2>/dev/null || exit $?
    fi
    exit 0
fi
exec "$real" "$@"
"#,
            quote(&log),
            quote(&ff.ffmpeg),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapped, std::fs::Permissions::from_mode(0o700)).unwrap();

    let engine = Engine::new(Ffmpeg {
        ffmpeg: wrapped,
        ffprobe: ff.ffprobe,
    });
    let opts = Options::new(200_000);
    let result = engine
        .compress(&input, &opts, &mut |_| {}, &AtomicBool::new(false))
        .unwrap();
    assert!(result.fits && !result.untouched);
    assert_eq!(
        engine.probe(&result.path).unwrap().video.unwrap().codec,
        "h264"
    );
    assert_eq!(
        result.output_bytes,
        std::fs::metadata(&result.path).unwrap().len()
    );
    assert!(result.output_bytes <= opts.target_bytes);

    let attempts = std::fs::read_to_string(log).unwrap();
    assert!(attempts.contains("hardware failed"));
    let pass_two: Vec<u32> = attempts
        .lines()
        .filter_map(|line| {
            line.strip_prefix("x264 2 ")?
                .strip_suffix('k')?
                .parse()
                .ok()
        })
        .collect();
    assert_eq!(
        attempts
            .lines()
            .filter(|line| line.starts_with("x264 1 "))
            .count(),
        2
    );
    assert_eq!(pass_two.len(), 2);
    assert!(pass_two[1] < pass_two[0]);
}
