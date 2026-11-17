//! 3D models: glTF binary (.glb) and glTF with embedded data (.gltf). Most of a model's
//! size is usually its textures, so those are resized and recompressed (like the photos in
//! a document) and the file is rebuilt; it stays a normal model that opens the same way.
//! Steps down until it fits. Meshes are left exactly as they are.
//!
//! Opaque textures become JPEG; textures with transparency stay PNG (only resized).
//! Both are core glTF, so every viewer reads them. A texture is only replaced when the
//! new one is smaller. Other formats (OBJ, STL, FBX, ...) are packed into an archive,
//! which shrinks text formats like OBJ and ASCII STL a lot.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{file_size, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, Progress};

/// (longest side in px, JPEG `-q:v`), best-looking first.
const STEPS: [(u32, u32); 6] = [
    (2048, 3),
    (2048, 6),
    (1536, 8),
    (1024, 10),
    (768, 14),
    (512, 18),
];
/// Textures smaller than this aren't worth touching.
const MIN_BYTES: usize = 32 * 1024;
const GLB_MAGIC: u32 = 0x4654_6C67; // "glTF"
const CHUNK_JSON: u32 = 0x4E4F_534A; // "JSON"
const CHUNK_BIN: u32 = 0x004E_4942; // "BIN\0"

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Model {
    Glb,
    Gltf,
}

/// Models we know how to shrink, by extension.
pub(crate) fn kind(path: &Path) -> Option<Model> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "glb" => Some(Model::Glb),
        "gltf" => Some(Model::Gltf),
        _ => None,
    }
}

/// Shrink the model's textures until it fits `target`.
pub(crate) fn fit(
    ctx: &Ctx,
    path: &Path,
    model: Model,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let src = std::fs::read(path)?;
    let (ext, parsed) = match model {
        Model::Glb => ("glb", read_glb(&src)?),
        Model::Gltf => ("gltf", read_gltf(&src)?),
    };
    if parsed.images.is_empty() {
        return Err(Error::Unsupported(
            "no textures to shrink in this model".into(),
        ));
    }
    let out = with_ext(out_base, ext);
    let tmp = tempfile::tempdir()?;
    let mut smallest = u64::MAX;
    for (i, &(side, q)) in STEPS.iter().enumerate() {
        let images = parsed
            .images
            .iter()
            .enumerate()
            .map(|(n, img)| shrink_texture(ctx, img, tmp.path(), n, side, q))
            .collect::<Result<Vec<_>>>()?;
        let bytes = match model {
            Model::Glb => write_glb(&parsed, &images)?,
            Model::Gltf => write_gltf(&parsed, &images)?,
        };
        std::fs::write(&out, bytes)?;
        progress((i + 1) as f32 / STEPS.len() as f32);
        let size = file_size(&out)?;
        if size <= target {
            return Ok(out);
        }
        smallest = smallest.min(size);
    }
    let _ = std::fs::remove_file(&out);
    Err(Error::DoesNotFit {
        what: name_of(path),
        target,
        best: smallest,
    })
}

/// A texture found in the model.
struct Texture {
    /// Index in the glTF `images` array.
    index: usize,
    data: Vec<u8>,
    png: bool,
}

/// What a step decided for one texture: new bytes and their MIME type, or `None` to keep it.
type Replacement = Option<(Vec<u8>, &'static str)>;

struct Parsed {
    json: Value,
    /// GLB only: the binary chunk.
    bin: Vec<u8>,
    images: Vec<Texture>,
}

fn bad(why: &str) -> Error {
    Error::Unsupported(format!("can't read this model: {why}"))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| bad("cut short"))
}

