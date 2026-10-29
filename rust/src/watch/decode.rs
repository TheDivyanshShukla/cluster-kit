//! ffmpeg writes PPM frames on a pipe. The header carries the size, so a scale
//! filter cannot desynchronise the reader. `-threads 1` keeps the cores for the detector.

use std::io::{self, BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct Probe {
    pub fps: f64,
    pub duration: f64,
}

pub fn ffmpeg_args(url: &str, kind: &str, ss: f64, width: u32) -> Vec<String> {
    let width = (width.clamp(320, 1280) / 2) * 2; // motion mode decodes wide and cuts crops from it
    let mut a = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
        "-threads".into(),
        "1".into(),
    ];
    match kind {
        "live" => {
            if url.starts_with("rtsp") {
                a.extend(["-rtsp_transport".into(), "tcp".into()]);
            }
            a.extend(["-i".into(), url.into()]);
        }
        "webcam" => {
            if cfg!(target_os = "macos") {
                a.extend(["-f".into(), "avfoundation".into(), "-framerate".into(), "30".into(), "-i".into(), format!("{url}:none")]);
            } else if cfg!(windows) {
                a.extend(["-f".into(), "dshow".into(), "-i".into(), format!("video={url}")]);
            } else {
                a.extend(["-f".into(), "v4l2".into(), "-i".into(), format!("/dev/video{url}")]);
            }
        }
        "archive" | "image" => {
            if ss > 0.0 { a.extend(["-ss".into(), format!("{ss:.3}")]); }
            a.extend(["-i".into(), url.into()]);
        }
        _ => {
            a.extend(["-re".into(), "-stream_loop".into(), "-1".into()]);
            if ss > 0.05 {
                a.extend(["-ss".into(), format!("{ss:.3}")]);
            }
            a.extend(["-i".into(), url.into()]);
        }
    }
    a.extend([
        "-an".into(),
        "-sn".into(),
        "-vf".into(),
        format!("scale={width}:-2"),
        "-pix_fmt".into(),
        "rgb24".into(),
        "-f".into(),
        "image2pipe".into(),
        "-vcodec".into(),
        "ppm".into(),
        "pipe:1".into(),
    ]);
    a
}

pub fn probe(path: &str) -> Probe {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "format=duration:stream=avg_frame_rate",
            "-of",
            "json",
            path,
        ])
        .output();
    let Ok(out) = out else { return Probe { fps: 25.0, duration: 0.0 } };
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::json!({}));
    let fps = v["streams"]
        .as_array()
        .and_then(|s| s.first())
        .and_then(|s| s["avg_frame_rate"].as_str())
        .map(parse_rate)
        .filter(|f| *f > 1.0 && *f < 120.0)
        .unwrap_or(25.0);
    let duration = v["format"]["duration"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    Probe { fps, duration }
}

fn parse_rate(s: &str) -> f64 {
    let mut p = s.split('/');
    let n: f64 = p.next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
    let d: f64 = p.next().and_then(|x| x.parse().ok()).unwrap_or(1.0);
    if d == 0.0 { 0.0 } else { n / d }
}

/// Why a decode ended. Archives treat only `Eof` as success; everything else is surfaced, never swallowed.
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum DecodeEnd {
    /// Clean end of file after at least one frame.
    Eof,
    /// FFmpeg exited cleanly but produced no frames (empty stream, audio-only, zero-length file).
    NoFrames,
    MissingFile,
    /// FFmpeg could not parse or decode the input (corrupt data, unknown codec, text file named .mp4).
    UnsupportedCodec(String),
    Failed(String),
    Cancelled,
}

