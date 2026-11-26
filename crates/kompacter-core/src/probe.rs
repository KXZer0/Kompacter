//! Reads file info with ffprobe and decides what kind of media a file is.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::error::Result;
use crate::ffmpeg::Ffmpeg;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Video,
    Audio,
    Image,
    Folder,
    /// Anything ffprobe can't read (documents, archives, ...). Passed through as-is.
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoStream {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub codec: String,
    /// ffprobe's pixel format, e.g. `yuv420p` (8-bit 4:2:0) or `yuv420p10le` (10-bit).
    pub pix_fmt: String,
    /// The video's own bitrate, when it's known or can be worked out.
    pub bitrate_kbps: Option<u32>,
    /// HDR (PQ or HLG): looks washed out as ordinary video unless tone-mapped.
    pub hdr: Option<Hdr>,
    /// A rotation flag turns the picture (by any quarter turn).
    pub rotated: bool,
}

/// HDR transfer curves, from the stream's `color_transfer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Hdr {
    /// PQ (SMPTE ST 2084): HDR10, HDR10+, most Dolby Vision.
    Pq,
    /// HLG (ARIB STD-B67): phones and TV broadcasts.
    Hlg,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioStream {
    pub codec: String,
    pub bitrate_kbps: Option<u32>,
    pub channels: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub kind: MediaKind,
    /// Seconds. 0 for images and non-media files.
    pub duration: f64,
    pub video: Option<VideoStream>,
    pub audio: Option<AudioStream>,
    /// True when the image has transparency (so we keep a format that supports it).
    pub has_alpha: bool,
}

pub fn probe(ff: &Ffmpeg, path: &Path) -> Result<MediaInfo> {
    let meta = std::fs::metadata(path)?;
    if meta.is_dir() {
        return Ok(MediaInfo {
            path: path.into(),
            size_bytes: dir_size(path)?,
            kind: MediaKind::Folder,
            duration: 0.0,
            video: None,
            audio: None,
            has_alpha: false,
        });
    }
    let size_bytes = meta.len();
    let other = || MediaInfo {
        path: path.into(),
        size_bytes,
        kind: MediaKind::Other,
        duration: 0.0,
        video: None,
        audio: None,
        has_alpha: false,
    };
    match ff.probe_json(path)? {
        Some(json) => Ok(classify(path, size_bytes, &json).unwrap_or_else(other)),
        None => Ok(other()),
    }
}

/// Turn ffprobe's JSON into a `MediaInfo`. Returns `None` when there is no usable stream.
pub(crate) fn classify(path: &Path, size_bytes: u64, json: &Value) -> Option<MediaInfo> {
    let streams = json.get("streams")?.as_array()?;
    let format = json.get("format");
    let format_name = format
        .and_then(|f| f.get("format_name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let duration = format
        .and_then(|f| f.get("duration"))
        .and_then(Value::as_str)
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);

    // Album art inside audio files shows up as a video stream; skip it.
    let video_json = streams.iter().find(|s| {
        s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_i64().unwrap_or(0) == 0
    });
    let audio_json = streams.iter().find(|s| s["codec_type"] == "audio");

    let video = video_json.map(|s| {
        let (w, h) = (
            s["width"].as_u64().unwrap_or(0) as u32,
            s["height"].as_u64().unwrap_or(0) as u32,
        );
        // Phones store portrait video sideways plus a rotation flag. ffmpeg turns the
        // picture upright before our filters run, so plan with the upright size.
        let turns = quarter_turns(s);
        let (width, height) = if turns % 2 == 1 { (h, w) } else { (w, h) };
        VideoStream {
            width,
            height,
            fps: parse_rate(s["avg_frame_rate"].as_str())
                .or_else(|| parse_rate(s["r_frame_rate"].as_str()))
                .unwrap_or(30.0),
            codec: s["codec_name"].as_str().unwrap_or("").into(),
            pix_fmt: s["pix_fmt"].as_str().unwrap_or("").into(),
            bitrate_kbps: video_kbps(s, streams, format),
            hdr: match s["color_transfer"].as_str() {
                Some("smpte2084") => Some(Hdr::Pq),
                Some("arib-std-b67") => Some(Hdr::Hlg),
                _ => None,
            },
            rotated: turns != 0,
        }
    });
    let audio = audio_json.map(|s| AudioStream {
        codec: s["codec_name"].as_str().unwrap_or("").into(),
        bitrate_kbps: bits_per_second(s).map(|b| (b / 1000) as u32),
        channels: s["channels"].as_u64().unwrap_or(2) as u32,
    });

    // HEIC/HEIF/AVIF photos come through the MP4 demuxer, with no duration and one frame.
    // APNG remains an image so the still-image job can reject frame-dropping conversions.
    let one_frame = video_json
        .and_then(|s| s["nb_frames"].as_str())
        .and_then(|n| n.parse::<u64>().ok())
        .is_none_or(|n| n <= 1);
    let is_image = matches!(format_name, "image2" | "apng")
        || format_name.ends_with("_pipe")
        || (is_heif_like(path) && (duration <= 0.0 || one_frame));
    let kind = match (&video, &audio) {
        (Some(_), _) if is_image => MediaKind::Image,
        (Some(_), _) if duration > 0.0 => MediaKind::Video,
        (None, Some(_)) if duration > 0.0 => MediaKind::Audio,
        _ => return None,
    };
    let has_alpha = video_json
        .and_then(|s| s["pix_fmt"].as_str())
        .map(|p| {
            ["rgba", "bgra", "argb", "abgr", "yuva", "ya", "pal8"]
                .iter()
                .any(|a| p.starts_with(a))
        })
        .unwrap_or(false);

    Some(MediaInfo {
        path: path.into(),
        size_bytes,
        kind,
        duration: if kind == MediaKind::Image {
            0.0
        } else {
            duration
        },
        video,
        audio,
        has_alpha,
    })
}

/// Phone photo formats (HEIC/HEIF from iPhones, AVIF) that ffmpeg reads as one-frame MP4s.
pub(crate) fn is_heif_like(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "heic" | "heif" | "hif" | "avif"))
}

