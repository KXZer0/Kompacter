//! Output formats the user can choose. Every default works for everyone; the others trade
//! compatibility for smaller files.

use serde::{Deserialize, Serialize};

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