impl DecodeEnd {
    pub fn code(&self) -> &'static str {
        match self {
            DecodeEnd::Eof => "eof",
            DecodeEnd::NoFrames => "no_frames",
            DecodeEnd::MissingFile => "missing_file",
            DecodeEnd::UnsupportedCodec(_) => "unsupported_media",
            DecodeEnd::Failed(_) => "decoder_failed",
            DecodeEnd::Cancelled => "cancelled",
        }
    }
    pub fn message(&self) -> String {
        match self {
            DecodeEnd::Eof => "end of media".into(),
            DecodeEnd::NoFrames => "The decoder finished but the source contained no readable video frames.".into(),
            DecodeEnd::MissingFile => "The media file does not exist or is not readable. Check the path and that the drive is mounted on this PC.".into(),
            DecodeEnd::UnsupportedCodec(d) => format!("FFmpeg cannot decode this media (unsupported codec or corrupt/non-media data): {d}"),
            DecodeEnd::Failed(d) => format!("FFmpeg failed: {d}"),
            DecodeEnd::Cancelled => "cancelled".into(),
        }
    }
    /// Retrying the same input cannot help.
    pub fn permanent(&self) -> bool {
        matches!(self, DecodeEnd::MissingFile | DecodeEnd::UnsupportedCodec(_) | DecodeEnd::NoFrames)
    }
}

/// Local files must exist before FFmpeg is spawned, so "missing" is not guessed from stderr text later.
pub fn preflight(url: &str, kind: &str) -> Result<(), DecodeEnd> {
    let local = matches!(kind, "archive" | "image" | "file") && !url.contains("://") && !url.chars().all(|c| c.is_ascii_digit());
    if local && !std::path::Path::new(url).is_file() {
        return Err(DecodeEnd::MissingFile);
    }
    Ok(())
}

fn classify_stderr(tail: &str) -> DecodeEnd {
    let l = tail.to_ascii_lowercase();
    let last = tail.lines().rev().find(|x| !x.trim().is_empty()).unwrap_or("").trim().to_string();
    if l.contains("no such file") || l.contains("does not exist") {
        DecodeEnd::MissingFile
    } else if ["invalid data found", "unknown decoder", "decoder (codec", "not found", "could not find codec", "unsupported", "moov atom", "no decoder"].iter().any(|k| l.contains(k)) {
        DecodeEnd::UnsupportedCodec(last)
    } else {
        DecodeEnd::Failed(if last.is_empty() { "ffmpeg exited with an error".into() } else { last })
    }
}

type ChildSlot = Arc<Mutex<Option<Child>>>;

/// Kill and wait. Safe to call repeatedly, and after a poisoned lock; afterwards `try_wait` returns `Some`.
fn reap(slot: &ChildSlot) {
    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = g.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Drain stderr on a thread, keeping the last 2 KB for error classification.
fn drain_stderr(err: std::process::ChildStderr) -> (Arc<Mutex<String>>, std::thread::JoinHandle<()>) {
    let tail = Arc::new(Mutex::new(String::new()));
    let t2 = tail.clone();
    let h = std::thread::spawn(move || {
        let mut e = err;
        let mut buf = [0u8; 512];
        while let Ok(n) = e.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut t = t2.lock().unwrap_or_else(|x| x.into_inner());
            t.push_str(&String::from_utf8_lossy(&buf[..n]));
            if t.len() > 2048 {
                let cut = t.len() - 2048;
                let cut = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(t.len());
                t.drain(..cut);
            }
        }
    });
    (tail, h)
}

pub struct Feed {
    child: ChildSlot,
    stdout: std::process::ChildStdout,
    tail: Arc<Mutex<String>>,
    err_thread: Option<std::thread::JoinHandle<()>>,
}

