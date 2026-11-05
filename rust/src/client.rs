//! Client commands: `ck run` (submit a job, print results) and `ck ping`. Owner: a3.

use std::fs;
use std::collections::HashSet;
use std::io::{self, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use crate::kernels::{PUSH_MARK, PUSH_MAX};
use crate::proto::*;
use crate::{die, Opts};

fn connect_client(o: &Opts) -> (BufReader<Sock>, Sock) {
    let ip = o.head.as_deref().unwrap_or("127.0.0.1");
    let s = TcpStream::connect((ip, port())).unwrap_or_else(|e| die(&format!("cannot reach head {ip}:{}: {e}", port())));
    let (r, mut w) = conn(s).unwrap();
    send(&mut w, HELLO_C, &[]).unwrap();
    (r, w)
}

pub fn run(o: &Opts) {
    if o.rest.first().map(String::as_str) == Some("attach") {
        return attach(o); // ponytail: until main.rs has --attach: `ck run attach N [--sha P | --synth NS]`
    }
    if let Ok(f) = std::env::var("CK_LIST_FILE") {
        return run_list(o, &f); // ponytail: until main.rs has --list FILE
    }
    let (kind, arg) = match (&o.sha, &o.cmd, &o.push) {
        (None, None, None) if o.synth.is_some() => (KIND_SYNTH, o.synth.unwrap().to_le_bytes().to_vec()),
        _ if o.synth.is_some() => die("--synth cannot be combined with --sha, --cmd or --push"),
        (None, None, Some(file)) => {
            let body = fs::read(file).unwrap_or_else(|e| die(&format!("cannot read {file}: {e}")));
            if body.len() > PUSH_MAX {
                die(&format!("{file} is {} bytes; --push sends the script with every chunk, keep it under {PUSH_MAX} bytes", body.len()));
            }
            let name = std::path::Path::new(file).file_name().map_or("script".into(), |n| n.to_string_lossy().into_owned());
            let interp = o.with.clone().unwrap_or_else(|| match std::path::Path::new(file).extension().and_then(|e| e.to_str()) {
                Some("py") => if cfg!(windows) { "python".into() } else { "python3".into() },
                Some("js") => "node".into(),
                Some("sh") => "sh".into(),
                _ => die("cannot tell how to run this file; add --with \"interpreter\" (for example --with python3)"),
            });
            let mut arg = PUSH_MARK.to_vec();
            arg.extend_from_slice(interp.as_bytes());
            arg.push(b'\n');
            arg.extend_from_slice(name.as_bytes());
            arg.push(b'\n');
            arg.extend_from_slice(&body);
            (KIND_CMD, arg)
        }
        (Some(p), None, None) => {
            let nib: Option<Vec<u8>> = p.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect();
            match nib.filter(|n| n.len() <= 64) {
                Some(n) => (KIND_SHA, n),
                None => die("--sha must be up to 64 hex digits"),
            }
        }
        (None, Some(c), None) => (KIND_CMD, c.as_bytes().to_vec()),
        _ => die("give exactly one of --sha PREFIX, --cmd \"program\", --push FILE or --synth NS"),
    };
    let end = o.from.checked_add(o.n).unwrap_or_else(|| die("--from + --n overflows"));
    let (mut r, mut s) = connect_client(o);
    let only = o.only.as_deref().unwrap_or("");
    if only.len() > 255 {
        die("--only list is longer than 255 characters");
    }
    let mut p = pack(&[o.from, end, o.chunk]);
    p.push(kind);
    p.push(only.len() as u8);
    p.extend_from_slice(only.as_bytes());
    p.extend_from_slice(&arg);
    let t = Instant::now();
    let (policy, reducer) = (std::env::var("CK_POLICY").unwrap_or_default(), std::env::var("CK_REDUCER").unwrap_or_default()); // until main.rs has --policy / --reducer
    if o.selector.is_some() || o.priority.is_some() || !policy.is_empty() || !reducer.is_empty() {
        // SUBMIT_JSON carries selector and priority (the old SUBMIT cannot)
        let priority = match o.priority.as_deref() {
            None => crate::jobs::normal_priority(),
            Some(n) => crate::jobs::PRIORITIES.iter().position(|p| *p == n).map(|i| i as u8).or_else(|| n.parse().ok().filter(|i| (*i as usize) < crate::jobs::PRIORITIES.len()))
                .unwrap_or_else(|| die(&format!("--priority must be one of: {}", crate::jobs::PRIORITIES.join(", ")))),
        };
        let spec = crate::jobs::JobSpec {
            kind, from: o.from, end, chunk: o.chunk, arg: arg.clone(), only: only.split(',').filter(|a| !a.is_empty()).map(String::from).collect(),
            selector: o.selector.clone().unwrap_or_default(), priority, policy, reducer, ..Default::default()
        };
        send(&mut s, SUBMIT_JSON, &serde_json::to_vec(&spec).unwrap()).unwrap();
    } else {
        send(&mut s, SUBMIT, &p).unwrap();
    }
    let id = match recv(&mut r).unwrap_or_else(|e| die(&format!("lost head: {e}"))) {
        (ACCEPTED, b) => u64_at(&b, 0).unwrap_or(0),
        (ERR, b) => die(&String::from_utf8_lossy(&b)),
        _ => die("unexpected reply from head"),
    };
    if o.detach {
        println!("job {id}");
        return;
    }
    eprintln!("job {id}  (Ctrl+C detaches; the job keeps running, see it with: ck job show {id})");
    stream(r, s, id, kind, o, Some((o.from, end)));
    eprintln!("{} numbers in {:.3}s = {:.1} M/s", o.n, t.elapsed().as_secs_f64(), o.n as f64 / t.elapsed().as_secs_f64() / 1e6);
}

/// `ck run --list FILE --cmd PROG`: one item per line (empty lines skipped), each task runs PROG with its items on stdin (JOB-01).
fn run_list(o: &Opts, file: &str) {
    let Some(cmd) = &o.cmd else { die("--list needs --cmd \"program\"") };
    let text = fs::read_to_string(file).unwrap_or_else(|e| die(&format!("cannot read {file}: {e}")));
    let items: Vec<String> = text.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
    if items.is_empty() {
        die(&format!("{file} has no items"));
    }
    let priority = o.priority.as_deref().and_then(|n| crate::jobs::PRIORITIES.iter().position(|p| *p == n).map(|i| i as u8).or_else(|| n.parse().ok())).unwrap_or_else(crate::jobs::normal_priority);
    let spec = crate::jobs::JobSpec {
        kind: KIND_LIST, from: 0, end: items.len() as u64, chunk: o.chunk, arg: cmd.as_bytes().to_vec(), selector: o.selector.clone().unwrap_or_default(), priority,
        policy: std::env::var("CK_POLICY").unwrap_or_default(), reducer: std::env::var("CK_REDUCER").unwrap_or_default(), items, ..Default::default()
    };
    let n = spec.end;
    let (mut r, mut s) = connect_client(o);
    send(&mut s, SUBMIT_JSON, &serde_json::to_vec(&spec).unwrap()).unwrap();
    let id = match recv(&mut r).unwrap_or_else(|e| die(&format!("lost head: {e}"))) {
        (ACCEPTED, b) => u64_at(&b, 0).unwrap_or(0),
        (ERR, b) => die(&String::from_utf8_lossy(&b)),
        _ => die("unexpected reply from head"),
    };
    if o.detach {
        println!("job {id}");
        return;
    }
    eprintln!("job {id}: {n} items");
    stream(r, s, id, KIND_LIST, o, Some((0, n))); // parts are printed in completion order; use `ck job result --reducer` for a combined value
}

/// `ck run attach N`: replay a job's stored results, then follow it live. Kind comes from --sha / --synth (else command output).
fn attach(o: &Opts) {
    let id: u64 = o.rest.get(1).and_then(|n| n.parse().ok()).unwrap_or_else(|| die("usage: ck run attach JOB_ID [--sha PREFIX | --synth NS]"));
    let kind = if o.sha.is_some() { KIND_SHA } else if o.synth.is_some() { KIND_SYNTH } else { KIND_CMD };
    let (r, mut s) = connect_client(o);
    send(&mut s, ATTACH, &id.to_le_bytes()).unwrap();
    stream(r, s, id, kind, o, None);
}

/// Parts seen so far; a part already printed (same start, end) is skipped, so a re-attach after a failover prints nothing twice.
#[derive(Default)]
struct Acc {
    hits: Vec<(u64, [u8; 32])>,
    counted: u64,
    seen: HashSet<(u64, u64)>,
}

impl Acc {
    /// Take one PART frame body; `out` gets command output.
    fn part(&mut self, kind: u8, body: &[u8], out: &mut impl Write) -> io::Result<()> {
        let (a, b) = (u64_at(body, 0)?, u64_at(body, 8)?);
        if !self.seen.insert((a, b)) {
            return Ok(());
        }
        match kind {
            KIND_SHA => {
                for r in body[16..].chunks_exact(40) {
                    self.hits.push((u64::from_le_bytes(r[..8].try_into().unwrap()), r[8..].try_into().unwrap()));
                }
            }
            KIND_SYNTH => self.counted += u64::from_le_bytes(body[16..24].try_into().unwrap_or([0; 8])),
            _ => out.write_all(&body[16..])?, // chunks arrive in completion order
        }
        Ok(())
    }
}

/// Heads to try after losing one, best first: beacons by highest epoch (when the discovery port is free), CK_HEADS,
/// --head, the PC's last_head. Each is "ip:port".
fn head_candidates(o: &Opts) -> Vec<String> {
    let mut v: Vec<(u64, String)> = vec![];
    if let Ok(s) = UdpSocket::bind(("0.0.0.0", disc_port())) {
        let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
        let (end, mut b) = (Instant::now() + Duration::from_millis(1500), [0u8; 512]);
        while Instant::now() < end {
            let Ok((n, from)) = s.recv_from(&mut b) else { continue };
            if let Some(bc) = std::str::from_utf8(&b[..n]).ok().and_then(|t| crate::agent::parse_beacon(t, from.ip())) {
                v.retain(|e| e.1 != bc.addr);
                v.push((bc.epoch, bc.addr));
            }
        }
    }
    v.sort_by(|x, y| y.0.cmp(&x.0));
    let mut out: Vec<String> = v.into_iter().map(|e| e.1).collect();
    let hp = |a: &str| if a.contains(':') { a.to_string() } else { format!("{a}:{}", port()) };
    let listed = std::env::var("CK_HEADS").unwrap_or_default();
    let last = std::fs::read_to_string(crate::agent::last_head_path()).ok().and_then(|t| crate::agent::parse_last_head(&t)).map(|h| h.0);
    for a in listed.split(',').map(str::trim).filter(|a| !a.is_empty()).map(hp).chain(Some(hp(o.head.as_deref().unwrap_or("127.0.0.1")))).chain(last) {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// The head is gone: find a head that takes ATTACH for `id`, for up to CK_FAILOVER_SECS (default 60).
fn reattach(o: &Opts, id: u64) -> (BufReader<Sock>, Sock) {
    let secs = crate::presence::get_or_env("client_failover_s", "CK_FAILOVER_SECS");
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < deadline {
        for addr in head_candidates(o) {
            let Some(a) = addr.to_socket_addrs().ok().and_then(|mut a| a.next()) else { continue };
            let Ok(s) = TcpStream::connect_timeout(&a, Duration::from_secs(2)) else { continue };
            let Ok((r, mut w)) = conn(s) else { continue };
            if send(&mut w, HELLO_C, &[]).is_ok() && send(&mut w, ATTACH, &id.to_le_bytes()).is_ok() {
                eprintln!("re-attached job {id} on {addr}");
                return (r, w);
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    die(&format!("lost head and found no other head within {secs}s (set CK_HEADS=ip,ip)"))
}

/// Print PART frames until DONE; if the head drops, re-attach `id` elsewhere and carry on. `range` is known right after a submit.
fn stream(mut r: BufReader<Sock>, mut w: Sock, id: u64, kind: u8, o: &Opts, range: Option<(u64, u64)>) {
    let mut acc = Acc::default();
    let mut stdout = io::stdout().lock();
    let mut moved = false;
    loop {
        let lost = match recv(&mut r) {
            Ok((PART, body)) => match acc.part(kind, &body, &mut stdout) {
                Ok(()) => continue,
                Err(e) => e.to_string(),
            },
            Ok((DONE, _)) => break,
            Ok((ERR, body)) if !moved => die(&String::from_utf8_lossy(&body)),
            Ok((ERR, body)) => String::from_utf8_lossy(&body).into_owned(), // the new head may still be catching up
            Ok(_) => continue,
            Err(e) => e.to_string(),
        };
        eprintln!("lost head ({lost}); looking for another");
        (r, w) = reattach(o, id);
        moved = true;
    }
    drop(w);
    let (hits, counted) = (&mut acc.hits, acc.counted);
    if kind == KIND_SHA {
        hits.sort_unstable();
        let pre = o.sha.as_deref().unwrap_or("");
        match range {
            Some((from, end)) => println!("{} hashes start with '{pre}' in {from}..{}", hits.len(), end - 1),
            None => println!("{} hashes start with '{pre}'", hits.len()),
        }
        for (n, d) in hits.iter().take(10) {
            println!("  {n:>14}  {}", d.iter().map(|b| format!("{b:02x}")).collect::<String>());
        }
    }
    if kind == KIND_SYNTH {
        println!("{counted} numbers counted");
        if range.is_some() && counted != o.n {
            die(&format!("MISMATCH: counted {counted} but --n is {}", o.n));
        }
    }
}

pub fn ping(o: &Opts) {
    let (mut r, mut s) = connect_client(o);
    let buf = vec![0u8; o.size];
    let mut v: Vec<f64> = (0..o.count)
        .map(|_| {
            let t = Instant::now();
            send(&mut s, PING, &buf).unwrap();
            recv(&mut r).unwrap();
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    v.sort_by(f64::total_cmp);
    let q = |f: f64| v[((v.len() - 1) as f64 * f) as usize];
    let mbs = 2.0 * o.size as f64 / q(0.5); // bytes per us == MB/s, both directions
    println!("{} B round trip over {} pings: min {:.1} us  median {:.1} us  p99 {:.1} us  ({mbs:.0} MB/s)", o.size, v.len(), q(0.0), q(0.5), q(0.99));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replayed_parts_are_skipped() {
        let part = |a: u64, b: u64, d: &[u8]| {
            let mut p = pack(&[a, b]);
            p.extend_from_slice(d);
            p
        };
        let (mut acc, mut out) = (Acc::default(), Vec::new());
        acc.part(KIND_CMD, &part(0, 10, b"x"), &mut out).unwrap();
        acc.part(KIND_CMD, &part(10, 20, b"y"), &mut out).unwrap();
        acc.part(KIND_CMD, &part(0, 10, b"x"), &mut out).unwrap(); // the new head replays everything
        acc.part(KIND_CMD, &part(20, 30, b"z"), &mut out).unwrap();
        assert_eq!(out, b"xyz");
        let mut s = Acc::default();
        for _ in 0..2 {
            s.part(KIND_SYNTH, &part(0, 5, &5u64.to_le_bytes()), &mut out).unwrap();
        }
        assert_eq!(s.counted, 5);
    }
}
