#!/usr/bin/env python3
"""Round 3 / t2: storage and state bad cases. One scenario per run:
  CK_BIN=... STRESS_DIR=... CK_PORT=9740 python3 -I scripts/stress/diskfault.py corrupt|kill9|diskfull|rodata|datadel|blobcache|standby|backup|retention
Each prints one `RESULT <case> | PASS/BUG/LIMIT | numbers` line per case. Uses the secured fleet helpers of common.py; never touches ./ck-data."""
from __future__ import annotations

import os
import random
import shutil
import sqlite3
import subprocess
import sys
import time
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402
from common import DIR, URL, bootstrap, cleanup, http, say, spawn, wait_up  # noqa: E402

PW = "stress-test-password-1"


def result(case: str, verdict: str, detail: str) -> None:
    say(f"RESULT {case} | {verdict} | {detail}")


def start_head(extra: dict | None = None, data: str = "data", args: list[str] | None = None, name: str = "head") -> subprocess.Popen:
    return spawn(["head"] + (args or []), name, extra, data=data)


def login() -> str:
    return bootstrap(PW)


def submit(tok: str, n: int, chunk: int, ns: int = 0, name: str = "t") -> int:
    s, j, _ = http("POST", "/api/v1/jobs", {"name": name, "kind": "synth", "synth_ns": ns, "n": n, "chunk": chunk, "reducer": "sum"}, tok)
    assert s in (200, 201), (s, j)
    return j["job"]["id"]


def wait_job(tok: str, jid: int, secs: float = 120, base: str = URL) -> dict:
    t = time.time()
    jb: dict = {}
    while time.time() - t < secs:
        try:
            s, v, _ = http("GET", f"/api/v1/jobs/{jid}", token=tok, base=base, timeout=5)
            if s == 200:
                jb = v["job"]
                if jb["state"] in ("done", "failed", "cancelled"):
                    return jb
        except Exception:
            pass
        time.sleep(0.5)
    return jb


def total(tok: str, jid: int, base: str = URL) -> str:
    return str(http("GET", f"/api/v1/jobs/{jid}/result?reducer=sum", token=tok, base=base)[1]).strip()


def kill(p: subprocess.Popen) -> None:
    try:
        os.killpg(p.pid, 18)
        os.killpg(p.pid, 9)
    except Exception:
        pass
    try:
        p.wait(5)
    except Exception:
        pass


def d(name: str = "data") -> str:
    return os.path.join(DIR, name)


def log_tail(name: str, pat: str = "") -> list[str]:
    try:
        return [x.rstrip() for x in open(os.path.join(DIR, name + ".log"), errors="replace") if pat in x][-5:]
    except Exception:
        return []