impl Feed {
    pub fn open(url: &str, kind: &str, ss: f64, width: u32) -> io::Result<Self> {
        let mut child = Command::new("ffmpeg").args(ffmpeg_args(url, kind, ss, width)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
        let (tail, h) = match child.stderr.take() {
            Some(err) => {
                let (t, h) = drain_stderr(err);
                (t, Some(h))
            }
            None => (Arc::new(Mutex::new(String::new())), None),
        };
        Ok(Feed { child: Arc::new(Mutex::new(Some(child))), stdout, tail, err_thread: h })
    }

    pub fn next_frame(&mut self) -> io::Result<(u32, u32, Vec<u8>)> {
        read_ppm(&mut self.stdout)
    }

    /// After `next_frame` fails: wait for FFmpeg, then say why it ended. `frames` = frames read so far.
    pub fn end(&mut self, frames: u64) -> DecodeEnd {
        let status = {
            let mut g = self.child.lock().unwrap_or_else(|e| e.into_inner());
            match g.as_mut() {
                Some(c) => c.wait(),
                None => return DecodeEnd::Failed("decoder process missing".into()),
            }
        };
        if let Some(h) = self.err_thread.take() {
            let _ = h.join();
        }
        let tail = self.tail.lock().map(|t| t.clone()).unwrap_or_default();
        match status {
            Ok(s) if s.success() => if frames == 0 { DecodeEnd::NoFrames } else { DecodeEnd::Eof },
            Ok(_) => classify_stderr(&tail),
            Err(e) => DecodeEnd::Failed(e.to_string()),
        }
    }

    /// Stop decoding now: kill and reap the child.
    pub fn cancel(&mut self) {
        reap(&self.child);
    }

    /// Handle for tests and watchdogs.
    #[allow(dead_code)]
    pub fn child(&self) -> ChildSlot {
        self.child.clone()
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        reap(&self.child);
        if let Some(h) = self.err_thread.take() {
            let _ = h.join();
        }
    }
}

pub fn read_ppm(r: &mut impl Read) -> io::Result<(u32, u32, Vec<u8>)> {
    let mut magic = [0u8; 2];
    r.read_exact(&mut magic)?;
    if &magic != b"P6" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a ppm frame"));
    }
    let w: u32 = token(r)?.parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ppm width"))?;
    let h: u32 = token(r)?.parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ppm height"))?;
    let maxv: u32 = token(r)?.parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ppm max"))?;
    if maxv != 255 || w == 0 || h == 0 || w > 4000 || h > 4000 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "ppm size"));
    }
    let n = (w as usize) * (h as usize) * 3;
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok((w, h, buf))
}

fn token(r: &mut impl Read) -> io::Result<String> {
    let mut s = String::new();
    let mut b = [0u8; 1];
    loop {
        r.read_exact(&mut b)?;
        if b[0] == b'#' {
            while b[0] != b'\n' {
                r.read_exact(&mut b)?;
            }
            continue;
        }
        if b[0].is_ascii_whitespace() {
            if !s.is_empty() {
                return Ok(s);
            }
            continue;
        }
        s.push(b[0] as char);
        if s.len() > 16 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ppm token"));
        }
    }
}

/// A second ffmpeg plays the camera for the wall. It does not wait on the detector.
pub fn preview_args(url: &str, kind: &str, ss: f64, rate: u32) -> Vec<String> {
    let rate = if rate >= 45 { 60 } else { 30 };
    let mut a = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
        "-threads".into(),
        "1".into(),
    ];
    match kind {
        "live" => {
            if url.starts_with("rtsp") {
                a.extend(["-rtsp_transport".into(), "tcp".into(), "-fflags".into(), "nobuffer".into(), "-flags".into(), "low_delay".into()]);
            }
            a.extend(["-i".into(), url.into()]);
        }
        "webcam" => {
            if cfg!(target_os = "macos") {
                a.extend(["-f".into(), "avfoundation".into(), "-framerate".into(), "30".into(), "-i".into(), format!("{url}:none")]);
            } else if cfg!(windows) {
                a.extend(["-f".into(), "dshow".into(), "-i".into(), format!("video={url}")]);
            } else {
                a.extend(["-f".into(), "v4l2".into(), "-i".into(), format!("/dev/video{url}")]);
            }
        }
        _ => {
            a.extend(["-re".into(), "-stream_loop".into(), "-1".into()]);
            if ss > 0.05 {
                a.extend(["-ss".into(), format!("{ss:.3}")]);
            }
            a.extend(["-i".into(), url.into()]);
        }
    }
    a.extend([
        "-an".into(),
        "-sn".into(),
        "-vf".into(),
        format!("framerate=fps={rate},scale=1280:-2:flags=fast_bilinear"),
        "-q:v".into(),
        "4".into(),
        "-f".into(),
        "mjpeg".into(),
        "pipe:1".into(),
    ]);
    a
}

