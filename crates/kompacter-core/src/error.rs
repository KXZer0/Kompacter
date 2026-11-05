use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "could not start `{0}`: is ffmpeg installed? (or set KOMPACTER_FFMPEG / KOMPACTER_FFPROBE)"
    )]
    FfmpegMissing(String),

    #[error("ffmpeg failed: {0}")]
    Ffmpeg(String),

    #[error("could not read media info for {path}: {reason}")]
    Probe { path: PathBuf, reason: String },

    #[error("invalid trim range: {0}")]
    InvalidTrim(String),

    #[error("invalid size `{0}` (try 20mb, 500kb, 1.5gb)")]
    InvalidSize(String),

    #[error("can't fit {what} under the limit ({target} bytes); best result was {best} bytes")]
    DoesNotFit {
        what: String,
        target: u64,
        best: u64,
    },

    #[error(
        "{seconds:.0}s is too long to fit under the limit at watchable quality; trim it first"
    )]
    TooLong { seconds: f64 },

    #[error("unsupported input: {0}")]
    Unsupported(String),

    #[error("cancelled")]
    Cancelled,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
}
