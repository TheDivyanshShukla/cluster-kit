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
    let width = (width.clamp(320, 640) / 2) * 2;
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

pub struct Feed {
    child: Arc<Mutex<Option<Child>>>,
    stdout: std::process::ChildStdout,
}

impl Feed {
    pub fn open(url: &str, kind: &str, ss: f64, width: u32) -> io::Result<Self> {
        let mut child = Command::new("ffmpeg").args(ffmpeg_args(url, kind, ss, width)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut e = err;
                let mut buf = [0u8; 512];
                while e.read(&mut buf).ok().unwrap_or(0) > 0 {}
            });
        }
        Ok(Feed { child: Arc::new(Mutex::new(Some(child))), stdout })
    }

    pub fn next_frame(&mut self) -> io::Result<(u32, u32, Vec<u8>)> {
        read_ppm(&mut self.stdout)
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        if let Ok(mut g) = self.child.lock() {
            if let Some(c) = g.as_mut() {
                let _ = c.kill();
            }
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
        if let Ok(mut g) = self.child.lock() {
            if let Some(c) = g.as_mut() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
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
    fn mjpeg_frames_split_on_markers() {
        let raw = [0x00u8, 0xFF, 0xD8, 0x01, 0x02, 0xFF, 0xD9, 0xFF, 0xD8, 0x03, 0xFF, 0xD9];
        let mut cur = std::io::Cursor::new(raw);
        assert_eq!(read_jpeg(&mut cur).unwrap(), vec![0xFF, 0xD8, 0x01, 0x02, 0xFF, 0xD9]);
        assert_eq!(read_jpeg(&mut cur).unwrap(), vec![0xFF, 0xD8, 0x03, 0xFF, 0xD9]);
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