pub struct Motion {
    child: Arc<Mutex<Option<Child>>>,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Motion {
    pub fn open(url: &str, kind: &str, ss: f64, rate: u32) -> io::Result<Self> {
        let mut child = Command::new("ffmpeg").args(preview_args(url, kind, ss, rate)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut e = err;
                let mut buf = [0u8; 512];
                while e.read(&mut buf).ok().unwrap_or(0) > 0 {}
            });
        }
        Ok(Motion { child: Arc::new(Mutex::new(Some(child))), stdout: BufReader::with_capacity(64 * 1024, stdout) })
    }

    pub fn child(&self) -> Arc<Mutex<Option<Child>>> {
        self.child.clone()
    }

    pub fn next_jpeg(&mut self) -> io::Result<Vec<u8>> {
        read_jpeg(&mut self.stdout)
    }
}

impl Drop for Motion {
    fn drop(&mut self) {
        reap(&self.child);
    }
}

/// Pull the next JPEG out of a concatenated mjpeg stream.
pub fn read_jpeg(r: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut prev = 0u8;
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "preview ended"));
        }
        let mut soi = None;
        for i in 0..buf.len() {
            if prev == 0xFF && buf[i] == 0xD8 {
                soi = Some(i);
                break;
            }
            prev = buf[i];
        }
        if let Some(i) = soi {
            r.consume(i + 1);
            break;
        }
        let n = buf.len();
        r.consume(n);
    }
    let mut out = vec![0xFF, 0xD8];
    prev = 0xD8;
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "preview ended"));
        }
        let mut eoi = None;
        for i in 0..buf.len() {
            if prev == 0xFF && buf[i] == 0xD9 {
                eoi = Some(i);
                break;
            }
            prev = buf[i];
        }
        if let Some(i) = eoi {
            out.extend_from_slice(&buf[..=i]);
            r.consume(i + 1);
            return Ok(out);
        }
        out.extend_from_slice(buf);
        if out.len() > 2_000_000 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "preview frame too large"));
        }
        let n = buf.len();
        r.consume(n);
    }
}

#[allow(dead_code)]
pub fn jpeg(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let enc = jpeg_encoder::Encoder::new(&mut out, 72);
    if w == 0 || h == 0 || rgb.len() < (w as usize) * (h as usize) * 3 {
        return out;
    }
    let _ = enc.encode(rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb);
    out
}

