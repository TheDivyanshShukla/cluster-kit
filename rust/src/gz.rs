//! Minimal gzip encoder (RFC 1951 fixed-Huffman blocks + RFC 1952 wrapper) for big API JSON replies.
//! No dependency: /api/v1/nodes and /cluster for 1,000 PCs are ~1.2 MB of very repetitive JSON that shrinks ~30-50x,
//! which matters when every console and wall screen on the lab LAN loads them.
//! ponytail: fixed Huffman codes and a short hash chain, so ~70-80 % of zlib's ratio; dynamic Huffman if that ever matters.

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const WINDOW: usize = 32 * 1024;
const CHAIN: usize = 8;
/// a match this long is good enough: stop searching (zlib's nice_length idea)
const NICE: usize = 64;

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    /// `n` bits of `v`, least significant first (DEFLATE bit order).
    fn put(&mut self, v: u32, n: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// A Huffman code: written most significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }
    fn lit(&mut self, sym: u32) {
        match sym {
            0..=143 => self.code(0x30 + sym, 8),
            144..=255 => self.code(0x190 + sym - 144, 9),
            256..=279 => self.code(sym - 256, 7),
            _ => self.code(0xC0 + sym - 280, 8),
        }
    }
    fn matched(&mut self, len: usize, dist: usize) {
        let li = LEN_BASE.iter().rposition(|&b| b as usize <= len).unwrap();
        self.lit(257 + li as u32);
        self.put((len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
        let di = DIST_BASE.iter().rposition(|&b| b as usize <= dist).unwrap();
        self.code(di as u32, 5);
        self.put((dist - DIST_BASE[di] as usize) as u32, DIST_EXTRA[di] as u32);
    }
}

fn crc32(data: &[u8]) -> u32 {
    static T: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    !data.iter().fold(!0u32, |c, &b| t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

/// gzip of `data` (one fixed-Huffman DEFLATE block).
pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut b = Bits { out: vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff], acc: 0, n: 0 };
    b.put(1, 1); // BFINAL
    b.put(1, 2); // BTYPE = 01, fixed Huffman
    let hash = |i: usize| ((data[i] as usize) << 10 ^ (data[i + 1] as usize) << 5 ^ data[i + 2] as usize) & 0x7FFF;
    let mut head = vec![usize::MAX; 1 << 15];
    let mut prev = vec![usize::MAX; data.len()];
    let mut i = 0;
    let insert = |i: usize, head: &mut Vec<usize>, prev: &mut Vec<usize>| {
        if i + 2 < data.len() {
            let h = hash(i);
            prev[i] = head[h];
            head[h] = i;
        }
    };
    while i < data.len() {
        let (mut best, mut at) = (0, 0);
        if i + 2 < data.len() {
            let mut c = head[hash(i)];
            let max = (data.len() - i).min(258);
            for _ in 0..CHAIN {
                if c == usize::MAX || i - c > WINDOW {
                    break;
                }
                // cheap reject first: a longer match must also agree on the byte just past the best so far
                if best == 0 || (best < max && data[c + best] == data[i + best]) {
                    let l = data[c..].iter().zip(&data[i..i + max]).take_while(|(a, b)| a == b).count();
                    if l > best {
                        (best, at) = (l, c);
                        if l >= max.min(NICE) {
                            break;
                        }
                    }
                }
                c = prev[c];
            }
        }
        if best >= 3 {
            b.matched(best, i - at);
            for k in i..i + best {
                insert(k, &mut head, &mut prev);
            }
            i += best;
        } else {
            b.lit(data[i] as u32);
            insert(i, &mut head, &mut prev);
            i += 1;
        }
    }
    b.lit(256); // end of block
    b.put(0, 7); // flush the last byte
    let mut out = b.out;
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    /// Round trip through Python's zlib (any dev box running the stress scripts has python3); skipped without it.
    #[test]
    fn python_gunzips_it_byte_exact() {
        let mut cases: Vec<Vec<u8>> = vec![vec![], b"a".to_vec(), b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec()];
        let json: String = (0..3000).map(|i| format!("{{\"node_id\":\"pc-{i}\",\"state\":\"active\",\"threads\":{},\"lab\":\"A{}\"}},", i % 16, i % 7)).collect();
        cases.push(json.into_bytes());
        cases.push((0..70_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect()); // incompressible, > window
        for c in &cases {
            let z = gzip(c);
            let py = std::process::Command::new("python3")
                .args(["-c", "import sys,gzip;sys.stdout.buffer.write(gzip.decompress(sys.stdin.buffer.read()))"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn();
            let Ok(mut p) = py else { return };
            use std::io::Write;
            p.stdin.take().unwrap().write_all(&z).unwrap();
            let out = p.wait_with_output().unwrap();
            assert!(out.status.success(), "python could not gunzip a {} byte input", c.len());
            assert_eq!(&out.stdout, c, "round trip of {} bytes", c.len());
        }
        let j = &cases[3];
        assert!(gzip(j).len() * 5 < j.len(), "repetitive JSON shrinks well: {} -> {}", j.len(), gzip(j).len());
    }
}