fn read_glb(src: &[u8]) -> Result<Parsed> {
    if u32_at(src, 0)? != GLB_MAGIC || u32_at(src, 4)? != 2 {
        return Err(bad("not a glTF 2.0 binary"));
    }
    let mut at = 12;
    let (mut json, mut bin) = (None, Vec::new());
    while at + 8 <= src.len() {
        let len = u32_at(src, at)? as usize;
        let ty = u32_at(src, at + 4)?;
        let body = src
            .get(at + 8..at + 8 + len)
            .ok_or_else(|| bad("cut short"))?;
        match ty {
            CHUNK_JSON => {
                json = Some(serde_json::from_slice::<Value>(body).map_err(|_| bad("broken JSON"))?)
            }
            CHUNK_BIN if bin.is_empty() => bin = body.to_vec(),
            _ => {}
        }
        at += 8 + len;
    }
    let json = json.ok_or_else(|| bad("no JSON chunk"))?;
    let mut images = Vec::new();
    let views = json["bufferViews"].as_array().cloned().unwrap_or_default();
    for (index, img) in json["images"].as_array().into_iter().flatten().enumerate() {
        let png = match img["mimeType"].as_str() {
            Some("image/png") => true,
            Some("image/jpeg") => false,
            _ => continue,
        };
        let Some(view) = img["bufferView"]
            .as_u64()
            .and_then(|v| views.get(v as usize))
        else {
            continue;
        };
        if view["buffer"].as_u64().unwrap_or(0) != 0 {
            continue;
        }
        let off = view["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = view["byteLength"].as_u64().unwrap_or(0) as usize;
        if let Some(data) = bin.get(off..off + len) {
            if data.len() >= MIN_BYTES {
                images.push(Texture {
                    index,
                    data: data.to_vec(),
                    png,
                });
            }
        }
    }
    Ok(Parsed { json, bin, images })
}

fn read_gltf(src: &[u8]) -> Result<Parsed> {
    let json: Value = serde_json::from_slice(src).map_err(|_| bad("broken JSON"))?;
    let mut images = Vec::new();
    for (index, img) in json["images"].as_array().into_iter().flatten().enumerate() {
        let Some(uri) = img["uri"].as_str() else {
            continue;
        };
        let (png, b64) = if let Some(rest) = uri.strip_prefix("data:image/png;base64,") {
            (true, rest)
        } else if let Some(rest) = uri.strip_prefix("data:image/jpeg;base64,") {
            (false, rest)
        } else {
            continue;
        };
        if let Some(data) = base64_decode(b64) {
            if data.len() >= MIN_BYTES {
                images.push(Texture { index, data, png });
            }
        }
    }
    Ok(Parsed {
        json,
        bin: Vec::new(),
        images,
    })
}

/// Re-encode one texture no bigger than `side`: JPEG if opaque, PNG if it has alpha.
fn shrink_texture(
    ctx: &Ctx,
    tex: &Texture,
    tmp: &Path,
    n: usize,
    side: u32,
    q: u32,
) -> Result<Replacement> {
    if ctx.cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let input = tmp.join(format!("in{n}.{}", if tex.png { "png" } else { "jpg" }));
    std::fs::write(&input, &tex.data)?;
    let Ok(Some(info)) = ctx.ff.probe_json(&input) else {
        return Ok(None);
    };
    let s = &info["streams"][0];
    let (Some(w), Some(h)) = (s["width"].as_u64(), s["height"].as_u64()) else {
        return Ok(None);
    };
    let pix = s["pix_fmt"].as_str().unwrap_or("");
    let alpha = tex.png && (pix.contains('a') || pix.starts_with("pal"));
    let k = (side as f64 / w.max(h).max(1) as f64).min(1.0);
    let even = |x: f64| ((x / 2.0).round() as u64 * 2).max(2);
    let (tw, th) = (even(w as f64 * k), even(h as f64 * k));
    let out = tmp.join(format!("out{n}.{}", if alpha { "png" } else { "jpg" }));
    let mut a = args(["-i".into(), input.as_os_str().to_owned()]);
    a.extend(args(["-frames:v", "1", "-vf"]));
    a.push(format!("scale={tw}:{th}:flags=lanczos").into());
    if alpha {
        a.extend(args([
            "-c:v",
            "png",
            "-pix_fmt",
            "rgba",
            "-compression_level",
            "9",
        ]));
    } else {
        a.extend(args(["-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-q:v"]));
        a.push(q.to_string().into());
    }
    a.push(out.clone().into());
    if ctx.ff.run(&a, 0.0, &mut |_| {}, ctx.cancel).is_err() {
        return Ok(None);
    }
    let bytes = std::fs::read(&out)?;
    if bytes.len() >= tex.data.len() {
        return Ok(None);
    }
    Ok(Some((
        bytes,
        if alpha { "image/png" } else { "image/jpeg" },
    )))
}

/// Rebuild the GLB: every buffer view of the binary chunk is laid out again in order,
/// with the new textures in place of the old ones.
fn write_glb(p: &Parsed, images: &[Replacement]) -> Result<Vec<u8>> {
    let mut json = p.json.clone();
    let mut swap: Vec<Option<&(Vec<u8>, &str)>> =
        vec![None; json["bufferViews"].as_array().map_or(0, Vec::len)];
    for (tex, new) in p.images.iter().zip(images) {
        if let Some(new) = new {
            let img = &mut json["images"][tex.index];
            if let Some(v) = img["bufferView"].as_u64() {
                swap[v as usize] = Some(new);
                img["mimeType"] = Value::from(new.1);
            }
        }
    }
    let mut bin = Vec::with_capacity(p.bin.len());
    if let Some(views) = json["bufferViews"].as_array_mut() {
        for (i, view) in views.iter_mut().enumerate() {
            if view["buffer"].as_u64().unwrap_or(0) != 0 {
                continue;
            }
            let off = view["byteOffset"].as_u64().unwrap_or(0) as usize;
            let len = view["byteLength"].as_u64().unwrap_or(0) as usize;
            let data: &[u8] = match swap[i] {
                Some((bytes, _)) => bytes,
                None => p
                    .bin
                    .get(off..off + len)
                    .ok_or_else(|| bad("buffer view out of range"))?,
            };
            // Accessors need their data 4-byte aligned.
            while !bin.len().is_multiple_of(4) {
                bin.push(0);
            }
            view["byteOffset"] = Value::from(bin.len());
            view["byteLength"] = Value::from(data.len());
            bin.extend_from_slice(data);
        }
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    if let Some(b) = json["buffers"].get_mut(0) {
        b["byteLength"] = Value::from(bin.len());
    }
    let mut text = serde_json::to_vec(&json).map_err(|_| bad("JSON"))?;
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    let total = 12 + 8 + text.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let mut out = Vec::with_capacity(total);
    out.extend(GLB_MAGIC.to_le_bytes());
    out.extend(2u32.to_le_bytes());
    out.extend((total as u32).to_le_bytes());
    out.extend((text.len() as u32).to_le_bytes());
    out.extend(CHUNK_JSON.to_le_bytes());
    out.extend(text);
    if !bin.is_empty() {
        out.extend((bin.len() as u32).to_le_bytes());
        out.extend(CHUNK_BIN.to_le_bytes());
        out.extend(bin);
    }
    Ok(out)
}

fn write_gltf(p: &Parsed, images: &[Replacement]) -> Result<Vec<u8>> {
    let mut json = p.json.clone();
    for (tex, new) in p.images.iter().zip(images) {
        if let Some((bytes, mime)) = new {
            json["images"][tex.index]["uri"] =
                Value::from(format!("data:{mime};base64,{}", base64_encode(bytes)));
        }
    }
    serde_json::to_vec(&json).map_err(|_| bad("JSON"))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in s.as_bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for n in 0..10 {
            let data: Vec<u8> = (0..n as u8).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
        assert_eq!(base64_encode(b"glTF"), "Z2xURg==");
    }
}