/// A bounded JPEG of the exact inference frame for evidence review.
pub fn thumbnail(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
    if w == 0 || h == 0 || rgb.len() < w as usize * h as usize * 3 { return Vec::new(); }
    // Large enough for a wall tile: the live view draws this exact analyzed frame with its boxes.
    let scale = (640.0 / w.max(h) as f64).min(1.0);
    let tw = (w as f64 * scale).round().max(1.0) as u32;
    let th = (h as f64 * scale).round().max(1.0) as u32;
    let mut px = Vec::with_capacity(tw as usize * th as usize * 3);
    for y in 0..th {
        for x in 0..tw {
            let sx = (x * w / tw).min(w - 1);
            let sy = (y * h / th).min(h - 1);
            let at = ((sy * w + sx) * 3) as usize;
            px.extend_from_slice(&rgb[at..at + 3]);
        }
    }
    jpeg(&px, tw, th)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_args_pace_in_realtime_and_rtsp_does_not() {
        let file = ffmpeg_args("vision/footage/a.mp4", "file", 1.5, 640);
        assert!(file.iter().any(|a| a == "-re"));
        assert!(file.iter().any(|a| a == "-stream_loop"));
        assert!(file.iter().any(|a| a == "scale=640:-2"));
        let live = ffmpeg_args("rtsp://10.0.0.5/live", "live", 0.0, 416);
        assert!(!live.iter().any(|a| a == "-re"));
        assert!(live.iter().any(|a| a == "scale=416:-2"));
        assert!(live.windows(2).any(|w| w == ["-rtsp_transport", "tcp"]));
        let preview = preview_args("vision/footage/a.mp4", "file", 0.0, 60);
        assert!(preview.iter().any(|a| a == "-re"));
        assert!(preview.iter().any(|a| a == "mjpeg"));
        assert!(preview.iter().any(|a| a == "framerate=fps=60,scale=1280:-2:flags=fast_bilinear"));
    }

    #[test]
    fn archives_and_images_finish_without_looping_or_realtime_pacing() {
        for kind in ["archive", "image"] {
            let args = ffmpeg_args("input.mp4", kind, 0.0, 640);
            assert!(!args.iter().any(|a| a == "-re" || a == "-stream_loop"));
        }
    }

    #[test]
    fn mjpeg_frames_split_on_markers() {
        let raw = [0x00u8, 0xFF, 0xD8, 0x01, 0x02, 0xFF, 0xD9, 0xFF, 0xD8, 0x03, 0xFF, 0xD9];
        let mut cur = std::io::Cursor::new(raw);
        assert_eq!(read_jpeg(&mut cur).unwrap(), vec![0xFF, 0xD8, 0x01, 0x02, 0xFF, 0xD9]);
        assert_eq!(read_jpeg(&mut cur).unwrap(), vec![0xFF, 0xD8, 0x03, 0xFF, 0xD9]);
    }

    const FOOTAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../vision/footage/people-detection.mp4");

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cw-decode-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn real_decode_then_cancel_reaps_the_child() {
        let mut feed = Feed::open(FOOTAGE, "archive", 0.0, 320).unwrap();
        let mut n = 0u64;
        for _ in 0..5 {
            let (w, h, px) = feed.next_frame().unwrap();
            assert_eq!(px.len(), (w * h * 3) as usize);
            n += 1;
        }
        assert_eq!(n, 5);
        let child = feed.child();
        assert!(child.lock().unwrap().as_mut().unwrap().try_wait().unwrap().is_none(), "still decoding");
        feed.cancel();
        assert!(child.lock().unwrap().as_mut().unwrap().try_wait().unwrap().is_some(), "killed and reaped, no zombie");
        // Dropping a second feed mid-stream reaps too.
        let feed2 = Feed::open(FOOTAGE, "archive", 0.0, 320).unwrap();
        let child2 = feed2.child();
        drop(feed2);
        assert!(child2.lock().unwrap().as_mut().unwrap().try_wait().unwrap().is_some());
    }

    #[test]
    fn bad_media_missing_file_and_clean_eof_are_distinct() {
        let bad = tmp("bad.mp4");
        std::fs::write(&bad, "this is not a video, just text\n").unwrap();
        let mut f = Feed::open(bad.to_str().unwrap(), "archive", 0.0, 320).unwrap();
        assert!(f.next_frame().is_err());
        let end = f.end(0);
        assert!(matches!(end, DecodeEnd::UnsupportedCodec(_)), "{end:?}");
        assert!(end.permanent());

        let gone = tmp("gone.mp4");
        assert_eq!(preflight(gone.to_str().unwrap(), "archive"), Err(DecodeEnd::MissingFile));
        // Even if preflight were skipped, FFmpeg's own error classifies as missing.
        let mut f = Feed::open(gone.to_str().unwrap(), "archive", 0.0, 320).unwrap();
        assert!(f.next_frame().is_err());
        assert_eq!(f.end(0), DecodeEnd::MissingFile);

        let png = tmp("one.png");
        let ok = Command::new("ffmpeg").args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=red:s=64x64", "-frames:v", "1"]).arg(&png).status().unwrap();
        assert!(ok.success());
        let mut f = Feed::open(png.to_str().unwrap(), "image", 0.0, 320).unwrap();
        f.next_frame().unwrap();
        assert!(f.next_frame().is_err());
        assert_eq!(f.end(1), DecodeEnd::Eof);
        let mut f = Feed::open(png.to_str().unwrap(), "image", 0.0, 320).unwrap();
        assert!(f.next_frame().is_ok());
        assert!(f.next_frame().is_err());
        assert_eq!(f.end(0), DecodeEnd::NoFrames, "clean exit with nothing counted is NoFrames");
    }

    #[test]
    fn ppm_round_trip_header() {
        let mut raw = b"P6\n2 2\n255\n".to_vec();
        raw.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 1, 2, 3]);
        let (w, h, px) = read_ppm(&mut &raw[..]).unwrap();
        assert_eq!((w, h, px.len()), (2, 2, 12));
        assert_eq!(px[0], 255);
    }
}