# ---------------------------------------------------------------- corrupt
def corrupt() -> None:
    p = start_head(); wait_up(); tok = login()
    N = 1_000_000
    jid = submit(tok, N, 20)
    jb = wait_job(tok, jid); good = total(tok, jid)
    say("baseline", jb.get("state"), jb.get("done"), good)
    kill(p)
    base = d("data"); time.sleep(1)
    # checkpoint WAL fully so file variants are meaningful: open + wal_checkpoint(TRUNCATE) once
    c = sqlite3.connect(os.path.join(base, "head.db")); c.execute("PRAGMA wal_checkpoint(TRUNCATE)"); c.close()
    sz = os.path.getsize(os.path.join(base, "head.db")); say("db size", sz)

    def variant(nm: str, fn: Any) -> None:
        dn = "c_" + nm
        shutil.rmtree(d(dn), ignore_errors=True); shutil.copytree(base, d(dn))
        shutil.copytree(os.path.join(DIR, "keys-data"), os.path.join(DIR, "keys-" + dn), dirs_exist_ok=True)
        fn(os.path.join(d(dn), "head.db"))
        q = start_head(data=dn, name="head_" + dn)
        time.sleep(0.5)
        try:
            wait_up(secs=15); up = True
        except Exception:
            up = False
        out = f"refused (exit {q.poll()})" if not up else ""
        if up:
            try:
                tk = login()
                s, v, _ = http("GET", f"/api/v1/jobs/{jid}", token=tk)
                tt = total(tk, jid) if s == 200 else "-"
                warn = log_tail("head_" + dn, "store.corrupt") + log_tail("head_" + dn, "corrupt")
                intact = s == 200 and tt == good
                out = f"served job={s} total={tt} intact={intact} warned={bool(warn)} {' '.join(w[:120] for w in warn[:1])}"
                result("corrupt/" + nm, "PASS" if intact or warn else "BUG", out)
            except Exception as ex:
                result("corrupt/" + nm, "PASS" if log_tail("head_" + dn, "corrupt") else "BUG", f"api error {ex!r} warned={log_tail('head_' + dn, 'corrupt')[:1]}")
        else:
            lg = log_tail("head_" + dn)
            result("corrupt/" + nm, "PASS" if lg else "BUG", out + " last log: " + (lg[-1][:200] if lg else "none"))
        kill(q)

    def rnd_pages(p: str) -> None:
        r = random.Random(7); b = bytearray(open(p, "rb").read())
        for _ in range(20):
            o = r.randrange(4096, len(b) - 4096); b[o:o + 4096] = bytes(r.randrange(256) for _ in range(4096))
        open(p, "wb").write(b)

    def trunc(p: str) -> None:
        os.truncate(p, os.path.getsize(p) // 2)

    def hdr(p: str) -> None:
        b = bytearray(open(p, "rb").read()); b[0:100] = os.urandom(100); open(p, "wb").write(b)

    def zero_mid(p: str) -> None:
        b = bytearray(open(p, "rb").read()); m = len(b) // 2; b[m:m + 65536] = bytes(65536); open(p, "wb").write(b)

    def late_pages(p: str) -> None:
        r = random.Random(9); b = bytearray(open(p, "rb").read()); n = len(b) // 4096
        for _ in range(8):
            o = r.randrange(int(n * 0.6), int(n * 0.95)) * 4096; b[o + 100:o + 3000] = bytes(r.randrange(256) for _ in range(2900))
        open(p, "wb").write(b)

    variant("late_data_pages", late_pages)
    variant("random_pages", rnd_pages)
    variant("truncated_half", trunc)
    variant("bad_header", hdr)
    variant("zeroed_64k", zero_mid)
    variant("empty_file", lambda p: open(p, "wb").close())
    variant("shm_garbage", lambda p: open(p + "-shm", "wb").write(os.urandom(32768)))
    variant("wal_garbage_no_header", lambda p: open(p + "-wal", "wb").write(os.urandom(50000)))

    # WAL cases need a WAL with live frames: run head, kill -9 mid-write, keep db+wal
    p = start_head(data="walsrc"); wait_up(); tk = login()
    j2 = submit(tk, 3_000_000, 500, 0)
    time.sleep(3)
    kill(p)
    ws = os.path.getsize(d("walsrc") + "/head.db-wal") if os.path.exists(d("walsrc") + "/head.db-wal") else 0
    say("wal size after kill9", ws)

    def wvariant(nm: str, fn: Any) -> None:
        dn = "w_" + nm
        shutil.rmtree(d(dn), ignore_errors=True); shutil.copytree(d("walsrc"), d(dn))
        shutil.copytree(os.path.join(DIR, "keys-walsrc"), os.path.join(DIR, "keys-" + dn), dirs_exist_ok=True)
        fn(os.path.join(d(dn), "head.db"))
        q = start_head(data=dn, name="head_" + dn)
        time.sleep(0.5)
        try:
            wait_up(secs=15); tk2 = login()
            # integrity of what it serves: every stored part must sum exactly to its range; job must finish exactly
            jb2 = wait_job(tk2, j2, 90)
            tt = total(tk2, j2)
            result("corrupt/" + nm, "PASS" if jb2.get("state") == "done" and tt == str(3_000_000) else "BUG", f"state={jb2.get('state')} done={jb2.get('done')} total={tt} warned={bool(log_tail('head_' + dn, 'corrupt'))}")
        except Exception as ex:
            result("corrupt/" + nm, "PASS" if q.poll() is not None or log_tail("head_" + dn, "corrupt") else "BUG", f"{ex!r} exit={q.poll()}")
        kill(q)

    wvariant("wal_deleted", lambda p: os.path.exists(p + "-wal") and os.remove(p + "-wal"))
    wvariant("wal_truncated_midframe", lambda p: os.truncate(p + "-wal", max(32, (os.path.getsize(p + "-wal") // 2) + 1000)))
    wvariant("wal_flipped_bytes", lambda p: (lambda b: (b.__setitem__(slice(5000, 5100), os.urandom(100)), open(p + "-wal", "wb").write(b)))(bytearray(open(p + "-wal", "rb").read())))


# ---------------------------------------------------------------- kill9 loop
def kill9() -> None:
    p = start_head(); wait_up(); tok = login()
    N = 4_000_000; CH = 100
    jid = submit(tok, N, CH, 80000, "k9")
    time.sleep(5); j0 = http("GET", f"/api/v1/jobs/{jid}", token=tok)[1]["job"]; say("rate probe: done", j0["done"], "of", N, "in 5s, parts/s ~", j0["done"] // CH // 5)
    ok = True; rounds = 0
    for i in range(20):
        time.sleep(random.uniform(1.5, 4))
        pre = http("GET", f"/api/v1/jobs/{jid}", token=tok, timeout=5)[1]["job"]
        kill(p)
        p = start_head(name=f"head_r{i}")
        try:
            wait_up(secs=30); tok = login()
        except Exception as ex:
            result(f"kill9/restart{i}", "BUG", repr(ex)); ok = False; break
        rounds += 1
        post = http("GET", f"/api/v1/jobs/{jid}", token=tok, timeout=5)[1]["job"]
        # done count must never go backwards beyond the un-synced WAL tail (NORMAL sync only loses on OS crash, not process kill)
        say(f"round {i}: done before kill={pre['done']} after restart={post['done']} state={post['state']}")
        if post["done"] < pre["done"]:
            ok = False; say("  REGRESSION in done count")
        if post["state"] == "done":
            break
    jb = wait_job(tok, jid, 180)
    tt = total(tok, jid)
    con = sqlite3.connect(os.path.join(d("data"), "head.db"))
    nparts, dup = con.execute("SELECT count(*), count(*)-count(DISTINCT start) FROM parts WHERE job_id=?", (jid,)).fetchone()
    ov = con.execute("SELECT count(*) FROM parts a JOIN parts b ON a.job_id=b.job_id AND a.start<b.start AND b.start<a.\"end\" WHERE a.job_id=?", (jid,)).fetchone()[0]
    sm = con.execute("SELECT sum(\"end\"-start) FROM parts WHERE job_id=?", (jid,)).fetchone()[0]
    result("kill9/20x", "PASS" if ok and jb.get("state") == "done" and tt == str(N) and ov == 0 and dup == 0 and sm == N else "BUG",
           f"rounds={rounds} state={jb.get('state')} done={jb.get('done')} total={tt} parts={nparts} dup_start={dup} overlaps={ov} covered={sm}")


# ---------------------------------------------------------------- disk full
def diskfull() -> None:
    """Needs a small mounted volume: DISK=/Volumes/ckfull (hdiutil create -size 100m -fs HFS+ -volname ckfull -attach x.dmg).
    A ballast file leaves ~10 MB free; a cmd job writes 1000 x 20 KB result parts, so the disk fills mid-job; then the ballast is freed."""
    vol = os.environ.get("DISK", "/Volumes/ckfull")
    data = os.path.join(vol, "data")
    shutil.rmtree(data, ignore_errors=True)
    for f in os.listdir(vol):
        if f.startswith("ballast"):
            os.remove(os.path.join(vol, f))
    ex = {"CK_DATA": data}
    p = start_head(ex); wait_up(); tok = login()
    ball = os.path.join(vol, "ballast")
    free = shutil.disk_usage(vol).free
    with open(ball, "wb") as f:
        left = free - 10 * 1048576
        while left > 0:
            f.write(b"\0" * 1048576); left -= 1048576
    say("free after ballast MB", shutil.disk_usage(vol).free // 1048576)
    N = 1000
    cmd = "printf '%s %s\\n' $CK_START $CK_END; head -c $((10000*(CK_END-CK_START))) /dev/zero | tr '\\0' x; echo"
    s, j, _ = http("POST", "/api/v1/jobs", {"name": "full", "kind": "cmd", "cmd": cmd, "n": N, "chunk": 1}, tok)
    say("submit", s, str(j)[:120]); jid = j["job"]["id"]
    t = time.time(); 
    while time.time() - t < 25:
        time.sleep(2)
        jb = http("GET", f"/api/v1/jobs/{jid}", token=tok, timeout=8)[1]["job"]
        say(f"  t={time.time() - t:.0f} state={jb['state']} done={jb['done']} free={shutil.disk_usage(vol).free // 1048576}MB")
    errs = log_tail("head", "job.error") + log_tail("head", "rror")
    say("logs during:", [x[:160] for x in errs[-3:]], "alive", p.poll() is None)
    jb_full = http("GET", f"/api/v1/jobs/{jid}", token=tok, timeout=8)[1]["job"]
    al = [a for a in http("GET", "/api/v1/alerts?active=1", token=tok, timeout=8)[1].get("alerts", []) if a["kind"] == "store_write"]
    result("diskfull/alert_visible", "PASS" if al else "BUG", f"store_write alerts while full: {len(al)}")
    os.remove(ball)
    jb = wait_job(tok, jid, 240)
    con = sqlite3.connect(os.path.join(data, "head.db"))
    rows = con.execute("SELECT start,\"end\",length(body) FROM parts WHERE job_id=? ORDER BY start", (jid,)).fetchall()
    cov = sum(r[1] - r[0] for r in rows)
    bad = sum(1 for r in rows if r[2] < 10000 * (r[1] - r[0]))
    gaps = sum(1 for x, y in zip(rows, rows[1:]) if x[1] != y[0])
    say("post: state", jb.get("state"), "done", jb.get("done"), "parts", len(rows), "covered", cov, "bad_bodies", bad, "gaps", gaps, "done_at_full", jb_full["done"])
    result("diskfull/job", "PASS" if jb.get("state") == "done" and cov == N and bad == 0 and gaps == 0 else "BUG",
           f"state={jb.get('state')} done={jb.get('done')} stored_numbers={cov}/{N} bad_bodies={bad} gaps={gaps} done_while_full={jb_full['done']}")
    j2 = submit(tok, 100_000, 100, 0, "after"); jb2 = wait_job(tok, j2, 60)
    result("diskfull/recover", "PASS" if jb2.get("state") == "done" and total(tok, j2) == "100000" else "BUG", f"new job state={jb2.get('state')}")


def rodata() -> None:
    """Read-only CK_DATA: (a) never-writable dir at start, (b) db file readable but dir/files read-only."""
    p = start_head(); wait_up(); tok = login(); jid = submit(tok, 100_000, 100); wait_job(tok, jid); kill(p); time.sleep(1)
    for f in os.listdir(d()):
        os.chmod(os.path.join(d(), f), 0o444)
    os.chmod(d(), 0o555)
    q = start_head(name="head_ro")
    time.sleep(4)
    code = q.poll()
    lg = log_tail("head_ro")
    up = True
    try:
        wait_up(secs=3)
    except Exception:
        up = False
    detail = f"exit={code} up={up} log={lg[-1][:200] if lg else 'none'}"
    if up:
        try:
            tk = login(); detail += f" login_ok={bool(tk)}"
        except Exception as ex:
            detail += f" login_err={ex!r}"
    result("rodata/start", "PASS" if code is not None and lg or (up and False) else "BUG", detail)
    kill(q)
    os.chmod(d(), 0o755)
    for f in os.listdir(d()):
        os.chmod(os.path.join(d(), f), 0o644)
    # restored perms: head runs again, data intact
    q = start_head(name="head_ro2"); wait_up(); tk = login()
    result("rodata/recover", "PASS" if total(tk, jid) == "100000" else "BUG", f"total={total(tk, jid)}")


def datadel() -> None:
    p = start_head(); wait_up(); tok = login()
    jid = submit(tok, 2_000_000, 100, 80000)
    time.sleep(6)
    shutil.rmtree(d())
    time.sleep(3)
    s, v, _ = http("GET", f"/api/v1/jobs/{jid}", token=tok, timeout=8)
    say("after rm -rf: api", s, v["job"]["done"] if s == 200 else str(v)[:100])
    jb = wait_job(tok, jid, 120)
    tt = total(tok, jid)
    lg = log_tail("head", "rror") + log_tail("head", "warn")
    say("logs", [x[:160] for x in lg[-3:]])
    s2 = http("POST", "/api/v1/jobs", {"name": "n", "kind": "synth", "synth_ns": 0, "n": 1000, "chunk": 100}, tok)[0]
    say("final state", jb.get("state"), jb.get("done"), tt, "submit new job", s2, "datadir exists", os.path.exists(d()), "alive", p.poll() is None)
    kill(p)
    start_head(name="head_after"); time.sleep(2)
    try:
        wait_up(secs=10); tk = login(); r = http("GET", f"/api/v1/jobs/{jid}", token=tk)[0]
    except Exception as ex:
        r = repr(ex)
    result("datadel/running", "LIMIT", f"job finished {jb.get('state')} total={tt} while db unlinked; after restart job lookup={r} (data gone as expected, head gave no alarm: logs={len(lg)})")


def put_blob(tok: str, data: bytes) -> str:
    import urllib.request
    r = urllib.request.Request(URL + "/api/v1/blobs", method="PUT", data=data, headers={"Authorization": "Bearer " + tok})
    import json as _j
    return _j.loads(urllib.request.urlopen(r, timeout=60).read())["sha256"]


def blob_job(tok: str, sha: str, name: str, wait: float = 60) -> str:
    cmd = "shasum -a 256 $CK_BLOB_0 #"
    s, j, _ = http("POST", "/api/v1/jobs", {"name": name, "kind": "cmd", "cmd": cmd, "n": 1, "chunk": 1, "blobs": [sha]}, tok)
    if s not in (200, 201):
        return f"submit {s} {str(j)[:100]}"
    jb = wait_job(tok, j["job"]["id"], wait)
    out = str(http("GET", f"/api/v1/jobs/{j['job']['id']}/result", token=tok)[1]).strip()
    return f"{jb.get('state')}:{out[:64]}" if jb.get("state") == "done" else f"{jb.get('state')}:{jb.get('error')}"


def blobcache() -> None:
    import hashlib
    start_head(args=["--no-local-worker"]); wait_up(); tok = login(); code = common.join_code(tok)
    ag = common.agent(0, code)
    time.sleep(6)
    data = os.urandom(6 * 1048576); want = hashlib.sha256(data).hexdigest()
    sha = put_blob(tok, data); assert sha == want
    cdir = os.path.join(d("agent0"), "cache"); cf = os.path.join(cdir, sha)
    r = blob_job(tok, sha, "b0"); say("baseline", r, "cached file", os.path.exists(cf))
    result("blobcache/baseline", "PASS" if r == f"done:{want}" else "BUG", r)

    def case(nm: str, mutate: Any, restart: bool) -> None:
        nonlocal ag
        if restart:
            kill(ag)
        mutate()
        if restart:
            ag = common.agent(0, code); time.sleep(6)
        r = blob_job(tok, sha, "b_" + nm)
        ok = os.path.exists(cf) and hashlib.sha256(open(cf, "rb").read()).hexdigest() == want
        result("blobcache/" + nm, "PASS" if r == f"done:{want}" and ok else "BUG", f"job={r[:80]} cache_file_ok_after={ok}")

    def flip() -> None:
        b = bytearray(open(cf, "rb").read())
        for o in (100, 3_000_000, len(b) - 10):
            b[o] ^= random.randrange(1, 256)
        open(cf, "wb").write(b)

    def zero() -> None:
        n = os.path.getsize(cf); open(cf, "wb").write(bytes(n))

    def half() -> None:
        os.truncate(cf, os.path.getsize(cf) // 2)

    case("flip_bytes_agent_running", flip, False)
    case("flip_bytes_agent_restarted", flip, True)
    case("zero_filled_same_size", zero, True)
    case("truncated_half", half, True)
    case("deleted", lambda: os.remove(cf), True)
    case("garbage_part_file", lambda: (os.remove(cf), open(cf + ".part", "wb").write(os.urandom(6 * 1048576))), True)
    case("idx_garbage", lambda: (os.remove(cf), open(cf + ".idx", "wb").write(os.urandom(100))), True)


def fleet(sb_data: str = "standby", sb_extra: dict | None = None, agents: int = 2) -> tuple:
    """Primary (no local worker) + one standby on BASE+5 + agents; returns (head, tok, code, standby, agent procs)."""
    head = start_head(args=["--no-local-worker"]); wait_up(); tok = login(); code = common.join_code(tok)
    sbp = common.BASE + 5
    ex = {"CK_PORT": str(sbp), "CK_PRIMARY_PORT": str(common.BASE), "CK_NODE_ID": "stress-standby", "CK_KEY_DIR": os.path.join(DIR, "keys-standby")}
    ex.update(sb_extra or {})
    sb = spawn(["head", "--standby", "--head", "127.0.0.1", "--code", code, "--no-local-worker"], "standby", ex, data=sb_data)
    ags = [common.agent(i, code, threads=2) for i in range(agents)]
    return head, tok, code, sb, ags


def ha(tok: str) -> dict:
    return http("GET", "/api/v1/ha", token=tok, timeout=5)[1]


def sb_wait_synced(tok: str, secs: float = 40) -> dict:
    t = time.time(); h: dict = {}
    while time.time() - t < secs:
        try:
            h = ha(tok)
            sbs = h.get("standbys") or []
            if sbs and all((x.get("lag", x.get("lag_entries", 1)) == 0) for x in sbs if isinstance(x, dict)):
                return h
        except Exception:
            pass
        time.sleep(1)
    return h


def db_stats(path: str, jid: int) -> tuple:
    try:
        c = sqlite3.connect("file:" + path + "?mode=ro", uri=True, timeout=5)
        ic = c.execute("PRAGMA integrity_check").fetchone()[0]
        n, sm = c.execute("SELECT count(*), coalesce(sum(\"end\"-start),0) FROM parts WHERE job_id=?", (jid,)).fetchone()
        return ic, n, sm
    except Exception as ex:
        return ("err " + str(ex)[:60], -1, -1)


def standby() -> None:
    head, tok, code, sb, ags = fleet()
    time.sleep(8)
    N = 2_000_000
    jid = submit(tok, N, 100, 20000, "sbj"); jb = wait_job(tok, jid, 120)
    h = sb_wait_synced(tok); say("ha", str(h)[:300])
    sbdb = os.path.join(d("standby"), "head.db")
    pdb = os.path.join(d("data"), "head.db")
    pstat = db_stats(pdb, jid); say("primary", pstat, "job", jb.get("state"), total(tok, jid))
    for nm in ("bad_header", "late_pages", "db_deleted_wal_kept"):
        kill(sb); time.sleep(1)
        b = bytearray(open(sbdb, "rb").read()) if os.path.exists(sbdb) else bytearray(8192)
        if nm == "bad_header":
            b[0:100] = os.urandom(100); open(sbdb, "wb").write(b)
        elif nm == "late_pages":
            r = random.Random(3); n = max(2, len(b) // 4096)
            for _ in range(6):
                o = r.randrange(n // 2, n) * 4096; b[o + 50:o + 2000] = os.urandom(1950)
            open(sbdb, "wb").write(b)
        else:
            os.remove(sbdb)
        ex = {"CK_PORT": str(common.BASE + 5), "CK_PRIMARY_PORT": str(common.BASE), "CK_NODE_ID": "stress-standby", "CK_KEY_DIR": os.path.join(DIR, "keys-standby")}
        sb = spawn(["head", "--standby", "--head", "127.0.0.1", "--code", code, "--no-local-worker"], "standby_" + nm, ex, data="standby")
        t = time.time(); st = (None, None, None)
        while time.time() - t < 45:
            time.sleep(3)
            if sb.poll() is not None:
                break
            st = db_stats(sbdb, jid)
            if st == pstat:
                break
        lg = log_tail("standby_" + nm)
        ok = st == pstat
        result("standby/corrupt_" + nm, "PASS" if ok else ("LIMIT" if sb.poll() is not None else "BUG"),
               f"standby exit={sb.poll()} db(integrity,parts,numbers)={st} primary={pstat} last_log={(lg[-1][:140] if lg else '')}")
        if sb.poll() is not None:  # operator remedy per OPERATIONS.md: delete the db, start again, wait for the full copy
            kill(sb); shutil.rmtree(d("standby"), ignore_errors=True)
            sb = spawn(["head", "--standby", "--head", "127.0.0.1", "--code", code, "--no-local-worker"], "standby_re_" + nm, ex, data="standby")
            sb_wait_synced(tok, 60); time.sleep(3)


def table_sums(path: str) -> dict:
    c = sqlite3.connect("file:" + path + "?mode=ro", uri=True)
    out = {}
    for t, cols in (("parts", "job_id,start,\"end\",body"), ("jobs", "*"), ("users", "*"), ("tokens", "*"), ("nodes", "node_id,state")):
        rows = c.execute(f"SELECT {cols} FROM {t} ORDER BY 1,2").fetchall() if t == "parts" else c.execute(f"SELECT {cols} FROM {t}").fetchall()
        import hashlib
        h = hashlib.sha256(repr(sorted(map(repr, rows))).encode()).hexdigest()[:12]
        out[t] = (len(rows), h)
    return out


def backup() -> None:
    p = start_head(); wait_up(); tok = login()
    N = 3_000_000
    jid = submit(tok, N, 100, 80000, "bk")
    time.sleep(6)
    bk = os.path.join(DIR, "mid.db")
    for f in (bk,):
        if os.path.exists(f):
            os.remove(f)
    t0 = time.time()
    # dispatch latency while the backup runs: submit tiny jobs, time the POST
    lat = []
    pr = subprocess.Popen([BIN_(), "admin", "backup", bk], env=common.env(), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    while pr.poll() is None:
        t1 = time.time(); http("GET", "/api/v1/jobs", token=tok, timeout=10); lat.append(time.time() - t1); time.sleep(0.1)
    out = pr.stdout.read().strip() if pr.stdout else ""
    say("backup mid-load: rc", pr.returncode, "took %.2fs" % (time.time() - t0), "max api latency %.0f ms" % (max(lat or [0]) * 1000), out[:100])
    ic = db_stats(bk, jid)
    wait_job(tok, jid, 200); want = total(tok, jid)
    live = table_sums(os.path.join(d(), "head.db"))
    # quiescent backup for exact equality
    bk2 = os.path.join(DIR, "final.db")
    if os.path.exists(bk2):
        os.remove(bk2)
    r2 = subprocess.run([BIN_(), "admin", "backup", bk2], env=common.env(), capture_output=True, text=True)
    kill(p); time.sleep(1)
    c = sqlite3.connect(os.path.join(d(), "head.db")); c.execute("PRAGMA wal_checkpoint(TRUNCATE)"); c.close()
    live = table_sums(os.path.join(d(), "head.db")); bs = table_sums(bk2)
    result("backup/final_equals_live", "PASS" if live == bs and r2.returncode == 0 else "BUG", f"rc={r2.returncode} live={live} backup={bs}")
    # restore (a) the mid-load backup: job must resume and finish exactly
    for nm, src, expect_resume in (("mid", bk, True), ("final", bk2, False)):
        rd = "restore_" + nm
        shutil.rmtree(d(rd), ignore_errors=True); os.makedirs(d(rd))
        shutil.copy(src, os.path.join(d(rd), "head.db"))  # docs: replace head.db, no -wal / -shm
        shutil.copytree(os.path.join(DIR, "keys-data"), os.path.join(DIR, "keys-" + rd), dirs_exist_ok=True)
        q = start_head(data=rd, name="head_" + rd); wait_up(secs=30)
        tk = login()
        jb2 = wait_job(tk, jid, 240); tt = total(tk, jid)
        result("backup/restore_" + nm, "PASS" if jb2.get("state") == "done" and tt == want == str(N) and ic[0] == "ok" else "BUG",
               f"integrity={ic[0] if nm == 'mid' else '-'} job={jb2.get('state')} done={jb2.get('done')} total={tt} want={want} backup_had_parts={ic[1] if nm == 'mid' else '-'}")
        kill(q)


def BIN_() -> str:
    return common.BIN


def retention() -> None:
    p = start_head(); wait_up(); tok = login()
    old = [submit(tok, 1000, 100, 0, f"old{i}") for i in range(5)]
    for j in old:
        wait_job(tok, j, 30)
    kill(p); time.sleep(1)
    dbp = os.path.join(d(), "head.db")
    c = sqlite3.connect(dbp)
    day = 86_400_000; now = int(time.time() * 1000)
    c.execute("DELETE FROM parts WHERE job_id IN ({})".format(",".join(map(str, old))))
    c.execute("UPDATE jobs SET finished_ms=? WHERE id IN ({})".format(",".join(map(str, old))), (now - 10 * day,))
    body = b"x" * 100
    for j in old:
        c.executemany("INSERT INTO parts (job_id,start,\"end\",body) VALUES (?,?,?,?)", ((j, i * 10, i * 10 + 10, body) for i in range(100_000)))
    c.executemany("INSERT INTO events (ts_ms,kind,actor,detail) VALUES (?,?,?,?)", ((now - 10 * day + i, "old.event", "t2", "x" * 60) for i in range(500_000)))
    c.commit(); c.close()
    sz = os.path.getsize(dbp) // 1048576
    say(f"db populated: 500k parts + 500k old events, {sz} MB")
    ex = {"CK_TUNE_PARTS_KEEP_DAYS": "1", "CK_TUNE_EVENTS_KEEP_DAYS": "1"}
    p = start_head(ex); wait_up(secs=60); tok = login()
    t0 = time.time(); samples: list = []  # (t_since_start, submit_to_done_ms, get_ms)
    while time.time() - t0 < 100:
        t1 = time.time()
        jid = submit(tok, 100_000, 100, 0, "disp")
        t2 = time.time(); http("GET", "/api/v1/jobs", token=tok, timeout=30); t3 = time.time()
        jb = wait_job(tok, jid, 60)
        samples.append((t1 - t0, (time.time() - t1) * 1000, (t3 - t2) * 1000, jb.get("state") == "done" and total(tok, jid) == "100000"))
        time.sleep(0.2)
    lg = [x.strip() for x in open(os.path.join(DIR, "head.log"), errors="replace") if "retention" in x or "prune" in x]
    before = [x for x in samples if x[0] < 28]; during = [x for x in samples if x[0] >= 28]
    def stat(v: list) -> str:
        a = sorted(x[1] for x in v); g = sorted(x[2] for x in v)
        return f"n={len(v)} submit->done p50={a[len(a)//2]:.0f}ms max={a[-1]:.0f}ms list p50={g[len(g)//2]:.0f}ms max={g[-1]:.0f}ms" if v else "n=0"
    allok = all(x[3] for x in samples)
    c = sqlite3.connect("file:" + dbp + "?mode=ro", uri=True)
    left_parts = c.execute("SELECT count(*) FROM parts WHERE job_id IN ({})".format(",".join(map(str, old)))).fetchone()[0]
    left_ev = c.execute("SELECT count(*) FROM events WHERE kind='old.event'").fetchone()[0]
    say("log:", lg[-3:])
    result("retention/dispatch_during_prune", "PASS" if allok and left_parts == 0 and left_ev == 0 else "BUG",
           f"before: {stat(before)} | during/after: {stat(during)} | all new jobs exact={allok} old parts left={left_parts} old events left={left_ev}")


def sbfull() -> None:
    """Standby disk full during snapshot and during replication (DISK=/Volumes/ckfull, 100 MB)."""
    vol = os.environ.get("DISK", "/Volumes/ckfull")
    sbdir = os.path.join(vol, "sbdata")
    shutil.rmtree(sbdir, ignore_errors=True)
    for f in os.listdir(vol):
        if f.startswith("ballast"):
            os.remove(os.path.join(vol, f))
    start_head(args=["--no-local-worker"]); wait_up(secs=30); tok = login(); code = common.join_code(tok)
    [common.agent(i, code, threads=2) for i in range(2)]
    time.sleep(6)
    cmd = "head -c $((10000*(CK_END-CK_START))) /dev/zero | tr '\\0' x; echo"
    s, j, _ = http("POST", "/api/v1/jobs", {"name": "big", "kind": "cmd", "cmd": cmd, "n": 3000, "chunk": 1000}, tok)
    big = j["job"]["id"]; jb = wait_job(tok, big, 120)
    pdb = os.path.join(d(), "head.db")
    c = sqlite3.connect(pdb); c.execute("PRAGMA wal_checkpoint(TRUNCATE)"); c.close()
    say("primary db MB", os.path.getsize(pdb) // 1048576, "big job", jb.get("state"))
    ball = os.path.join(vol, "ballast")
    with open(ball, "wb") as f:
        left = shutil.disk_usage(vol).free - 8 * 1048576
        while left > 0:
            f.write(b"\0" * 1048576); left -= 1048576
    ex = {"CK_PORT": str(common.BASE + 5), "CK_PRIMARY_PORT": str(common.BASE), "CK_NODE_ID": "stress-standby", "CK_KEY_DIR": os.path.join(DIR, "keys-standby")}
    sb = spawn(["head", "--standby", "--head", "127.0.0.1", "--code", code, "--no-local-worker"], "standby", ex, data=sbdir)
    time.sleep(15)
    say("standby alive", sb.poll() is None, "log", [x[:150] for x in log_tail("standby")[-3:]], "free MB", shutil.disk_usage(vol).free // 1048576)
    N = 1_000_000
    t0 = time.time(); jid = submit(tok, N, 100, 20000, "during"); jb = wait_job(tok, jid, 120); tt = total(tok, jid)
    result("sbfull/primary_unaffected", "PASS" if jb.get("state") == "done" and tt == str(N) else "BUG", f"job {jb.get('state')} total={tt} in {time.time() - t0:.1f}s while standby disk full; standby alive={sb.poll() is None}")
    os.remove(ball)
    t = time.time(); st = None
    pstat = db_stats(pdb, jid)
    while time.time() - t < 90:
        time.sleep(3)
        if sb.poll() is not None:
            break
        st = db_stats(os.path.join(sbdir, "head.db"), jid)
        if st == pstat:
            break
    result("sbfull/recovers", "PASS" if st == pstat else "BUG", f"after space freed: standby(integrity,parts,numbers)={st} primary={pstat} exit={sb.poll()} in {time.time() - t:.0f}s")
    # replication stage: sync is done, now fill again and run another job; then free
    with open(ball, "wb") as f:
        try:
            while True:
                f.write(b"\0" * 1048576); f.flush()
        except OSError:
            pass
    t0 = time.time(); j3 = submit(tok, N, 100, 20000, "during2"); jb3 = wait_job(tok, j3, 120); t3 = total(tok, j3)
    result("sbfull/primary_unaffected_replication", "PASS" if jb3.get("state") == "done" and t3 == str(N) else "BUG", f"job {jb3.get('state')} total={t3} in {time.time() - t0:.1f}s; standby alive={sb.poll() is None}")
    os.remove(ball)
    p3 = db_stats(pdb, j3); t = time.time(); st = None
    while time.time() - t < 90:
        time.sleep(3)
        st = db_stats(os.path.join(sbdir, "head.db"), j3)
        if st == p3 or sb.poll() is not None:
            break
    result("sbfull/replication_recovers", "PASS" if st == p3 else "BUG", f"standby={st} primary={p3} exit={sb.poll()}")


def agentfull() -> None:
    """Agent disk full during a blob fetch: no wrong result, job completes after space is freed (DISK=/Volumes/ckfull)."""
    import hashlib
    vol = os.environ.get("DISK", "/Volumes/ckfull")
    adir = os.path.join(vol, "agdata")
    shutil.rmtree(adir, ignore_errors=True)
    for f in os.listdir(vol):
        if f.startswith("ballast"):
            os.remove(os.path.join(vol, f))
    start_head(args=["--no-local-worker"]); wait_up(secs=30); tok = login(); code = common.join_code(tok)
    ball = os.path.join(vol, "ballast")
    with open(ball, "wb") as f:
        left = shutil.disk_usage(vol).free - 2 * 1048576
        while left > 0:
            f.write(b"\0" * 1048576); left -= 1048576
    ag = common.spawn(["agent", "--head", "127.0.0.1", "--threads", "1", "--name", "a0", "--code", code], "agent0", {"CK_NODE_ID": "stress-a0", "CK_KEY_DIR": os.path.join(DIR, "keys-a0")}, data=adir)
    time.sleep(6)
    data = os.urandom(6 * 1048576); want = hashlib.sha256(data).hexdigest(); sha = put_blob(tok, data)
    s, j, _ = http("POST", "/api/v1/jobs", {"name": "af", "kind": "cmd", "cmd": "shasum -a 256 $CK_BLOB_0 #", "n": 1, "chunk": 1, "blobs": [sha]}, tok)
    jid = j["job"]["id"]
    time.sleep(20)
    jb = http("GET", f"/api/v1/jobs/{jid}", token=tok)[1]["job"]
    say("while agent disk full: state", jb["state"], "error", jb.get("error"), "agent alive", ag.poll() is None, "log", [x[:150] for x in log_tail("agent0")[-2:]])
    wrong = jb["state"] == "done"
    os.remove(ball)
    jb = wait_job(tok, jid, 90)
    out = str(http("GET", f"/api/v1/jobs/{jid}/result", token=tok)[1]).strip()[:64]
    result("agentfull/blob_fetch", "PASS" if jb.get("state") == "done" and out == want and not wrong else "BUG", f"done_while_full={wrong} final={jb.get('state')} sha_ok={out == want} agent alive={ag.poll() is None}")


# ---------------------------------------------------------------- main
if __name__ == "__main__":
    sc = sys.argv[1] if len(sys.argv) > 1 else "corrupt"
    try:
        globals()[sc]()
    finally:
        cleanup()
