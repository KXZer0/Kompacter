//! Output formats the user can choose. Every default works for everyone; the others trade
//! compatibility for smaller files.

use serde::{Deserialize, Serialize};

use crate::{Options, Speed};

/// How to trade format compatibility, detail and encoding time.
///
/// - Compatibility: formats every phone, PC and chat app plays, encoded quickly.
/// - Balanced (default): the most detail that still plays almost everywhere: H.264,
///   MP3 (Opus only when the budget is tiny), pictures in their best fitting format, Zip.
/// - Quality: the most detail per MB, for newer devices: AV1, Opus, WebP and 7z, with
///   the slower encoder settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompressionPreference {
    Compatibility,
    #[default]
    Balanced,
    /// Called "Efficiency" before; settings saved with that name still load.
    #[serde(alias = "efficiency")]
    Quality,
}

impl CompressionPreference {
    /// Defaults for a preference. Explicit format choices can override these options.
    /// Whether video goes to the graphics card is the GPU encoding switch, not this.
    pub fn options(self, target_bytes: u64) -> Options {
        let mut options = Options::new(target_bytes);
        match self {
            Self::Compatibility => {
                options.speed = Speed::Fast;
                options.audio_format = AudioFormat::Mp3;
                options.image_format = ImageFormat::Jpeg;
            }
            Self::Balanced => {
                options.audio_format = AudioFormat::Auto;
                options.image_format = ImageFormat::Auto;
            }
            Self::Quality => {
                options.speed = Speed::Quality;
                options.video_codec = VideoCodec::Av1;
                options.audio_format = AudioFormat::Opus;
                options.image_format = ImageFormat::Webp;
                options.archive = ArchiveFormat::SevenZ;
            }
        }
        options
    }
}

/// Video codec. Same file size: AV1 looks best, then H.265, then H.264.
/// Same look: AV1 is smallest. H.264 plays (and previews inline) everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    /// Most compatible: previews inline everywhere.
    #[default]
    H264,
    /// About 35% smaller at the same look; fast on most GPUs. Some PCs only offer a download.
    H265,
    /// About 45% smaller at the same look; best quality per MB. Slow without a newer GPU.
    Av1,
}

impl VideoCodec {
    /// Bits needed for the same look, relative to H.264.
    pub fn efficiency(self) -> f64 {
        match self {
            VideoCodec::H264 => 1.0,
            VideoCodec::H265 => 0.65,
            VideoCodec::Av1 => 0.55,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264",
            VideoCodec::H265 => "H.265",
            VideoCodec::Av1 => "AV1",
        }
    }

    /// What ffprobe calls a stream in this codec.
    pub(crate) fn probe_name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "h264",
            VideoCodec::H265 => "hevc",
            VideoCodec::Av1 => "av1",
        }
    }
}

/// Audio-only files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    /// MP3, or Opus when the budget is tiny (it sounds far better there).
    #[default]
    Auto,
    /// Plays everywhere.
    Mp3,
    /// Sounds better at the same size; a few older players can't open it.
    Opus,
}

/// Pictures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    /// Lossless first (identical pixels, as WebP), then JPEG, or WebP when transparent.
    #[default]
    Auto,
    /// Always JPEG, except pictures with transparency (those stay WebP).
    Jpeg,
    /// Always WebP: smaller than JPEG, keeps transparency.
    Webp,
}

/// Folders and other files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ArchiveFormat {
    /// Opens everywhere.
    #[default]
    #[serde(rename = "zip")]
    Zip,
    /// Usually 20–40% smaller for documents and programs. Opens on Windows 11, or with 7-Zip.
    #[serde(rename = "7z")]
    SevenZ,
}

impl ArchiveFormat {
    pub fn ext(self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::SevenZ => "7z",
        }
    }
}