/// Video capture devices on this machine, as both index ("0") and name, so a webcam source can name either.
/// macOS: FFmpeg avfoundation listing. Linux: /dev/video*. Windows: FFmpeg dshow listing.
pub fn capture_devices() -> Vec<String> {
    if cfg!(target_os = "linux") {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir("/dev") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Some(n) = name.strip_prefix("video").filter(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty()) {
                    out.push(n.to_string());
                    out.push(format!("/dev/{name}"));
                }
            }
        }
        out.sort();
        return out;
    }
    let (fmt, input) = if cfg!(target_os = "windows") { ("dshow", "dummy") } else { ("avfoundation", "") };
    let Ok(o) = Command::new("ffmpeg").args(["-hide_banner", "-f", fmt, "-list_devices", "true", "-i", input]).stdin(Stdio::null()).output() else { return Vec::new() };
    parse_device_list(&String::from_utf8_lossy(&o.stderr), cfg!(target_os = "windows"))
}

fn parse_device_list(text: &str, dshow: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut video = !dshow;
    for line in text.lines() {
        if line.contains("audio devices") {
            video = false;
            continue;
        }
        if line.contains("video devices") {
            video = true;
            continue;
        }
        if !video {
            continue;
        }
        if dshow {
            // dshow: [dshow @ ..] "Integrated Camera" (video)
            if line.contains("(video)") {
                if let Some(name) = line.split('"').nth(1) {
                    out.push(format!("{}", out.len() / 2));
                    out.push(name.to_string());
                }
            }
        } else if let Some(rest) = line.split("] [").nth(1) {
            // avfoundation: [AVFoundation indev @ 0x..] [0] MacBook Air Camera
            if let Some((idx, name)) = rest.split_once("] ") {
                if idx.chars().all(|c| c.is_ascii_digit()) && !name.starts_with("Capture screen") {
                    out.push(idx.to_string());
                    out.push(name.trim().to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod device_tests {
    #[test]
    fn avfoundation_listing_gives_video_devices_only() {
        let text = "[AVFoundation indev @ 0x1] AVFoundation video devices:\n[AVFoundation indev @ 0x1] [0] MacBook Air Camera\n[AVFoundation indev @ 0x1] [4] Capture screen 0\n[AVFoundation indev @ 0x1] AVFoundation audio devices:\n[AVFoundation indev @ 0x1] [0] AirPods\n";
        assert_eq!(super::parse_device_list(text, false), vec!["0".to_string(), "MacBook Air Camera".to_string()]);
    }
}
