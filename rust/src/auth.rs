//! Node credentials and message authentication (SEC-02). Owner: a4. Pure library; the lead wires it in.
//! Contract (keep these signatures):
//! - `hmac_sha256(key, msg) -> [u8; 32]` (RFC 2104)
//! - `ct_eq(a, b) -> bool` constant-time compare
//! - `nonce() -> [u8; 32]` from the OS random source
//! - `respond(key, nonce, node_id) -> [u8; 32]`, `verify(key, nonce, node_id, mac) -> bool`
//! - `FrameMac::new(key)`, `.seal(kind, payload) -> [u8; 40]` (seq u64 LE + mac, seq increments),
//!   `.open(kind, payload, tag) -> bool` (rejects replayed or out-of-order seq)
//! - `load_key(path) -> io::Result<Vec<u8>>`, `save_key(path, key) -> io::Result<()>` (file mode 0600 on Unix)
#![allow(dead_code)]

use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::path::Path;

/// RFC 2104 HMAC-SHA256 over the concatenation of `parts`.
fn hmac_parts(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    for p in parts {
        inner.update(p);
    }
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    hmac_parts(key, &[msg])
}

/// Constant-time equality (length is not secret: all compared values are fixed-size MACs).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut d = (a.len() ^ b.len()) as u32;
    for i in 0..a.len().max(b.len()) {
        d |= (*a.get(i).unwrap_or(&0) ^ *b.get(i).unwrap_or(&0)) as u32;
    }
    d == 0
}

#[cfg(windows)]
#[link(name = "bcrypt")]
extern "system" {
    fn BCryptGenRandom(alg: *mut core::ffi::c_void, buf: *mut u8, len: u32, flags: u32) -> i32;
}

/// Fill `buf` from the OS CSPRNG. Panics if none: a predictable nonce would void the handshake.
fn fill_random(buf: &mut [u8]) {
    #[cfg(unix)]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(buf))
            .expect("no system randomness (/dev/urandom)");
    }
    #[cfg(windows)]
    {
        // BCRYPT_USE_SYSTEM_PREFERRED_RNG = 2 (null algorithm handle). std has no CSPRNG API, no new crates.
        let st = unsafe { BCryptGenRandom(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32, 2) };
        assert!(st >= 0, "BCryptGenRandom failed: {st:#x}");
    }
}

pub fn nonce() -> [u8; 32] {
    let mut n = [0u8; 32];
    fill_random(&mut n);
    n
}

/// mac = HMAC(key, "ck-hello" || nonce || node_id)
pub fn respond(key: &[u8], nonce: &[u8], node_id: &str) -> [u8; 32] {
    hmac_parts(key, &[b"ck-hello", nonce, node_id.as_bytes()])
}

pub fn verify(key: &[u8], nonce: &[u8], node_id: &str, mac: &[u8]) -> bool {
    ct_eq(&respond(key, nonce, node_id), mac)
}

/// Per-connection frame MAC. Keep one per direction pair: separate send and receive sequences.
pub struct FrameMac {
    key: Vec<u8>,
    send_seq: u64,
    recv_seq: u64,
}

impl FrameMac {
    pub fn new(key: &[u8]) -> FrameMac {
        FrameMac { key: key.to_vec(), send_seq: 0, recv_seq: 0 }
    }
    fn tag(&self, seq: u64, kind: u8, payload: &[u8]) -> [u8; 32] {
        let len = (payload.len() as u32).to_le_bytes();
        hmac_parts(&self.key, &[&seq.to_le_bytes(), &[kind], &len, payload])
    }
    /// seq (u64 LE, from 1) + HMAC(key, seq || kind || len u32 LE || payload).
    pub fn seal(&mut self, kind: u8, payload: &[u8]) -> [u8; 40] {
        self.send_seq += 1;
        let mut out = [0u8; 40];
        out[..8].copy_from_slice(&self.send_seq.to_le_bytes());
        out[8..].copy_from_slice(&self.tag(self.send_seq, kind, payload));
        out
    }
    /// Accepts only a valid MAC with seq greater than the last accepted one; state moves only on success.
    pub fn open(&mut self, kind: u8, payload: &[u8], tag: &[u8]) -> bool {
        if tag.len() != 40 {
            return false;
        }
        let seq = u64::from_le_bytes(tag[..8].try_into().unwrap());
        if seq <= self.recv_seq || !ct_eq(&self.tag(seq, kind, payload), &tag[8..]) {
            return false;
        }
        self.recv_seq = seq;
        true
    }
}

pub fn load_key(path: &Path) -> io::Result<Vec<u8>> {
    let k = std::fs::read(path)?;
    if k.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty key file"));
    }
    Ok(k)
}

/// Atomic write: temp file (0600 on Unix) in the same directory, then rename.
pub fn save_key(path: &Path, key: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let r = o.open(&tmp).and_then(|mut f| {
        f.write_all(key)?;
        f.sync_all()
    });
    match r.and_then(|_| std::fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn rfc4231_vectors() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn challenge_response() {
        let (k, n) = ([7u8; 32], nonce());
        assert_ne!(n, nonce());
        let m = respond(&k, &n, "node-a");
        assert!(verify(&k, &n, "node-a", &m));
        assert!(!verify(&k, &n, "node-b", &m));
        assert!(!verify(&k, &nonce(), "node-a", &m));
        assert!(!verify(&[8u8; 32], &n, "node-a", &m));
        assert!(!verify(&k, &n, "node-a", &m[..31]));
    }

    #[test]
    fn frame_mac() {
        let (mut a, mut b) = (FrameMac::new(b"k"), FrameMac::new(b"k"));
        let t1 = a.seal(3, b"one");
        assert_eq!(&t1[..8], &1u64.to_le_bytes());
        let t2 = a.seal(3, b"two");
        let t3 = a.seal(4, b"three");
        assert!(b.open(3, b"one", &t1));
        assert!(!b.open(3, b"one", &t1), "replay");
        assert!(!b.open(3, b"twX", &t2), "tampered payload");
        assert!(!b.open(4, b"two", &t2), "tampered kind");
        assert!(b.open(3, b"two", &t2), "failed open must not advance state");
        let t4 = a.seal(3, b"four");
        assert!(b.open(3, b"four", &t4));
        assert!(!b.open(4, b"three", &t3), "out of order");
        assert!(!b.open(3, b"x", &t4[..39]));
        assert!(!FrameMac::new(b"other").open(3, b"one", &t1), "wrong key");
        // send and receive sequences are independent
        let mut c = FrameMac::new(b"k");
        let _ = c.seal(1, b"");
        assert!(c.open(3, b"one", &t1));
    }

    #[test]
    fn key_file() {
        let d = std::env::temp_dir().join(format!("ck-auth-{}-{}", std::process::id(), hex(&nonce()[..4])));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("node.key");
        save_key(&p, b"secret-key").unwrap();
        assert_eq!(load_key(&p).unwrap(), b"secret-key");
        save_key(&p, b"second").unwrap();
        assert_eq!(load_key(&p).unwrap(), b"second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(!d.join("node.key.tmp").exists());
        std::fs::write(&p, b"").unwrap();
        assert!(load_key(&p).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
