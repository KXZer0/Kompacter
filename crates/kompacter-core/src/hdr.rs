//! HDR to SDR. Phone and console HDR clips (PQ or HLG) look grey and washed out once they
//! become ordinary video, so their colours are tone-mapped to plain BT.709 first. This
//! needs ffmpeg's `zscale` (libzimg) and `tonemap` filters; a build without them encodes
//! the picture as before.

use std::ffi::OsString;
use std::process::Stdio;

use crate::ffmpeg::{args, command, Ffmpeg};

/// To linear light, squeeze the highlights with the Hable curve, back to BT.709 at 8-bit
/// 4:2:0, and say so on the frames (newer ffmpeg tags the output from them). The input's curve, primaries and matrix come from the decoded frames; a file
/// that tags only some of them makes zscale refuse, and the jobs then encode as before.
pub(crate) const TONEMAP: &str = "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,\
     tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p,\
     setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv";

/// Output flags that tag the result as plain SDR BT.709, so players don't treat it as HDR.
pub(crate) fn sdr_tags() -> Vec<OsString> {
    args([
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
        "-colorspace",
        "bt709",
        "-color_range",
        "tv",
    ])
}

/// True when this ffmpeg has the filters tone-mapping needs. Asks ffmpeg once (~50 ms);
/// `Engine` caches the answer.
pub(crate) fn detect(ff: &Ffmpeg) -> bool {
    command(&ff.ffmpeg)
        .args(["-hide_banner", "-filters"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success() && has_filters(&String::from_utf8_lossy(&o.stdout)))
}

/// Reads `ffmpeg -filters` (lines like ` .SC zscale  V->V  Apply resizing, ...`).
fn has_filters(list: &str) -> bool {
    let names: Vec<&str> = list
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();
    ["zscale", "tonemap"].iter().all(|f| names.contains(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_filters_in_the_list() {
        let full = "Filters:\n  T.. = Timeline support\n ... = Source or sink filter\n \
                    TS. colorspace        V->V       Convert between colorspaces.\n \
                    .S. tonemap           V->V       Conversion to/from different dynamic ranges.\n \
                    ... tonemap_opencl    V->V       Perform HDR to SDR conversion with tonemapping.\n \
                    .SC zscale            V->V       Apply resizing, colorspace and bit depth conversion.\n";
        assert!(has_filters(full));
        // No libzimg: only the OpenCL tonemap and no zscale.
        let no_zimg = "Filters:\n .S. tonemap  V->V  x\n ... tonemap_opencl  V->V  x\n";
        assert!(!has_filters(no_zimg));
        assert!(!has_filters(""));
    }

    #[test]
    fn chain_ends_in_8bit_bt709() {
        assert!(TONEMAP.starts_with("zscale=t=linear"));
        assert!(TONEMAP.contains("zscale=t=bt709:m=bt709:r=tv,format=yuv420p,"));
        // The frames say BT.709 too, which newer ffmpeg uses to tag the output.
        assert!(TONEMAP.ends_with(
            "setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv"
        ));
        assert!(
            !TONEMAP.contains(' '),
            "a filter chain has no spaces: {TONEMAP}"
        );
    }
}
