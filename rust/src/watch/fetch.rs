use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use crate::watch::{log, repo_root};

struct Item {
    url: &'static str,
    rel: &'static str,
}

const ITEMS: &[Item] = &[
    Item {
        url: "https://github.com/ultralytics/yolov5/releases/download/v7.0/yolov5n.onnx",
        rel: "vision/models/yolov5n.onnx",
    },
    Item {
        url: "https://github.com/intel-iot-devkit/sample-videos/raw/master/person-bicycle-car-detection.mp4",
        rel: "vision/footage/person-bicycle-car-detection.mp4",
    },
    Item {
        url: "https://github.com/intel-iot-devkit/sample-videos/raw/master/people-detection.mp4",
        rel: "vision/footage/people-detection.mp4",
    },
];

pub fn run() -> Result<(), String> {
    let root = repo_root();
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(180)).redirects(10).build();
    for item in ITEMS {
        let dest = root.join(item.rel);
        if dest.is_file() && fs::metadata(&dest).map(|m| m.len() > 100_000).unwrap_or(false) {
            log(&format!("already have {}", item.rel));
            continue;
        }
        if let Some(dir) = dest.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        log(&format!("fetching {}", item.rel));
        let resp = agent.get(item.url).call().map_err(|e| e.to_string())?;
        let mut reader = resp.into_reader();
        let tmp = dest.with_extension("part");
        let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
        let mut buf = [0u8; 64 * 1024];
        let mut got = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            got += n as u64;
        }
        fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
        log(&format!("saved {} ({:.1} MB)", item.rel, got as f64 / 1e6));
    }
    let _ = Path::new(".");
    log("fetch done");
    Ok(())
}
