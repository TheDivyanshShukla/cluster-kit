//! Agent A: media probing results, fingerprints, availability.
//!
//! Missing files: `probe` returns ApiError `not_found`; `missing_asset` builds a
//! `MediaAsset` with `Availability::Missing` for callers that must keep the row.

use crate::watch::domain::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;

const MIB: u64 = 1 << 20;
const STILL_EXT: [&str; 6] = ["jpg", "jpeg", "png", "webp", "bmp", "gif"];

/// Stable media ID for a (source, fingerprint) pair.
pub fn media_id(source_id: &str, fingerprint: &str) -> String {
    stable_id("media", &[source_id, fingerprint])
}

/// Fingerprint method `sha256-1m`: SHA-256 over the file size (u64 LE), the first 1 MiB, and the
/// last 1 MiB (not overlapping the head; files <= 1 MiB hash fully once). Cheap on huge
/// recordings; a change confined to the middle of a >2 MiB file would go unnoticed.
pub fn fingerprint(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    let mut h = Sha256::new();
    h.update(size.to_le_bytes());
    let head = size.min(MIB);
    let mut buf = vec![0u8; head as usize];
    f.read_exact(&mut buf)?;
    h.update(&buf);
    let tail_start = size.saturating_sub(MIB).max(head);
    if tail_start < size {
        f.seek(SeekFrom::Start(tail_start))?;
        let mut t = vec![0u8; (size - tail_start) as usize];
        f.read_exact(&mut t)?;
        h.update(&t);
    }
    Ok(format!("sha256-1m:{}", h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

fn is_still(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).map(|e| STILL_EXT.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false)
}

fn uri_of(path: &Path) -> String {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().into_owned()
}

/// Row for a file that is gone: availability Missing, fingerprint derived from the path.
pub fn missing_asset(source_id: &str, path: &Path) -> MediaAsset {
    let uri = uri_of(path);
    let fp = format!("missing:{uri}");
    MediaAsset {
        id: media_id(source_id, &fp), source_id: source_id.into(), uri, fingerprint: fp,
        duration_us: None, width: None, height: None, codec: None, timebase: None, capture_start_utc_us: None,
        proxy: ProxyStatus::None, availability: Availability::Missing, still: is_still(path), node_id: None,
    }
}

pub fn probe(source_id: &str, path: &Path) -> Result<MediaAsset, ApiError> {
    probe_with(source_id, path, None)
}

/// ffprobe + fingerprint. `creation_time` tags are NOT trusted as capture time; only
/// `capture_start_utc_us` passed by the caller is recorded.
pub fn probe_with(source_id: &str, path: &Path, capture_start_utc_us: Option<Us>) -> Result<MediaAsset, ApiError> {
    if !path.is_file() {
        return Err(ApiError::new("not_found", format!("media file not found: {}", path.display())));
    }
    let fp = fingerprint(path).map_err(|e| ApiError::new("internal", format!("fingerprint: {e}")))?;
    let out = run_ffprobe(path)?;
    let j: serde_json::Value = serde_json::from_slice(&out).map_err(|e| ApiError::new("parse_error", format!("ffprobe json: {e}")))?;
    let v = j["streams"].as_array().and_then(|s| s.iter().find(|s| s["codec_type"] == "video"))
        .ok_or_else(|| ApiError::new("invalid_request", "no video stream"))?;
    let secs = |x: &serde_json::Value| x.as_str().and_then(|s| s.parse::<f64>().ok()).filter(|d| d.is_finite() && *d >= 0.0);
    let still = is_still(path);
    let duration_us = if still { None } else { secs(&j["format"]["duration"]).or_else(|| secs(&v["duration"])).map(|d| (d * 1e6).round() as i64) };
    Ok(MediaAsset {
        id: media_id(source_id, &fp),
        source_id: source_id.into(),
        uri: uri_of(path),
        fingerprint: fp,
        duration_us,
        width: v["width"].as_u64().map(|n| n as u32),
        height: v["height"].as_u64().map(|n| n as u32),
        codec: v["codec_name"].as_str().map(String::from),
        timebase: v["time_base"].as_str().map(String::from),
        capture_start_utc_us,
        proxy: ProxyStatus::None,
        availability: Availability::Available,
        still,
        node_id: None,
    })
}

fn run_ffprobe(path: &Path) -> Result<Vec<u8>, ApiError> {
    let env = std::env::var("CW_FFPROBE").ok();
    let cands: Vec<&str> = env.iter().map(|s| s.as_str()).chain(["ffprobe", "/opt/homebrew/bin/ffprobe", "/usr/local/bin/ffprobe", "/usr/bin/ffprobe"]).collect();
    for bin in cands {
        match Command::new(bin).args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"]).arg(path).output() {
            Ok(o) if o.status.success() => return Ok(o.stdout),
            Ok(o) => return Err(ApiError::new("invalid_request", format!("ffprobe failed: {}", String::from_utf8_lossy(&o.stderr).trim()))),
            Err(_) => continue,
        }
    }
    Err(ApiError::new("unavailable", "ffprobe not found (set CW_FFPROBE)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::storage::tests::tmp;

    fn footage() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../vision/footage/people-detection.mp4")
    }

    #[test]
    fn probe_real_video() {
        let m = probe("s1", &footage()).unwrap();
        assert!(m.duration_us.unwrap() > 1_000_000);
        assert!(m.width.unwrap() > 0 && m.height.unwrap() > 0);
        assert!(m.codec.is_some() && m.timebase.as_deref().unwrap().contains('/'));
        assert_eq!(m.capture_start_utc_us, None);
        assert!(!m.still && m.availability == Availability::Available);
        assert_eq!(m.id, media_id("s1", &m.fingerprint));
        assert_eq!(probe("s1", &footage()).unwrap().fingerprint, m.fingerprint);
        assert_eq!(probe_with("s1", &footage(), Some(5)).unwrap().capture_start_utc_us, Some(5));
    }

    #[test]
    fn missing_file_is_not_found_and_missing_asset() {
        let p = tmp("mc").join("gone.mp4");
        assert_eq!(probe("s", &p).unwrap_err().code, "not_found");
        assert_eq!(missing_asset("s", &p).availability, Availability::Missing);
    }

    #[test]
    fn fingerprint_depends_on_content_and_size() {
        let d = tmp("fp");
        let (a, b) = (d.join("a"), d.join("b"));
        std::fs::write(&a, vec![1u8; 3 * MIB as usize]).unwrap();
        std::fs::write(&b, vec![1u8; 3 * MIB as usize + 1]).unwrap();
        assert_ne!(fingerprint(&a).unwrap(), fingerprint(&b).unwrap());
        let mut v = vec![1u8; 3 * MIB as usize];
        *v.last_mut().unwrap() = 9;
        std::fs::write(&b, v).unwrap();
        assert_ne!(fingerprint(&a).unwrap(), fingerprint(&b).unwrap());
        std::fs::write(&b, vec![7u8; 10]).unwrap();
        assert!(fingerprint(&b).is_ok());
    }

    #[test]
    fn still_image_flagged() {
        let p = tmp("still").join("x.png");
        let ok = Command::new("/opt/homebrew/bin/ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", "color=c=red:s=16x16", "-frames:v", "1", "-y"]).arg(&p).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            return;
        }
        let m = probe("s", &p).unwrap();
        assert!(m.still && m.duration_us.is_none() && m.width == Some(16));
    }
}
