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

    let video = video_json.map(|s| VideoStream {
        width: s["width"].as_u64().unwrap_or(0) as u32,
        height: s["height"].as_u64().unwrap_or(0) as u32,
        fps: parse_rate(s["avg_frame_rate"].as_str())
            .or_else(|| parse_rate(s["r_frame_rate"].as_str()))
            .unwrap_or(30.0),
        codec: s["codec_name"].as_str().unwrap_or("").into(),
    });
    let audio = audio_json.map(|s| AudioStream {
        codec: s["codec_name"].as_str().unwrap_or("").into(),
        bitrate_kbps: s["bit_rate"]
            .as_str()
            .and_then(|b| b.parse::<u64>().ok())
            .map(|b| (b / 1000) as u32),
        channels: s["channels"].as_u64().unwrap_or(2) as u32,
    });

    let is_still_image = format_name == "image2" || format_name.ends_with("_pipe");
    let kind = match (&video, &audio) {
        (Some(_), _) if is_still_image => MediaKind::Image,
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