/// How many quarter turns (0–3) the stream's rotation flag asks for; odd means sideways.
/// Newer files carry it as display-matrix side data, older ones as a `rotate` tag.
fn quarter_turns(stream: &Value) -> i64 {
    let from_matrix = stream["side_data_list"]
        .as_array()
        .and_then(|l| l.iter().find_map(|d| d["rotation"].as_f64()));
    let from_tag = stream["tags"]["rotate"]
        .as_str()
        .and_then(|r| r.parse::<f64>().ok());
    let turns = (from_matrix.or(from_tag).unwrap_or(0.0) / 90.0).round() as i64;
    turns.rem_euclid(4)
}

/// `bit_rate` of a stream or of the whole file, in bits per second.
fn bits_per_second(v: &Value) -> Option<u64> {
    v["bit_rate"].as_str()?.parse().ok()
}

/// The video's own bitrate: ffprobe's number when the container has one (MP4, MOV),
/// otherwise the whole file's minus the audio (MKV, WebM). Rounded up, so a guess errs
/// on the big side. `None` when an audio track's bitrate is unknown too.
fn video_kbps(video: &Value, streams: &[Value], format: Option<&Value>) -> Option<u32> {
    let bits = match bits_per_second(video) {
        Some(b) => b,
        None => {
            let audio: Option<u64> = streams
                .iter()
                .filter(|s| s["codec_type"] == "audio")
                .map(bits_per_second)
                .sum();
            bits_per_second(format?)?.saturating_sub(audio?)
        }
    };
    (bits > 0).then(|| bits.div_ceil(1000) as u32)
}

fn parse_rate(rate: Option<&str>) -> Option<f64> {
    let (n, d) = rate?.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then(|| n / d)
}

