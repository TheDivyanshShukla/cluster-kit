use std::net::UdpSocket;
use std::thread;
use std::time::Duration;

use crate::watch::{log, DISC, MAGIC};

pub fn beacon(name: String, port: u16) {
    let payload = format!("{{\"magic\":\"{MAGIC}\",\"name\":\"{name}\",\"port\":{port}}}");
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            log(&format!("discovery beacon failed: {e}"));
            return;
        }
    };
    let _ = sock.set_broadcast(true);
    loop {
        let _ = sock.send_to(payload.as_bytes(), ("255.255.255.255", DISC));
        thread::sleep(Duration::from_secs(2));
    }
}

pub fn discover(name: &str) -> String {
    let sock = UdpSocket::bind(("0.0.0.0", DISC)).unwrap_or_else(|e| {
        eprintln!("cw: UDP {DISC}: {e}");
        std::process::exit(1);
    });
    let _ = sock.set_read_timeout(Some(Duration::from_secs(1)));
    log(&format!("looking for head '{name}' on UDP {DISC}"));
    let mut buf = [0u8; 2048];
    let mut quiet = 0;
    loop {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                if text.contains(MAGIC) && text.contains(&format!("\"name\":\"{name}\"")) {
                    let ip = from.ip().to_string();
                    log(&format!("found head at {ip}"));
                    return ip;
                }
            }
            Err(_) => {
                quiet += 1;
                if quiet % 15 == 0 {
                    log("still waiting for the head");
                }
            }
        }
    }
}
