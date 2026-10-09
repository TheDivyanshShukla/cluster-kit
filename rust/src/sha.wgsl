// SHA-256 of the decimal string of every number in a batch; records the batch offsets of hashes
// that match a nibble prefix. One invocation per number. Numbers are passed as hi * 1e9 + lo so
// everything stays in u32 (WGSL has no u64).
//
// p: [lo, hi, count, cap, mask0..mask7, want0..want7]
@group(0) @binding(0) var<storage, read> p: array<u32, 20>;
struct Out { count: atomic<u32>, idx: array<u32> }
@group(0) @binding(1) var<storage, read_write> out: Out;

var<private> K: array<u32, 64> = array<u32, 64>(
    0x428a2f98u, 0x71374491u, 0xb5c0fbcfu, 0xe9b5dba5u, 0x3956c25bu, 0x59f111f1u, 0x923f82a4u, 0xab1c5ed5u,
    0xd807aa98u, 0x12835b01u, 0x243185beu, 0x550c7dc3u, 0x72be5d74u, 0x80deb1feu, 0x9bdc06a7u, 0xc19bf174u,
    0xe49b69c1u, 0xefbe4786u, 0x0fc19dc6u, 0x240ca1ccu, 0x2de92c6fu, 0x4a7484aau, 0x5cb0a9dcu, 0x76f988dau,
    0x983e5152u, 0xa831c66du, 0xb00327c8u, 0xbf597fc7u, 0xc6e00bf3u, 0xd5a79147u, 0x06ca6351u, 0x14292967u,
    0x27b70a85u, 0x2e1b2138u, 0x4d2c6dfcu, 0x53380d13u, 0x650a7354u, 0x766a0abbu, 0x81c2c92eu, 0x92722c85u,
    0xa2bfe8a1u, 0xa81a664bu, 0xc24b8b70u, 0xc76c51a3u, 0xd192e819u, 0xd6990624u, 0xf40e3585u, 0x106aa070u,
    0x19a4c116u, 0x1e376c08u, 0x2748774cu, 0x34b0bcb5u, 0x391c0cb3u, 0x4ed8aa4au, 0x5b9cca4fu, 0x682e6ff3u,
    0x748f82eeu, 0x78a5636fu, 0x84c87814u, 0x8cc70208u, 0x90befffau, 0xa4506cebu, 0xbef9a3f7u, 0xc67178f2u);

var<private> w: array<u32, 64>;
var<private> len: u32;

fn put(b: u32) {
    w[len >> 2u] |= b << (24u - 8u * (len & 3u));
    len += 1u;
}

// Decimal digits of x, at least `width` of them (zero padded).
fn digits(x: u32, width: u32) {
    var d: array<u32, 10>;
    var k = 0u;
    var v = x;
    loop {
        d[k] = v % 10u;
        v /= 10u;
        k += 1u;
        if (v == 0u && k >= width) { break; }
    }
    for (var j = k; j > 0u; j--) { put(48u + d[j - 1u]); }
}

fn rotr(x: u32, n: u32) -> u32 { return (x >> n) | (x << (32u - n)); }

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p[2]) { return; }
    let l0 = p[0] + i;                  // p[0] < 1e9 and i < 2^24, so no u32 overflow
    let lo = l0 % 1000000000u;
    let hi = p[1] + l0 / 1000000000u;

    // One 64-byte block: message, 0x80, zeros, bit length (numbers are at most 19 digits).
    for (var t = 0u; t < 16u; t++) { w[t] = 0u; }
    len = 0u;
    if (hi > 0u) { digits(hi, 1u); digits(lo, 9u); } else { digits(lo, 1u); }
    let bits = len * 8u;
    put(0x80u);
    w[15] = bits;

    for (var t = 16u; t < 64u; t++) {
        let s0 = rotr(w[t - 15u], 7u) ^ rotr(w[t - 15u], 18u) ^ (w[t - 15u] >> 3u);
        let s1 = rotr(w[t - 2u], 17u) ^ rotr(w[t - 2u], 19u) ^ (w[t - 2u] >> 10u);
        w[t] = w[t - 16u] + s0 + w[t - 7u] + s1;
    }
    var h = array<u32, 8>(0x6a09e667u, 0xbb67ae85u, 0x3c6ef372u, 0xa54ff53au,
                          0x510e527fu, 0x9b05688cu, 0x1f83d9abu, 0x5be0cd19u);
    var a = h[0]; var b = h[1]; var c = h[2]; var d = h[3];
    var e = h[4]; var f = h[5]; var g = h[6]; var hh = h[7];
    for (var t = 0u; t < 64u; t++) {
        let t1 = hh + (rotr(e, 6u) ^ rotr(e, 11u) ^ rotr(e, 25u)) + ((e & f) ^ (~e & g)) + K[t] + w[t];
        let t2 = (rotr(a, 2u) ^ rotr(a, 13u) ^ rotr(a, 22u)) + ((a & b) ^ (a & c) ^ (b & c));
        hh = g; g = f; f = e; e = d + t1; d = c; c = b; b = a; a = t1 + t2;
    }
    h[0] += a; h[1] += b; h[2] += c; h[3] += d; h[4] += e; h[5] += f; h[6] += g; h[7] += hh;

    for (var k = 0u; k < 8u; k++) {
        if ((h[k] & p[4u + k]) != p[12u + k]) { return; }
    }
    let slot = atomicAdd(&out.count, 1u);
    if (slot < p[3]) { out.idx[slot] = i; }
}
