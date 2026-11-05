//! Hardware video encoders (H.264, H.265, AV1). ffmpeg builds often *list* NVENC/QSV/AMF even when the PC has
//! no such GPU, so each candidate is checked with a tiny real encode before it's trusted.

use std::process::Stdio;

use serde::Serialize;

use crate::ffmpeg::Ffmpeg;
use crate::VideoCodec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoder {
    /// NVIDIA
    Nvenc,
    /// Intel Quick Sync
    Qsv,
    /// AMD
    Amf,
    /// macOS
    VideoToolbox,
}

impl Encoder {
    /// ffmpeg's name for this GPU's encoder of `codec` (`None`: this GPU family has none).
    pub fn ffmpeg_name(self, codec: VideoCodec) -> Option<&'static str> {
        use VideoCodec::*;
        Some(match (self, codec) {
            (Encoder::Nvenc, H264) => "h264_nvenc",
            (Encoder::Nvenc, H265) => "hevc_nvenc",
            (Encoder::Nvenc, Av1) => "av1_nvenc",
            (Encoder::Qsv, H264) => "h264_qsv",
            (Encoder::Qsv, H265) => "hevc_qsv",
            (Encoder::Qsv, Av1) => "av1_qsv",
            (Encoder::Amf, H264) => "h264_amf",
            (Encoder::Amf, H265) => "hevc_amf",
            (Encoder::Amf, Av1) => "av1_amf",
            (Encoder::VideoToolbox, H264) => "h264_videotoolbox",
            (Encoder::VideoToolbox, H265) => "hevc_videotoolbox",
            (Encoder::VideoToolbox, Av1) => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Encoder::Nvenc => "NVIDIA NVENC",
            Encoder::Qsv => "Intel Quick Sync",
            Encoder::Amf => "AMD AMF",
            Encoder::VideoToolbox => "Apple VideoToolbox",
        }
    }

    /// Pixel format the encoder is happiest with.
    pub(crate) fn pix_fmt(self) -> &'static str {
        match self {
            Encoder::Qsv => "nv12",
            _ => "yuv420p",
        }
    }

    /// Rate-control args for a single-pass encode aimed at `kbps`.
    pub(crate) fn rate_args(self, kbps: u32) -> Vec<String> {
        let (b, max, buf) = (
            format!("{kbps}k"),
            format!("{}k", kbps * 3 / 2),
            format!("{}k", kbps * 2),
        );
        let mut a: Vec<String> = match self {
            // Two-pass-like quality inside one run.
            Encoder::Nvenc => [
                "-preset",
                "p5",
                "-tune",
                "hq",
                "-rc",
                "vbr",
                "-multipass",
                "fullres",
            ]
            .map(String::from)
            .to_vec(),
            Encoder::Qsv => ["-preset", "medium"].map(String::from).to_vec(),
            Encoder::Amf => ["-quality", "balanced", "-rc", "vbr_peak"]
                .map(String::from)
                .to_vec(),
            Encoder::VideoToolbox => vec![],
        };
        a.extend([
            "-b:v".into(),
            b,
            "-maxrate".into(),
            max,
            "-bufsize".into(),
            buf,
        ]);
        a
    }
}

/// Candidates worth trying on this OS, best first.
fn candidates() -> &'static [Encoder] {
    if cfg!(target_os = "macos") {
        &[Encoder::VideoToolbox]
    } else if cfg!(target_os = "windows") {
        &[Encoder::Nvenc, Encoder::Amf, Encoder::Qsv]
    } else {
        &[Encoder::Nvenc, Encoder::Qsv]
    }
}

/// Hardware encoders for `codec` that actually work on this machine, best first. Takes
/// ~0.5 s per candidate, so callers should cache it (`Engine` does). Older GPUs often have
/// H.264 but no AV1; the test encode finds out.
pub fn detect(ff: &Ffmpeg, codec: VideoCodec) -> Vec<Encoder> {
    candidates()
        .iter()
        .copied()
        .filter(|e| e.ffmpeg_name(codec).is_some_and(|name| works(ff, *e, name)))
        .collect()
}

fn works(ff: &Ffmpeg, enc: Encoder, name: &str) -> bool {
    crate::ffmpeg::command(&ff.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(["-f", "lavfi", "-i", "color=black:s=256x256:r=30:d=0.2"])
        .args(["-c:v", name, "-pix_fmt", enc.pix_fmt()])
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_args_scale_with_bitrate() {
        let a = Encoder::Nvenc.rate_args(2000);
        let i = a.iter().position(|x| x == "-maxrate").unwrap();
        assert_eq!(a[i + 1], "3000k");
        assert!(a.contains(&"2000k".to_string()));
    }
}
