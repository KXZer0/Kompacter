//! Human-friendly text: sizes, durations, dashed outlines.

const KB: f64 = 1024.0;
const MB: f64 = KB * 1024.0;
const GB: f64 = MB * 1024.0;

/// "48.2 MB"-style sizes, in the same binary units the engine uses.
pub fn bytes(b: u64) -> String {
    let b = b as f64;
    let trim = |s: String| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    if b >= GB {
        format!("{} GB", trim(format!("{:.2}", b / GB)))
    } else if b >= 100.0 * MB {
        format!("{} MB", (b / MB).round())
    } else if b >= MB {
        format!("{} MB", trim(format!("{:.1}", b / MB)))
    } else {
        format!("{} KB", (b / KB).round())
    }
}

/// "1:23" from seconds.
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// "About 40 s left" / "About 2 min left".
pub fn seconds_left(s: f64) -> String {
    let s = s.ceil().max(1.0);
    if s >= 90.0 {
        format!("About {} min left", (s / 60.0).round())
    } else {
        format!("About {s} s left")
    }
}

/// "1:05.250": trim times, to the millisecond.
pub fn clock_ms(secs: f64) -> String {
    let ms = (secs.max(0.0) * 1000.0).round() as u64;
    format!("{}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

/// Reads a typed time: "1:05.250", "65.25", "0:01:05.250". `None` if it isn't one.
pub fn parse_time(s: &str) -> Option<f64> {
    let mut secs = 0.0;
    for part in s.trim().split(':') {
        let v: f64 = part.trim().parse().ok()?;
        if v < 0.0 {
            return None;
        }
        secs = secs * 60.0 + v;
    }
    Some(secs)
}

/// "2.4 Mbps" / "900 kbps".
pub fn rate(kbps: u32) -> String {
    if kbps >= 1000 {
        let m = format!("{:.1}", kbps as f64 / 1000.0);
        format!("{} Mbps", m.trim_end_matches(".0"))
    } else {
        format!("{kbps} kbps")
    }
}

pub fn file_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// SVG path of evenly dashed strokes around a rounded rectangle (for the drop zone).
/// Slint paths have no dash style, so the dashes are drawn as short segments.
pub fn dashed_round_rect(w: f64, h: f64, r: f64, dash: f64, gap: f64, offset: f64) -> String {
    use std::f64::consts::PI;
    let inset = 1.0;
    let (w, h, r) = (w - 2.0 * inset, h - 2.0 * inset, r - inset);
    // Walk the outline in small steps and collect points.
    let mut pts: Vec<(f64, f64)> = Vec::new();
    let corner = |cx: f64, cy: f64, a0: f64, pts: &mut Vec<(f64, f64)>| {
        for i in 0..=12 {
            let a = a0 + PI / 2.0 * i as f64 / 12.0;
            pts.push((cx + r * a.cos(), cy + r * a.sin()));
        }
    };
    let o = inset;
    corner(o + w - r, o + r, -PI / 2.0, &mut pts);
    corner(o + w - r, o + h - r, 0.0, &mut pts);
    corner(o + r, o + h - r, PI / 2.0, &mut pts);
    corner(o + r, o + r, PI, &mut pts);
    pts.push(pts[0]);

    let total: f64 = pts
        .windows(2)
        .map(|s| (s[1].0 - s[0].0).hypot(s[1].1 - s[0].1))
        .sum();
    // Fit a whole number of dash+gap periods so the seam is invisible.
    let n = (total / (dash + gap)).round().max(1.0);
    let period = total / n;
    let dash = period * dash / (dash + gap);

    let mut out = String::new();
    // Distance along the outline; the offset makes the dashes march.
    let mut pos = period - offset.rem_euclid(period);
    let mut drawing = false;
    for s in pts.windows(2) {
        let (a, b) = (s[0], s[1]);
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        let mut t = 0.0;
        while t < len {
            let phase = (pos + t) % period;
            let on = phase < dash;
            let step = if on { dash - phase } else { period - phase }.min(len - t);
            let p = (a.0 + (b.0 - a.0) * t / len, a.1 + (b.1 - a.1) * t / len);
            if on && !drawing {
                out += &format!("M{:.1} {:.1}", p.0, p.1);
            }
            let e = t + step;
            let q = (a.0 + (b.0 - a.0) * e / len, a.1 + (b.1 - a.1) * e / len);
            if on {
                out += &format!("L{:.1} {:.1}", q.0, q.1);
            }
            drawing = on;
            t = e.max(t + 1e-6);
        }
        pos += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(bytes(20 * 1024 * 1024), "20 MB");
        assert_eq!(bytes(128 * 1024 * 1024), "128 MB");
        assert_eq!(bytes(1536 * 1024), "1.5 MB");
        assert_eq!(bytes(2048), "2 KB");
    }

    #[test]
    fn trim_times() {
        assert_eq!(clock_ms(65.25), "1:05.250");
        assert_eq!(clock_ms(0.0), "0:00.000");
        assert_eq!(parse_time("1:05.250"), Some(65.25));
        assert_eq!(parse_time("12.5"), Some(12.5));
        assert_eq!(parse_time("1:00:00"), Some(3600.0));
        assert_eq!(parse_time("abc"), None);
    }

    #[test]
    fn dashes() {
        let d = dashed_round_rect(308.0, 300.0, 28.0, 6.0, 7.0, 3.0);
        assert!(d.starts_with('M'));
        assert!(d.matches('M').count() > 50);
    }
}
