//! Built-in work: SHA-256 prefix search (CPU/GPU), any-language commands, pushed scripts, synthetic tasks.
//! Owner: a5 (wave 3). `run_task` is the contract the agent calls; keep its signature.

use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
#[cfg(feature = "gpu")]
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use std::{env, fs};

use crate::proto::{KIND_LIST, KIND_SHA, KIND_SYNTH};

pub static CANCELLED: AtomicU64 = AtomicU64::new(0); // last job the head cancelled
// CANCEL_RANGE (HA-06): the one chunk (job, start) the head cancelled last. ponytail: one slot per process; agents run one chunk at a time.
pub static CANCELLED_RANGE: (AtomicU64, AtomicU64) = (AtomicU64::new(u64::MAX), AtomicU64::new(0));

/// Is chunk (job, a) cancelled, as a whole job (CANCEL) or alone (CANCEL_RANGE)?
pub fn cancelled(job: u64, a: u64) -> bool {
    CANCELLED.load(Relaxed) == job || (CANCELLED_RANGE.0.load(Relaxed) == job && CANCELLED_RANGE.1.load(Relaxed) == a)
}

#[cfg(feature = "gpu")]
pub static GPU: OnceLock<Option<crate::gpu::Gpu>> = OnceLock::new();

/// Run one chunk [a, b) of a job and return the RESULT body.
/// Err((retry, message)): the chunk failed; retry = worth running again (JOB-04 exit code 75 or a timeout).
pub fn run_task(kind: u8, job: u64, a: u64, b: u64, arg: &[u8]) -> Result<Vec<u8>, (bool, String)> {
    match kind {
        KIND_SHA => Ok(gpu_sha(job, a, b, arg).unwrap_or_else(|| sha_range(job, a, b, arg))),
        KIND_SYNTH => Ok(synth_range(job, a, b, arg)),
        KIND_LIST => run_list_par(job, arg, a, b),
        _ => run_cmd_par(job, arg, a, b),
    }
}

/// Synthetic task for tests and simulation: (b - a) * ns_per_number of compute, spread over the rayon
/// threads (each sleeps its share, so wall time = compute / threads). Sleeps in <= 50 ms slices and stops
/// early when the job is cancelled. Body = count of numbers u64 LE.
pub fn synth_range(job: u64, a: u64, b: u64, arg: &[u8]) -> Vec<u8> {
    let ns = arg.get(..8).map_or(0, |x| u64::from_le_bytes(x.try_into().unwrap()));
    let threads = rayon::current_num_threads() as u64;
    let share = Duration::from_nanos(((b - a) as u128 * ns as u128 / threads as u128).min(u64::MAX as u128) as u64);
    (0..threads).into_par_iter().for_each(|_| {
        let end = Instant::now() + share;
        while !cancelled(job, a) {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_millis(50)));
        }
    });
    (b - a).to_le_bytes().to_vec()
}

pub fn dec(mut n: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut i = 20;
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            return &buf[i..];
        }
    }
}

fn encode_hits(ns: impl IntoIterator<Item = u64>) -> Vec<u8> {
    let mut out = vec![];
    for n in ns {
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&Sha256::digest(dec(n, &mut [0; 20])));
    }
    out
}

/// GPU path for a SHA chunk, or None to use the CPU (no --gpu, no GPU, or out of its limits).
fn gpu_sha(job: u64, a: u64, b: u64, nibbles: &[u8]) -> Option<Vec<u8>> {
    #[cfg(feature = "gpu")]
    if let Some(Some(g)) = GPU.get() {
        return g.matches(a, b, nibbles, &CANCELLED, job).map(encode_hits);
    }
    let _ = (job, a, b, nibbles);
    None
}

const SHA256_INIT: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

