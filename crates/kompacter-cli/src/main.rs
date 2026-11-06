//! `kompacter` CLI: a thin wrapper over kompacter-core, handy for testing the engine
//! and for scripting. The desktop app calls the same library directly.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use kompacter_core::size::{human, parse_size, PRESETS};
use kompacter_core::{ArchiveFormat, Engine, Mode, Options, Speed, Trim, VideoCodec};

#[derive(Parser)]
#[command(
    name = "kompacter",
    version,
    about = "Fit any file under an upload limit. All local."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Compress a file or folder to fit the target size.
    Compress {
        input: PathBuf,
        #[command(flatten)]
        common: Common,
        /// Only cut the selection, no re-encode (needs --start/--end).
        #[arg(long)]
        keep_quality: bool,
        #[arg(long, value_enum, default_value_t = SpeedArg::Balanced)]
        speed: SpeedArg,
        /// Don't use the GPU, encode on the CPU only.
        #[arg(long)]
        cpu: bool,
        /// Output folder (default: next to the input).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Make parts that each fit when one file can't (video, audio, folders).
        #[arg(long)]
        split: bool,
        /// Make a GIF instead (videos up to 30 s; use --start/--end for a part).
        #[arg(long, conflicts_with_all = ["keep_quality", "split"])]
        gif: bool,
    },
    /// Show what we'd do (resolution, bitrate, quality) without encoding. Prints JSON.
    Estimate {
        input: PathBuf,
        #[command(flatten)]
        common: Common,
    },
    /// Print media info as JSON.
    Probe { input: PathBuf },
    /// List built-in size presets.
    Presets,
    /// Show which GPU encoders work on this PC.
    Encoders,
}

#[derive(clap::Args)]
struct Common {
    /// Size limit: 20mb, 500kb, 1.5gb, or a preset id (see `kompacter presets`).
    #[arg(short, long, default_value = "discord")]
    target: String,
    /// Trim start, in seconds.
    #[arg(long)]
    start: Option<f64>,
    /// Trim end, in seconds.
    #[arg(long)]
    end: Option<f64>,
    /// Video codec: h264 (most compatible), h265 (smaller), av1 (smallest).
    #[arg(long, value_enum, default_value_t = CodecArg::H264)]
    codec: CodecArg,
    /// Archive for folders and other files: zip (opens everywhere) or 7z (smaller).
    #[arg(long, value_enum, default_value_t = ArchiveArg::Zip)]
    archive: ArchiveArg,
}

#[derive(Clone, Copy, ValueEnum)]
enum CodecArg {
    H264,
    H265,
    Av1,
}

#[derive(Clone, Copy, ValueEnum)]
enum ArchiveArg {
    Zip,
    #[value(name = "7z")]
    SevenZ,
}

#[derive(Clone, Copy, ValueEnum)]
enum SpeedArg {
    Fast,
    Balanced,
    Quality,
}

impl Common {
    fn options(&self) -> Result<Options> {
        let mut o = Options::new(parse_size(&self.target)?);
        o.video_codec = match self.codec {
            CodecArg::H264 => VideoCodec::H264,
            CodecArg::H265 => VideoCodec::H265,
            CodecArg::Av1 => VideoCodec::Av1,
        };
        o.archive = match self.archive {
            ArchiveArg::Zip => ArchiveFormat::Zip,
            ArchiveArg::SevenZ => ArchiveFormat::SevenZ,
        };
        if self.start.is_some() || self.end.is_some() {
            let t = Trim::new(self.start.unwrap_or(0.0), self.end.unwrap_or(f64::MAX))
                .map_err(anyhow::Error::msg)?;
            o.trim = Some(t);
        }
        Ok(o)
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let engine = Engine::default();
    match cli.cmd {
        Cmd::Presets => {
            for p in PRESETS {
                println!("{:<22} {:<22} {}", p.id, p.label, human(p.bytes));
            }
        }
        Cmd::Encoders => {
            for codec in [VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1] {
                let found = engine.hw_encoders_for(codec);
                if found.is_empty() {
                    println!("{:<6} no working GPU encoder; uses the CPU", codec.label());
                }
                for e in found {
                    let name = e.ffmpeg_name(codec).unwrap_or("-");
                    println!("{:<6} {:<20} {name}", codec.label(), e.label());
                }
            }
        }
        Cmd::Probe { input } => {
            let info = engine.probe(&input)?;
            println!("{}", serde_json::to_string_pretty(&info)?);
        }
        Cmd::Estimate { input, common } => {
            let info = engine.probe(&input)?;
            let est = engine.estimate(&info, &common.options()?)?;
            println!("{}", serde_json::to_string_pretty(&est)?);
        }
        Cmd::Compress {
            input,
            common,
            keep_quality,
            speed,
            cpu,
            out,
            split,
            gif,
        } => {
            let mut opts = common.options()?;
            opts.out_dir = out;
            opts.split = split;
            opts.gpu = !cpu;
            opts.mode = if gif {
                Mode::Gif
            } else if keep_quality {
                Mode::KeepQuality
            } else {
                Mode::Fit
            };
            opts.speed = match speed {
                SpeedArg::Fast => Speed::Fast,
                SpeedArg::Balanced => Speed::Balanced,
                SpeedArg::Quality => Speed::Quality,
            };
            let cancel = AtomicBool::new(false);
            let mut last = -1i32;
            let mut on_progress = |p: f32| {
                let pct = (p * 100.0) as i32;
                if pct != last {
                    last = pct;
                    eprint!("\r{pct:>3}%");
                    let _ = std::io::stderr().flush();
                }
            };
            let res = engine
                .compress(&input, &opts, &mut on_progress, &cancel)
                .with_context(|| format!("compressing {}", input.display()))?;
            eprintln!();
            if res.untouched {
                println!(
                    "Already fits ({}), nothing to do.",
                    human(res.original_bytes)
                );
            } else {
                let mark = if res.fits {
                    "✓"
                } else {
                    "✗ still over the limit"
                };
                println!(
                    "{} → {}  {}  {}",
                    human(res.original_bytes),
                    human(res.output_bytes),
                    mark,
                    res.path.display()
                );
            }
        }
    }
    Ok(())
}