pub(crate) fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        total += if meta.is_dir() {
            dir_size(&entry.path())?
        } else {
            meta.len()
        };
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_video_and_skips_cover_art() {
        let j = json!({
            "format": {"format_name": "mov,mp4,m4a,3gp,3g2,mj2", "duration": "62.5"},
            "streams": [
                {"codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080,
                 "avg_frame_rate": "30000/1001", "disposition": {"attached_pic": 0}},
                {"codec_type": "audio", "codec_name": "aac", "bit_rate": "128000", "channels": 2}
            ]
        });
        let info = classify(Path::new("clip.mp4"), 48, &j).unwrap();
        assert_eq!(info.kind, MediaKind::Video);
        assert!((info.video.unwrap().fps - 29.97).abs() < 0.01);

        let mp3 = json!({
            "format": {"format_name": "mp3", "duration": "200.0"},
            "streams": [
                {"codec_type": "audio", "codec_name": "mp3", "channels": 2},
                {"codec_type": "video", "codec_name": "mjpeg", "disposition": {"attached_pic": 1}}
            ]
        });
        assert_eq!(
            classify(Path::new("song.mp3"), 1, &mp3).unwrap().kind,
            MediaKind::Audio
        );
    }

    #[test]
    fn portrait_phone_video_is_upright() {
        let stream = |extra: Value| {
            let mut s = json!({"codec_type": "video", "codec_name": "h264", "width": 1920,
                               "height": 1080, "avg_frame_rate": "30/1"});
            s.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            json!({"format": {"format_name": "mov,mp4", "duration": "10"}, "streams": [s]})
        };
        let size = |j: &Value| {
            let v = classify(Path::new("a.mp4"), 1, j).unwrap().video.unwrap();
            (v.width, v.height, v.rotated)
        };
        let matrix = stream(json!({"side_data_list": [{"rotation": -90}]}));
        assert_eq!(size(&matrix), (1080, 1920, true));
        let tag = stream(json!({"tags": {"rotate": "270"}}));
        assert_eq!(size(&tag), (1080, 1920, true));
        let upside_down = stream(json!({"tags": {"rotate": "180"}}));
        assert_eq!(size(&upside_down), (1920, 1080, true));
        assert_eq!(size(&stream(json!({}))), (1920, 1080, false));
    }

    #[test]
    fn detects_hdr_from_the_transfer_curve() {
        let with = |trc: &str| {
            let j = json!({
                "format": {"format_name": "mov,mp4", "duration": "5"},
                "streams": [{"codec_type": "video", "codec_name": "hevc", "width": 3840,
                             "height": 2160, "avg_frame_rate": "60/1",
                             "pix_fmt": "yuv420p10le", "color_primaries": "bt2020",
                             "color_transfer": trc}]
            });
            classify(Path::new("a.mp4"), 1, &j).unwrap().video.unwrap()
        };
        assert_eq!(with("smpte2084").hdr, Some(Hdr::Pq));
        assert_eq!(with("arib-std-b67").hdr, Some(Hdr::Hlg));
        assert_eq!(with("bt709").hdr, None);
        assert_eq!(with("bt709").pix_fmt, "yuv420p10le");
    }

    #[test]
    fn video_bitrate_from_the_stream_or_the_file_minus_audio() {
        let video = |video_rate: Option<&str>, file_rate: &str, audio_rate: Option<&str>| {
            let mut v = json!({"codec_type": "video", "codec_name": "h264", "width": 320,
                               "height": 240, "avg_frame_rate": "30/1"});
            let mut a = json!({"codec_type": "audio", "codec_name": "pcm_s16le"});
            if let Some(r) = video_rate {
                v["bit_rate"] = r.into();
            }
            if let Some(r) = audio_rate {
                a["bit_rate"] = r.into();
            }
            let j = json!({
                "format": {"format_name": "matroska,webm", "duration": "10",
                           "bit_rate": file_rate},
                "streams": [v, a]
            });
            classify(Path::new("a.mkv"), 1, &j).unwrap().video.unwrap()
        };
        // MP4 / MOV: the stream says it (rounded up).
        let mp4 = video(Some("200400"), "1752709", Some("1536000"));
        assert_eq!(mp4.bitrate_kbps, Some(201));
        // MKV: the file's bitrate minus the audio's.
        let mkv = video(None, "1752709", Some("1536000"));
        assert_eq!(mkv.bitrate_kbps, Some(217));
        // The audio's bitrate is unknown too: no guess.
        assert_eq!(video(None, "1752709", None).bitrate_kbps, None);
    }

    #[test]
    fn detects_images_and_alpha() {
        let png = json!({
            "format": {"format_name": "png_pipe"},
            "streams": [{"codec_type": "video", "codec_name": "png", "width": 800, "height": 600,
                         "pix_fmt": "rgba", "avg_frame_rate": "0/0"}]
        });
        let info = classify(Path::new("a.png"), 1, &png).unwrap();
        assert_eq!(info.kind, MediaKind::Image);
        assert!(info.has_alpha);
    }
}