/// SHA-256 state words of a number's decimal string. At most 20 bytes always fit one 64-byte
/// block, so this pads by hand and runs the compression (SHA instructions) once, skipping the
/// general-purpose hasher's buffering.
fn sha_words(n: u64) -> [u32; 8] {
    let mut buf = [0u8; 20];
    let msg = dec(n, &mut buf);
    let mut block = [0u8; 64];
    block[..msg.len()].copy_from_slice(msg);
    block[msg.len()] = 0x80;
    block[56..].copy_from_slice(&(msg.len() as u64 * 8).to_be_bytes());
    let mut st = SHA256_INIT;
    sha2::compress256(&mut st, &[*sha2::digest::generic_array::GenericArray::from_slice(&block)]);
    st
}

/// Prefix nibbles as per-word (mask, want), so a candidate is checked without building bytes.
fn prefix_words(nibbles: &[u8]) -> ([u32; 8], [u32; 8]) {
    let (mut mask, mut want) = ([0u32; 8], [0u32; 8]);
    for (i, &c) in nibbles.iter().enumerate() {
        let shift = 28 - 4 * (i % 8) as u32;
        mask[i / 8] |= 0xf << shift;
        want[i / 8] |= (c as u32) << shift;
    }
    (mask, want)
}

pub fn sha_range(job: u64, a: u64, b: u64, nibbles: &[u8]) -> Vec<u8> {
    if nibbles.len() > 64 {
        return vec![];
    }
    let (mask, want) = prefix_words(nibbles);
    let hits: Vec<u64> = (0..(b - a) as usize)
        .into_par_iter()
        .with_min_len(1 << 10)
        .filter_map(|k| {
            if cancelled(job, a) {
                return None; // chunk cancelled: rush through the rest of the chunk
            }
            let n = a + k as u64;
            let h = sha_words(n);
            (0..8).all(|i| h[i] & mask[i] == want[i]).then_some(n)
        })
        .collect();
    encode_hits(hits)
}

// Split the chunk into one piece per thread and run the program on each piece in parallel,
// so single-threaded programs use every core. Multi-threaded programs: start the worker with --threads 1.
fn run_cmd_par(job: u64, cmd: &[u8], a: u64, b: u64) -> Result<Vec<u8>, (bool, String)> {
    let parts = (rayon::current_num_threads() as u64).min(b - a).max(1);
    let step = (b - a).div_ceil(parts);
    let res: Vec<_> = (0..parts)
        .into_par_iter()
        .map(|i| run_cmd(cmd, job, parts, a + i * step, (a + (i + 1) * step).min(b)))
        .collect();
    let fails: Vec<&(bool, String)> = res.iter().filter_map(|r| r.as_ref().err()).collect();
    if let Some(first) = fails.first() {
        return Err((fails.iter().all(|f| f.0), first.1.clone()));
    }
    Ok(res.into_iter().flatten().flatten().collect())
}

/// List job (JOB-01): arg = command, 0x00, items joined by '\n' for [a, b). One contiguous piece of items per thread,
/// the command runs once per piece with its items on stdin (one per line); stdout joined in order.
fn run_list_par(job: u64, arg: &[u8], a: u64, b: u64) -> Result<Vec<u8>, (bool, String)> {
    let split = arg.iter().position(|c| *c == 0).unwrap_or(arg.len());
    let (cmd, items) = (&arg[..split], arg.get(split + 1..).unwrap_or(&[]));
    let lines: Vec<&[u8]> = if items.is_empty() { vec![] } else { items.split(|c| *c == b'\n').collect() };
    let n = lines.len() as u64;
    let parts = (rayon::current_num_threads() as u64).min(n).max(1);
    let step = n.div_ceil(parts).max(1);
    let res: Vec<_> = (0..parts)
        .into_par_iter()
        .filter(|i| i * step < n)
        .map(|i| {
            let (lo, hi) = ((i * step) as usize, ((i + 1) * step).min(n) as usize);
            let mut stdin = lines[lo..hi].join(&b'\n');
            stdin.push(b'\n');
            run_cmd_in(cmd, job, parts, a + lo as u64, a + hi as u64, Some(&stdin))
        })
        .collect();
    let _ = b; // the item count, not b, drives the split; the head sends exactly b - a items
    let fails: Vec<&(bool, String)> = res.iter().filter_map(|r| r.as_ref().err()).collect();
    if let Some(first) = fails.first() {
        return Err((fails.iter().all(|f| f.0), first.1.clone()));
    }
    Ok(res.into_iter().flatten().flatten().collect())
}

