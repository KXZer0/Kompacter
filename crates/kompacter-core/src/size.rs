//! Upload limits and size parsing. Sizes use binary units (1 MB = 1024 * 1024 bytes),
//! which is how chat apps such as Discord measure their limits.

use serde::Serialize;

use crate::error::{Error, Result};

pub const KB: u64 = 1024;
pub const MB: u64 = 1024 * KB;
pub const GB: u64 = 1024 * MB;

/// A named upload limit the user can pick in settings.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub bytes: u64,
}

/// Built-in limits. Keep these in sync with each service's help pages.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "discord",
        label: "Discord (free)",
        bytes: 20 * MB,
    },
    Preset {
        id: "discord-nitro-basic",
        label: "Discord Nitro Basic",
        bytes: 50 * MB,
    },
    Preset {
        id: "discord-nitro",
        label: "Discord Nitro",
        bytes: 500 * MB,
    },
    Preset {
        id: "email",
        label: "Email attachment",
        bytes: 25 * MB,
    },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id.eq_ignore_ascii_case(id))
}

/// Parse "20mb", "20 MB", "500k", "1.5gb", a preset id ("discord"), or plain bytes ("1048576").
pub fn parse_size(input: &str) -> Result<u64> {
    let s = input.trim().to_ascii_lowercase();
    if let Some(p) = preset(&s) {
        return Ok(p.bytes);
    }
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: f64 = num.parse().map_err(|_| Error::InvalidSize(input.into()))?;
    let mult = match unit.trim() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => KB,
        "m" | "mb" | "mib" => MB,
        "g" | "gb" | "gib" => GB,
        _ => return Err(Error::InvalidSize(input.into())),
    };
    let bytes = value * mult as f64;
    if !bytes.is_finite() || bytes < 1.0 {
        return Err(Error::InvalidSize(input.into()));
    }
    Ok(bytes as u64)
}

/// "48.2 MB"-style display.
pub fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes >= GB {
        format!("{:.2} GB", b / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", b / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", b / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units_and_presets() {
        assert_eq!(parse_size("20mb").unwrap(), 20 * MB);
        assert_eq!(parse_size(" 20 MB ").unwrap(), 20 * MB);
        assert_eq!(parse_size("500k").unwrap(), 500 * KB);
        assert_eq!(parse_size("1.5gb").unwrap(), (1.5 * GB as f64) as u64);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("discord").unwrap(), 20 * MB);
        assert!(parse_size("twenty").is_err());
        assert!(parse_size("20tb").is_err());
        assert!(parse_size("0").is_err());
    }

    #[test]
    fn human_readable() {
        assert_eq!(human(48 * MB + MB / 5), "48.2 MB");
        assert_eq!(human(512), "512 B");
    }
}
