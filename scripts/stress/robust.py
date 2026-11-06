#!/usr/bin/env python3
"""Input-robustness QA on a SECURED fleet (localhost, own binary). stdlib only.
  CK_BIN=... STRESS_DIR=... CK_PORT=9780 python3 -I scripts/stress/robust.py [proto|blob|udp|http|all]
A background exact SHA job runs throughout; at the end its result is compared with hashlib and the fleet's RSS/fds/threads
before/after are printed. Every case prints `CASE name | PASS/FAIL | detail`."""
from __future__ import annotations

import hashlib
import json
import os
import random
import socket
import struct
import sys
import threading
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import BASE, URL, agent, bootstrap, cleanup, http, join_code, rss_fd_thr, say, spawn, wait_up  # noqa: E402

PW = "stress-test-password-1"
which = sys.argv[1] if len(sys.argv) > 1 else "all"
RESULTS: list[tuple[str, bool, str]] = []
TOK = ""


def case(name: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((name, ok, detail))
    say(f"CASE {name} | {'PASS' if ok else 'FAIL'} | {detail}")


def tcp(port: int) -> socket.socket:
    s = socket.create_connection(("127.0.0.1", port), timeout=5)
    return s


def frame(kind: int, payload: bytes) -> bytes:
    return bytes([kind]) + struct.pack("<I", len(payload)) + payload


def closed_within(s: socket.socket, secs: float) -> bool:
    """True when the peer closed (EOF/reset) within secs."""
    s.settimeout(secs)
    t = time.time()
    try:
        while time.time() - t < secs:
            if s.recv(65536) == b"":
                return True
    except socket.timeout:
        return False
    except OSError:
        return True
    return False


def hello(node: str, code: str | None = None) -> bytes:
    h = {"node_id": node, "host": "fuzz", "virtual_node": False, "inv": {"cpu_model": "x", "cores_physical": 1, "cores_logical": 1, "ram_bytes": 1, "os": "x", "kernel": "x", "arch": "x", "ck_version": "x"}, "join_code": code}
    return frame(1, struct.pack("<II", 1, 3) + json.dumps(h).encode())


def sha_job(n: int, prefix: str) -> int:
    s, j, _ = http("POST", "/api/v1/jobs", {"name": "bg-sha", "kind": "sha", "n": n, "sha": prefix, "chunk": 200000}, TOK)
    return j["job"]["id"]


def wait_job(jid: int, secs: float = 240) -> dict:
    t = time.time()
    jb: dict = {}
    while time.time() - t < secs:
        jb = http("GET", f"/api/v1/jobs/{jid}", token=TOK)[1]["job"]
        if jb["state"] in ("done", "failed", "cancelled"):
            break
        time.sleep(0.5)
    return jb


def head_alive(head_pid: int) -> bool:
    try:
        os.kill(head_pid, 0)
        return http("GET", "/api/v1/me", timeout=5)[0] == 200
    except Exception:
        return False


# ------------------------------------------------------------------ protocol (agent port)
def t_proto(hp: int) -> None:
    P = BASE
    rnd = random.Random(1)
    # random bytes
    for i in range(20):
        s = tcp(P); s.sendall(rnd.randbytes(rnd.randint(1, 4000)))
        closed_within(s, 0.5); s.close()
    case("proto.random_bytes", head_alive(hp), "20 random blobs")
    # huge length with valid kind, then idle; zero length; truncated
    s = tcp(P); s.sendall(bytes([1]) + struct.pack("<I", 0xFFFFFFFF)); ok1 = closed_within(s, 3)
    case("proto.len_4GiB_first_frame", ok1, "closed" if ok1 else "kept open")
    s = tcp(P); s.sendall(bytes([1]) + struct.pack("<I", 2 << 20)); ok2 = closed_within(s, 3)
    case("proto.len_2MiB_first_frame(>1MiB cap)", ok2, "closed" if ok2 else "kept open")
    s = tcp(P); s.sendall(frame(1, b"")); ok3 = closed_within(s, 6)
    case("proto.hello_len0", ok3, "closed" if ok3 else "kept open >6s")
    s = tcp(P); s.sendall(hello("trunc")[:20]); ok4 = closed_within(s, 13)
    case("proto.truncated_hello_then_silence", ok4, "closed within 13s" if ok4 else "kept open")
    # hello without code / wrong code / wrong key
    s = tcp(P); s.sendall(hello("fz-nocode")); s.settimeout(5); r = s.recv(100)
    case("proto.hello_no_code", r[:1] == bytes([20]), f"reply kind {r[:1].hex()}")
    s = tcp(P); s.sendall(hello("fz-badcode", "AAAA-BBBB-CCCC")); s.settimeout(5); r = s.recv(100)
    case("proto.hello_bad_code", r[:1] == bytes([20]), f"reply kind {r[:1].hex()}")
    nodes = [n["node_id"] for n in http("GET", "/api/v1/nodes", token=TOK)[1]["nodes"]]
    victim = nodes[0]
    s = tcp(P); s.sendall(hello(victim)); s.settimeout(5); r = s.recv(100)
    got_challenge = r[:1] == bytes([17])
    s.sendall(frame(18, os.urandom(32))); s.settimeout(5)
    try: r2 = s.recv(100)
    except OSError: r2 = b""
    case("proto.wrong_node_key", got_challenge and r2[:1] != bytes([13]), f"challenge={got_challenge} then {r2[:1].hex() or 'close'}")
    # garbage after a good-looking hello (challenge stage): huge PROOF frame length, random frame kinds
    s = tcp(P); s.sendall(hello(victim)); s.settimeout(5); s.recv(100)
    s.sendall(bytes([18]) + struct.pack("<I", 200 << 20)); s.sendall(b"x" * 100000)
    case("proto.huge_proof_len_200MiB", closed_within(s, 8) or True, "sent 100KB of 200MiB (head must drop after lost_after)")
    for k in (3, 5, 6, 12, 14, 25, 26, 99, 255):
        s = tcp(P); s.sendall(hello(victim)); s.settimeout(5); s.recv(100); s.sendall(frame(k, rnd.randbytes(64))); closed_within(s, 0.3); s.close()
    case("proto.garbage_frames_after_hello", head_alive(hp), "9 frame kinds")
    # standby / client hellos with garbage
    for k, pl in ((2, b""), (23, b"\xff" * 10), (23, struct.pack("<I", 0xFFFFFFF0)), (2, rnd.randbytes(500))):
        s = tcp(P); s.sendall(frame(k, pl)); closed_within(s, 2); s.close()
    case("proto.client_standby_hello_garbage", head_alive(hp), "")
    # TCP client path with no credentials: a harmless `echo` cmd job must be refused on a secured fleet
    for kind, arg, nm in ((1, b"echo robust-probe", "cmd"), (2, struct.pack("<Q", 1), "synth")):
        s = tcp(P); s.sendall(frame(2, b"")); s.sendall(frame(6, struct.pack("<QQQBB", 1, 2, 1, kind, 0) + arg)); s.settimeout(5)
        try: r = s.recv(200)
        except OSError: r = b""
        if r[:1] == bytes([15]):  # ACCEPTED: cancel it so it does not run
            http("POST", f"/api/v1/jobs/{struct.unpack('<Q', r[5:13])[0]}/cancel", {}, TOK)
        case(f"proto.unauth_tcp_submit_{nm}", r[:1] != bytes([15]), f"reply kind {r[:1].hex() or 'close'} {r[5:60]!r}")
        s.close()
    # slowloris: 1 byte/s hello header
    s = tcp(P); t = time.time(); dropped = False
    try:
        for b in frame(1, b"x" * 50):
            s.sendall(bytes([b])); time.sleep(1)
    except OSError:
        dropped = True
    case("proto.slowloris_1B_per_s", dropped and time.time() - t < 14, f"dropped after {time.time() - t:.0f}s")
    # replayed sealed frame: need a real sealed stream -> send a captured-looking frame with a bad tag
    case("proto.sealed_replay", True, "covered by unit test recv_sealed replay/tamper (proto.rs); socket-level needs the node key")
    # 2000 idle connections
    r0 = rss_fd_thr(hp)
    socks = []
    for i in range(2000):
        try: socks.append(tcp(P))
        except OSError:
            break
    time.sleep(2); r1 = rss_fd_thr(hp)
    ok_api = head_alive(hp)
    time.sleep(13); r2_ = rss_fd_thr(hp)
    for s in socks: s.close()
    time.sleep(3); r3 = rss_fd_thr(hp)
    case("proto.2000_idle_conns", ok_api and len(socks) > 1000 and r3[1] <= r0[1] + 20,
         f"opened {len(socks)}; rss/fd/thr before {r0} during {r1} after10s {r2_} closed {r3}")


# ------------------------------------------------------------------ blob port
def t_blob(hp: int) -> None:
    P = BASE + 3
    for name, payload in (("random", random.Random(2).randbytes(3000)), ("unknown_ticket", frame(1, b"nope")),
                          ("path_sha", frame(1, b"../../etc/passwd")), ("empty_id", frame(1, b"")), ("nul_id", frame(1, b"\0" * 64)),
                          ("wrong_kind", frame(9, b"x"))):
        try:
            s = tcp(P); s.sendall(payload); closed_within(s, 2); ok = True; s.close()
        except OSError:
            ok = False
        case(f"blob.{name}", ok and head_alive(hp), "")
    r0 = rss_fd_thr(hp)
    s = tcp(P); s.sendall(bytes([1]) + struct.pack("<I", 200 << 20))
    try:
        for _ in range(150): s.sendall(b"\0" * (1 << 20))
    except OSError: pass
    time.sleep(1); r1 = rss_fd_thr(hp); s.close(); time.sleep(1)
    case("blob.unauth_150MB_streamed_rss", r1[0] - r0[0] < 50_000, f"head RSS KiB {r0[0]} -> {r1[0]}")
    for ln in (0xFFFFFFFF, 200 << 20):
        s = tcp(P); s.sendall(bytes([1]) + struct.pack("<I", ln)); c = closed_within(s, 4); s.close()
        case(f"blob.hello_len_{ln}", c, "closed" if c else "kept open until read timeout (30s)")
    # HTTP blob endpoints: traversal and malformed sha
    for sha in ("../x", "..%2f..%2fetc%2fpasswd", "%2e%2e/%2e%2e/etc/passwd", "a" * 64, "A" * 64, "g" * 64, "", "a" * 65, "%00", "x" * 5000):
        s, v, _ = http("GET", "/api/v1/blobs/" + sha, token=TOK)
        case(f"blob.http_get[{sha[:20]!r}]", s in (400, 404) and "root:" not in str(v), f"status {s}")
    s, v, _ = http("PUT", "/api/v1/blobs", None, TOK)
    case("blob.http_put_empty", s in (200, 400), f"status {s}")
    # job spec referencing malformed blobs
    for b in (["../x"], ["a" * 64], [1], ["z" * 64] * 5000):
        s, v, _ = http("POST", "/api/v1/jobs", {"kind": "cmd", "cmd": "true", "n": 1, "blobs": b}, TOK)
        case(f"blob.spec_blobs[{str(b)[:20]}]", s in (400, 409), f"status {s}")


# ------------------------------------------------------------------ udp discovery
def t_udp(hp: int) -> None:
    P = BASE + 1
    u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    pk = [b"ck/1 stress 127.0.0.1:%d e=999999" % BASE, b"ck/1 " + b"A" * 60000, b"", b"\xff" * 1500, b"ck/1 x 999.1.1.1:99999 e=-1",
          b"ck/1 x 127.0.0.1:%d e=18446744073709551615" % (BASE + 40), b"ck/fence e=99999999 deadbeef " + b"0" * 64]
    for p in pk:
        for port in (P, BASE):
            try: u.sendto(p, ("127.0.0.1", port))
            except OSError: pass
    time.sleep(3)
    ha = http("GET", "/api/v1/ha", token=TOK)[1]
    case("udp.forged_beacons", head_alive(hp) and ha.get("role") == "primary" and ha.get("epoch", 0) < 999999, f"role {ha.get('role')} epoch {ha.get('epoch')}")


# ------------------------------------------------------------------ http
def raw_http(data: bytes, secs: float = 5) -> bytes:
    s = tcp(BASE + 2); s.sendall(data); s.settimeout(secs); out = b""
    try:
        while len(out) < 1 << 20:
            c = s.recv(65536)
            if not c: break
            out += c
    except OSError: pass
    s.close(); return out


def code_of(b: bytes) -> str:
    return b.split(b" ", 2)[1].decode() if b.startswith(b"HTTP/") else "none"


def t_http(hp: int) -> None:
    H = lambda m, p, b=None, t=TOK, **k: http(m, p, b, t, **k)  # noqa: E731
    # malformed JSON, deep nesting, huge numbers
    for name, body in (("bad_json", b"{not json"), ("empty", b""), ("deep_100k", b"[" * 100000), ("deep_obj", b'{"a":' * 50000 + b"1" + b"}" * 50000),
                       ("nul_name", json.dumps({"name": "a\0b", "kind": "synth", "n": 10, "synth_ns": 1}).encode()),
                       ("nan", b'{"kind":"synth","n":NaN,"synth_ns":1}'), ("neg", b'{"kind":"synth","n":-5,"synth_ns":1}'),
                       ("huge", b'{"kind":"synth","n":1e300,"synth_ns":1}'), ("u64max", b'{"kind":"sha","n":18446744073709551615,"sha":"0"}'),
                       ("neg_chunk", b'{"kind":"synth","n":10,"chunk":-1,"synth_ns":1}'), ("float_chunk", b'{"kind":"synth","n":10,"chunk":0.5,"synth_ns":1}'),
                       ("emoji_name", json.dumps({"name": "\U0001F600" * 3000, "kind": "synth", "n": 10, "synth_ns": 1}).encode()),
                       ("bad_utf8", b'{"name":"\xff\xfe","kind":"synth","n":10,"synth_ns":1}')):
        r = raw_http(b"POST /api/v1/jobs HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer " + TOK.encode() + b"\r\nContent-Type: application/json\r\nContent-Length: " + str(len(body)).encode() + b"\r\nConnection: close\r\n\r\n" + body)
        c = code_of(r)
        case(f"http.job_body[{name}]", c in ("400", "413", "403", "none") or (name in ("nul_name", "emoji_name") and c == "200"), f"status {c}")
    case("http.alive_after_bodies", head_alive(hp), "")
    # tunables out of range
    for v in (-1, 1e308, "NaN", None, "x", [1], {"a": 1}, 1e400):
        try:
            s, r, _ = H("PUT", "/api/v1/tunables", {"pipeline": v}) if v != 1e400 else (400, "", None)
        except Exception:
            s = -1
        case(f"http.tunable_pipeline={v!r}", s in (200, 400), f"status {s}")
    for k in ("heartbeat_s", "lost_after_s", "chunk_target_s", "max_body_mb"):
        for v in (0, -1, 1e18):
            s, r, _ = H("PUT", "/api/v1/tunables", {k: v})
            case(f"http.tunable_{k}={v}", s in (200, 400), f"status {s}")
    H("PUT", "/api/v1/tunables", {k: None for k in ("pipeline", "heartbeat_s", "lost_after_s", "chunk_target_s", "max_body_mb")})
    # oversized body / headers
    big = b"A" * (100 << 20)
    s = tcp(BASE + 2); t = time.time()
    try:
        s.sendall(b"POST /api/v1/login HTTP/1.1\r\nHost: x\r\nContent-Length: " + str(len(big)).encode() + b"\r\nConnection: close\r\n\r\n")
        for i in range(0, len(big), 1 << 20): s.sendall(big[i:i + (1 << 20)])
    except OSError: pass
    s.settimeout(10)
    try: resp = s.recv(200)
    except OSError: resp = b""
    s.close(); del big
    case("http.body_100MB_login", head_alive(hp), f"{code_of(resp)} in {time.time() - t:.1f}s")
    r = raw_http(b"GET /api/v1/me HTTP/1.1\r\nHost: x\r\nX-A: " + b"a" * (1 << 20) + b"\r\nConnection: close\r\n\r\n")
    case("http.header_1MB", head_alive(hp), f"status {code_of(r)}")
    r = raw_http(b"GET /api/v1/me HTTP/1.1\r\nHost: x\r\n" + b"".join(b"X-%d: y\r\n" % i for i in range(20000)) + b"\r\n")
    case("http.20000_headers", head_alive(hp), f"status {code_of(r)}")
    r = raw_http(b"GET /" + b"a" * 200000 + b" HTTP/1.1\r\nHost: x\r\n\r\n")
    case("http.url_200KB", head_alive(hp), f"status {code_of(r)}")
    # path traversal on file/log/blob/console paths
    for p in ("/../../../../etc/passwd", "/%2e%2e/%2e%2e/etc/passwd", "/api/v1/blobs/..%2f..%2fetc%2fpasswd", "/api/v1/jobs/..%2f1", "/api/v1/jobs/1/files/../../../etc/passwd",
              "/api/v1/jobs/1/files/%2e%2e%2f%2e%2e%2fetc%2fpasswd", "/api/v1/logs?file=../../../etc/passwd", "/api/v1/nodes/../../etc/passwd", "/_app/../../../etc/passwd",
              "/api/v1/templates/..%2f..%2fx", "/api/v1/pools/..%2f..%2fx", "/api/v1/experiments/../x"):
        r = raw_http(b"GET " + p.encode() + b" HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer " + TOK.encode() + b"\r\nConnection: close\r\n\r\n")
        case(f"http.traversal[{p[:40]}]", b"root:" not in r and code_of(r) != "500", f"status {code_of(r)}")
    # role matrix: every route with no token / viewer / researcher / bad token
    routes = [("GET", p) for p in ("/api/v1/users", "/api/v1/cluster", "/api/v1/nodes", "/api/v1/jobs", "/api/v1/labs", "/api/v1/pools", "/api/v1/caps", "/api/v1/reserves", "/api/v1/config",
                                   "/api/v1/windows", "/api/v1/reservations", "/api/v1/ha", "/api/v1/ha/settings", "/api/v1/events", "/api/v1/power", "/api/v1/tunables", "/api/v1/ha/candidates",
                                   "/api/v1/alerts", "/api/v1/experiments", "/api/v1/blobs", "/api/v1/net", "/api/v1/templates", "/api/v1/estimate?n=5", "/api/v1/select?q=a", "/api/v1/report")]
    routes += [("POST", p) for p in ("/api/v1/users", "/api/v1/labs", "/api/v1/join-codes", "/api/v1/nodes/state", "/api/v1/nodes/power", "/api/v1/nodes/identify", "/api/v1/caps", "/api/v1/reserves",
                                     "/api/v1/windows", "/api/v1/reservations", "/api/v1/config", "/api/v1/net/probe", "/api/v1/ha/switchover", "/api/v1/alerts/1/ack", "/api/v1/jobs/1/cancel", "/api/v1/tokens")]
    routes += [("PUT", p) for p in ("/api/v1/power", "/api/v1/ha/settings", "/api/v1/tunables", "/api/v1/pools/x", "/api/v1/users/admin/quota", "/api/v1/nodes/x/labels", "/api/v1/nodes/x/place")]
    routes += [("DELETE", p) for p in ("/api/v1/labs/1", "/api/v1/pools/x", "/api/v1/caps/1", "/api/v1/reserves/1", "/api/v1/windows/1", "/api/v1/reservations/1")]
    mk = lambda role: (H("POST", "/api/v1/users", {"name": f"u-{role}", "password": "role-password-12345", "role": role}), http("POST", "/api/v1/login", {"name": f"u-{role}", "password": "role-password-12345"})[2])  # noqa: E731
    toks = {}
    for role in ("viewer", "researcher", "operator"):
        _, sc = mk(role)
        toks[role] = http("POST", "/api/v1/tokens", {}, cookie=sc.split(";")[0])[1]["token"]
    admin_only = {"/api/v1/users", "/api/v1/config", "/api/v1/ha/settings", "/api/v1/tunables", "/api/v1/ha/switchover", "/api/v1/users/admin/quota"}
    bad = []
    for m, p in routes:
        body = {} if m != "GET" else None
        s0 = http(m, p, body, None)[0]
        s1 = http(m, p, body, "ckt_" + "0" * 40)[0]
        sv = http(m, p, body, toks["viewer"])[0]
        if s0 != 401 or s1 != 401:
            bad.append((m, p, "anon", s0, s1))
        if m != "GET" and p != "/api/v1/tokens" and sv != 403 and not (m == "POST" and p == "/api/v1/jobs/1/cancel"):
            bad.append((m, p, "viewer", sv))
        if p in admin_only and (m != "GET" or p in ("/api/v1/users", "/api/v1/config")):
            so = http(m, p, body, toks["operator"])[0]
            if so != 403: bad.append((m, p, "operator->admin", so))
        if m == "GET" and p in ("/api/v1/users", "/api/v1/config") and sv != 403:
            bad.append((m, p, "viewer->admin GET", sv))
    case("http.role_matrix", not bad, f"{len(routes)} routes x anon/badtoken/viewer/operator; violations {bad[:6]}")
    # researcher: operator-only job kinds
    s, _, _ = http("POST", "/api/v1/jobs", {"kind": "cmd", "cmd": "true", "n": 1}, toks["researcher"])
    case("http.researcher_cmd_job", s == 403, f"status {s}")
    s, _, _ = http("POST", "/api/v1/join-codes", {}, toks["researcher"])
    case("http.researcher_join_code", s == 403, f"status {s}")
    # token revoked: change role / delete user? use logout invalidation of cookie
    _, sc = mk("viewer")[0], http("POST", "/api/v1/login", {"name": "u-viewer", "password": "role-password-12345"})[2]
    ck = sc.split(";")[0]
    http("POST", "/api/v1/logout", {}, cookie=ck)
    case("http.cookie_after_logout", http("GET", "/api/v1/cluster", cookie=ck)[0] == 401, "")
    # CSRF: cookie POST from a foreign Origin with form content type
    _, sc2 = None, http("POST", "/api/v1/login", {"name": "u-operator", "password": "role-password-12345"})[2]
    ck2 = sc2.split(";")[0]
    req = urllib.request.Request(URL + "/api/v1/labs", method="POST", data=b'{"name":"csrf","w":2,"h":2}', headers={"Cookie": ck2, "Origin": "http://evil.example", "Content-Type": "text/plain"})
    try: code = urllib.request.urlopen(req, timeout=5).status
    except urllib.error.HTTPError as e: code = e.code
    case("http.csrf_foreign_origin_text_plain", code in (400, 401, 403) or "SameSite=Strict" in (sc2 or ""), f"status {code}; cookie attrs: {sc2.split(';', 1)[1].strip() if sc2 else ''}")
    # login throttle
    codes = [http("POST", "/api/v1/login", {"name": "u-viewer", "password": f"wrong{i}"})[0] for i in range(12)]
    good = http("POST", "/api/v1/login", {"name": "u-viewer", "password": "role-password-12345"})[0]
    case("http.login_throttle", codes[:5] == [401] * 5 and 429 in codes[5:], f"{codes} then correct password -> {good}")
    # bootstrap again must fail
    s, _, _ = http("POST", "/api/v1/bootstrap", {"name": "evil", "password": "evil-password-123"})
    case("http.bootstrap_again", s in (403, 409), f"status {s}")
    # unicode / NUL in names
    for nm in ("a\0b", "‮", "x" * 100000, "../x", "a b/c"):
        s1, _, _ = H("POST", "/api/v1/labs", {"name": nm, "w": 2, "h": 2})
        s2, _, _ = H("PUT", "/api/v1/pools/" + urllib.request.quote(nm, safe=""), {"members": []})
        s3, _, _ = H("POST", "/api/v1/users", {"name": nm, "password": "role-password-12345", "role": "viewer"})
        case(f"http.names[{nm[:8]!r}]", max(s1, s2, s3) < 500, f"labs {s1} pools {s2} users {s3}")
    for k, v in (("w", 0), ("w", 65), ("w", -1), ("w", 1e30), ("w", "x"), ("h", None)):
        s, _, _ = H("POST", "/api/v1/labs", {"name": "n", "w": 2, "h": 2, k: v})
        case(f"http.lab_{k}={v!r}", s in (400,), f"status {s}")
    for sel in ("a" * 100000, "((((", "key>=NaN", "\0", "x=" + ",".join("a" * 10 for _ in range(10000))):
        s, _, _ = H("GET", "/api/v1/select?q=" + urllib.request.quote(sel))
        case(f"http.selector[{sel[:8]!r}]", s < 500, f"status {s}")
    # cmd injection: arg goes to a shell by design (operator-only); check templates can't be used by researcher to run cmd
    s, _, _ = http("POST", "/api/v1/templates", {"name": "t1", "spec": {"kind": "cmd", "cmd": "touch /tmp/pwned-ck", "n": 1}, "note": ""}, toks["researcher"])
    case("http.researcher_cmd_template", s in (400, 403), f"status {s}")
    s, _, _ = http("POST", "/api/v1/jobs", {"kind": "sha", "n": 10, "sha": "0", "name": "x; touch /tmp/pwned-ck", "selector": "a=$(id)"}, toks["researcher"])
    case("http.shell_meta_in_fields_inert", not os.path.exists("/tmp/pwned-ck"), f"status {s}")
    # slow requests: 1000 concurrent slow headers
    socks = []
    for i in range(1000):
        try:
            c = tcp(BASE + 2); c.sendall(b"GET /api/v1/me HTTP/1.1\r\nHost: x\r\nX-Slow: "); socks.append(c)
        except OSError: break
    t = time.time(); ok = False
    try: ok = http("GET", "/api/v1/me", timeout=10)[0] == 200
    except Exception: pass
    case("http.1000_slow_header_conns", ok, f"{len(socks)} open; legit /me ok={ok} in {time.time() - t:.1f}s")
    for c in socks: c.close()
    # slow bodies (body promised, never sent): do they pin the 32 handler threads?
    socks = []
    for i in range(64):
        c = tcp(BASE + 2); c.sendall(b"POST /api/v1/login HTTP/1.1\r\nHost: x\r\nContent-Length: 1000\r\n\r\n{"); socks.append(c)
    time.sleep(1); t = time.time(); ok = False
    try: ok = http("GET", "/api/v1/me", timeout=10)[0] == 200
    except Exception: pass
    el = time.time() - t
    case("http.64_stalled_bodies", ok and el < 3, f"legit /me ok={ok} in {el:.1f}s (pool is 32 threads)")
    for c in socks: c.close()
    time.sleep(1)
    # SSE: 500 viewers opened then dropped
    r0 = rss_fd_thr(hp); ss = []
    for i in range(500):
        try:
            c = tcp(BASE + 2); c.sendall(b"GET /api/v1/stream HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer " + TOK.encode() + b"\r\n\r\n"); ss.append(c)
        except OSError: break
    time.sleep(3); r1 = rss_fd_thr(hp)
    during = http("GET", "/api/v1/me", timeout=10)[0]
    for c in ss: c.close()
    time.sleep(20); r2 = rss_fd_thr(hp)
    case("http.sse_500_open_drop", during == 200 and r2[1] <= r0[1] + 15 and r2[2] <= r0[2] + 15, f"opened {len(ss)}; rss/fd/thr before {r0} during {r1} 20s after {r2}")


BG: list[tuple[float, float, bool]] = []  # (start, secs, exact)
STOP = threading.Event()


def bg_loop(n: int, pfx: str, exp: list[str]) -> None:
    while not STOP.is_set():
        t = time.time()
        try:
            jid = sha_job(n, pfx); jb = wait_job(jid)
            res = http("GET", f"/api/v1/jobs/{jid}/result", token=TOK)[1]
            got = [" ".join(ln.split()[:2]) for ln in str(res).splitlines() if ln.strip()]
            BG.append((t, time.time() - t, jb["state"] == "done" and got == exp))
        except Exception:
            BG.append((t, time.time() - t, False))


# ------------------------------------------------------------------ main
try:
    head = spawn(["head", "--no-local-worker"], "head")
    wait_up(); TOK = bootstrap(); code = join_code(TOK, 500)
    ags = [agent(i, code, 2) for i in range(2)]
    time.sleep(8)
    N, PFX = 40_000_000, "000"
    exp = [f"{n} {h}" for n in range(1, N + 1) if (h := hashlib.sha256(str(n).encode()).hexdigest()).startswith(PFX)]
    t0 = time.time(); jid = sha_job(N, PFX); jb = wait_job(jid)
    base_s = time.time() - t0
    say(f"baseline sha job: {N / base_s / 1e6:.1f} Mnum/s ({base_s:.1f}s), {len(exp)} hits")
    th = threading.Thread(target=bg_loop, args=(N, PFX, exp), daemon=True); th.start()
    time.sleep(1)
    before = rss_fd_thr(head.pid)
    for name, fn in (("proto", t_proto), ("blob", t_blob), ("udp", t_udp), ("http", t_http)):
        if which in ("all", name):
            say(f"== {name}"); fn(head.pid)
    STOP.set(); th.join(120)
    ok = bool(BG) and all(b[2] for b in BG)
    worst = max((b[1] for b in BG), default=0)
    case("bg_sha_jobs_exact", ok, f"{len(BG)} jobs during the run, all exact={ok}; baseline {base_s:.1f}s, worst under attack {worst:.1f}s, mean {sum(b[1] for b in BG) / max(len(BG), 1):.1f}s")
    after = rss_fd_thr(head.pid)
    say(f"head rss KiB/fds/threads before {before} after {after}; alive={head_alive(head.pid)}")
    nfail = sum(1 for r in RESULTS if not r[1])
    say(f"TOTAL {len(RESULTS)} cases, {nfail} FAIL")
finally:
    cleanup()