/// Agent calibration (FLT-06): SHA-256 MH/s measured over about `secs` seconds on all rayon threads.
pub fn calibrate_mhs(secs: f64) -> f64 {
    let t = Instant::now();
    let (mut n, step) = (0u64, 1u64 << 22);
    while t.elapsed().as_secs_f64() < secs {
        sha_range(u64::MAX, n, n + step, &[0xf; 40]); // a 40-nibble prefix: no hits, nothing allocated
        n += step;
    }
    n as f64 / t.elapsed().as_secs_f64() / 1e6
}

// `--push FILE`: the head ships the script itself, so a worker needs no copy of it. The argument is
// "\x01PUSH\n<interpreter>\n<file name>\n<script bytes>". The worker saves the script once under its temp folder
// (named by a hash of the contents) and runs `<interpreter> <saved path> START END`.
pub const PUSH_MARK: &[u8] = b"\x01PUSH\n";
pub const PUSH_MAX: usize = 64 * 1024;

fn command_base(cmd: &[u8]) -> String {
    let Some(rest) = cmd.strip_prefix(PUSH_MARK) else { return String::from_utf8_lossy(cmd).into_owned() };
    let mut lines = rest.splitn(3, |b| *b == b'\n');
    let interp = String::from_utf8_lossy(lines.next().unwrap_or(b"")).into_owned();
    let name: String = String::from_utf8_lossy(lines.next().unwrap_or(b"script"))
        .chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).collect();
    let body = lines.next().unwrap_or(b"");
    let name = if name.is_empty() { "script".to_string() } else { name };
    let hash = body.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
    let dir = env::temp_dir().join(format!("ck-push-{hash:016x}"));
    let path = dir.join(&name);
    if !path.exists() {
        let _ = fs::create_dir_all(&dir);
        let tmp = dir.join(format!("{name}.{}", std::process::id()));
        if fs::write(&tmp, body).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
    format!("{interp} \"{}\"", path.display())
}

pub const OUT_MAX: usize = 64 << 20; // stdout per piece
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// One piece of a `--cmd` job (JOB-04): env CK_*, exit 0 = ok, 75 = retry, anything else = failed.
fn run_cmd(cmd: &[u8], job: u64, pieces: u64, a: u64, b: u64) -> Result<Vec<u8>, (bool, String)> {
    run_cmd_in(cmd, job, pieces, a, b, None)
}

/// `stdin` = Some(items): a list job piece. The command gets no START END arguments, only the env vars and the items.
fn run_cmd_in(cmd: &[u8], job: u64, pieces: u64, a: u64, b: u64, stdin: Option<&[u8]>) -> Result<Vec<u8>, (bool, String)> {
    let scratch = env::temp_dir().join(format!("ck-scratch-{}-{}", std::process::id(), SCRATCH_SEQ.fetch_add(1, Relaxed)));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch).map_err(|e| (false, format!("scratch dir: {e}")))?;
    let line = if stdin.is_some() { command_base(cmd) } else { format!("{} {a} {b}", command_base(cmd)) };
    let mut c = if cfg!(windows) { let mut c = Command::new("cmd"); c.args(["/C", &line]); c } else { let mut c = Command::new("sh"); c.args(["-c", &line]); c };
    // ponytail: the head owns the attempt count; CK_ATTEMPT is only what the agent's environment says (default 1)
    let attempt = env::var("CK_ATTEMPT").unwrap_or_else(|_| "1".into());
    c.env("CK_JOB", job.to_string()).env("CK_TASK", format!("{job}:{a}-{b}")).env("CK_ATTEMPT", attempt)
        .env("CK_THREADS", pieces.to_string()).env("CK_START", a.to_string()).env("CK_END", b.to_string()).env("CK_SCRATCH", &scratch);
    let out = match stdin {
        None => c.output(),
        Some(items) => c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().and_then(|mut ch| {
            let mut si = ch.stdin.take().unwrap();
            let items = items.to_vec();
            // a thread, so a program that fills its output pipe before reading all input cannot deadlock us
            std::thread::spawn(move || { let _ = si.write_all(&items); }); // a closed pipe (program ignored stdin) is fine
            ch.wait_with_output()
        }),
    };
    let _ = fs::remove_dir_all(&scratch);
    let o = out.map_err(|e| (false, format!("could not start the program: {e}")))?;
    match o.status.code() {
        Some(0) if o.stdout.len() > OUT_MAX => Err((false, format!("output over the {} MiB limit", OUT_MAX >> 20))),
        Some(0) => Ok(o.stdout),
        code => {
            let err = String::from_utf8_lossy(&o.stderr);
            let last: String = err.trim().lines().last().unwrap_or("").chars().take(300).collect();
            let what = code.map_or("killed by a signal".to_string(), |c| format!("exit {c}"));
            Err((code == Some(75), format!("{what}: {last}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_and_prefix() {
        assert_eq!(dec(0, &mut [0; 20]), b"0");
        assert_eq!(dec(u64::MAX, &mut [0; 20]), b"18446744073709551615");
        let (mask, want) = prefix_words(&[6, 0xb, 8, 6]); // sha256("1") = 6b86b273...
        assert_eq!(sha_words(1)[0] & mask[0], want[0]);
        assert_ne!(sha_words(1)[0] & mask[0], prefix_words(&[6, 0xb, 8, 7]).1[0]);
        for n in [0, 1, 9, 10, 999_999_999, 1_000_000_000, 12_345_678_901_234, u64::MAX] {
            let words: Vec<u8> = sha_words(n).iter().flat_map(|w| w.to_be_bytes()).collect();
            assert_eq!(words[..], Sha256::digest(dec(n, &mut [0; 20]))[..], "n = {n}");
        }
        let (mask, want) = prefix_words(&[6, 0xb, 8, 6]);
        assert_eq!((mask[0], want[0], mask[1]), (0xffff_0000, 0x6b86_0000, 0));
    }

    #[test]
    fn synth_duration_and_cancel() {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
        let ns = 1_000_000u64.to_le_bytes(); // 1 ms per number
        let t = Instant::now();
        let body = pool.install(|| synth_range(u64::MAX - 1, 0, 400, &ns)); // 400 ms compute / 4 threads
        assert_eq!(body, 400u64.to_le_bytes());
        let d = t.elapsed();
        assert!(d >= Duration::from_millis(90) && d < Duration::from_millis(400), "{d:?}");
        CANCELLED.store(77, Relaxed);
        let t = Instant::now();
        pool.install(|| synth_range(77, 0, 40_000, &ns)); // would take 10 s
        assert!(t.elapsed() < Duration::from_millis(200));
        CANCELLED.store(0, Relaxed);
    }

    #[test]
    fn range_cancel_is_per_chunk() {
        CANCELLED_RANGE.1.store(10, Relaxed);
        CANCELLED_RANGE.0.store(555, Relaxed);
        assert!(cancelled(555, 10) && !cancelled(555, 20) && !cancelled(556, 10));
        CANCELLED_RANGE.0.store(u64::MAX, Relaxed);
        assert!(!cancelled(555, 10));
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_matches_cpu() {
        let Some(g) = crate::gpu::Gpu::new() else { return eprintln!("no GPU, skipped") };
        let never = AtomicU64::new(0);
        // includes 0, the 1e9 boundary (hi/lo split) and 14-digit numbers (zero-padded lo)
        for (a, b, prefix) in [(0, 20_000_000, "00000"), (999_000_000, 1_001_000_000, "000"), (12_345_678_900_000, 12_345_680_000_000, "000")] {
            let nib: Vec<u8> = prefix.bytes().map(|c| (c as char).to_digit(16).unwrap() as u8).collect();
            let cpu: Vec<u64> = sha_range(u64::MAX, a, b, &nib).chunks_exact(40).map(|r| u64::from_le_bytes(r[..8].try_into().unwrap())).collect();
            let gpu = g.matches(a, b, &nib, &never, u64::MAX).unwrap();
            assert!(!cpu.is_empty());
            assert_eq!(cpu, gpu, "range {a}..{b} prefix {prefix}");
        }
    }

    #[cfg(unix)]
    fn cmd(script: &str, a: u64, b: u64) -> Result<Vec<u8>, (bool, String)> {
        run_cmd(format!("sh -c '{script}' x").as_bytes(), 7, 2, a, b)
    }

    #[cfg(unix)]
    #[test]
    fn cmd_env_and_ok() {
        assert_eq!(cmd("echo $CK_JOB $CK_TASK $CK_START $CK_END $CK_THREADS $CK_ATTEMPT", 5, 9).unwrap(), b"7 7:5-9 5 9 2 1\n");
        assert_eq!(cmd("echo \"$1-$2\"", 5, 9).unwrap(), b"5-9\n");
    }

    #[cfg(unix)]
    #[test]
    fn cmd_exit_codes() {
        let e = cmd("echo oops >&2; exit 75", 0, 1).unwrap_err();
        assert!(e.0 && e.1.contains("75") && e.1.contains("oops"), "{e:?}");
        let e = cmd("echo a >&2; echo last >&2; exit 3", 0, 1).unwrap_err();
        assert!(!e.0 && e.1 == "exit 3: last", "{e:?}");
        assert!(run_cmd(b"/no/such/program-x", 1, 1, 0, 1).is_err());
        // mixed pieces: retry only if every failing piece said 75
        assert_eq!(run_cmd_par(1, b"sh -c 'exit 75' x", 0, 4).unwrap_err().0, true);
    }

    #[cfg(unix)]
    #[test]
    fn list_split_and_stdin() {
        let items = (0..10).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let arg = |c: &str| [c.as_bytes(), b"\0", items.as_bytes()].concat();
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
        // 10 items over 3 threads: pieces of 4, 4, 2 lines
        let out = pool.install(|| run_list_par(1, &arg("sh -c 'wc -l'"), 100, 110)).unwrap();
        let counts: Vec<u32> = String::from_utf8(out).unwrap().split_whitespace().map(|x| x.parse().unwrap()).collect();
        assert_eq!(counts, [4, 4, 2]);
        // order, env range and stdin content
        let out = pool.install(|| run_list_par(1, &arg("sh -c 'echo $CK_START-$CK_END; cat'"), 100, 110)).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "100-104\n0\n1\n2\n3\n104-108\n4\n5\n6\n7\n108-110\n8\n9\n");
        // exit codes
        let e = pool.install(|| run_list_par(1, &arg("sh -c 'exit 75'"), 0, 10)).unwrap_err();
        assert!(e.0, "{e:?}");
        let e = pool.install(|| run_list_par(1, &arg("sh -c 'echo bad >&2; exit 2'"), 0, 10)).unwrap_err();
        assert!(!e.0 && e.1 == "exit 2: bad", "{e:?}");
        // a program that ignores stdin is fine
        assert!(run_list_par(1, &arg("true"), 0, 10).is_ok());
    }

    #[test]
    fn calibration_runs() {
        assert!(calibrate_mhs(0.2) > 0.1);
    }

    #[cfg(unix)]
    #[test]
    fn cmd_scratch_removed() {
        let p = String::from_utf8(cmd("touch $CK_SCRATCH/f; echo $CK_SCRATCH", 0, 1).unwrap()).unwrap();
        assert!(p.contains("ck-scratch-") && !std::path::Path::new(p.trim()).exists());
    }
}
