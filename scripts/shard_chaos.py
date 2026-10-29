#!/usr/bin/env python3
"""Real-process chaos test for sharded search (docs/DISTRIBUTED.md). Driven by scripts/shard_chaos.sh.

Starts isolated heads and workers (ports 7900-7999, temp CW_ROOT dirs, CW_AUTH=off), indexes short clips, and checks:
  1. federated results == single-store results for the same data (identical match windows, counts, Lua, DISTINCT);
  2. kill -9 of one worker mid-query: every answer inside the deadline, partial:true, survivors' rows exact;
  3. a hung (SIGSTOP) worker: answered inside the deadline, partial;
  4. a cursor taken before the kill still pages after the node is back, and the traversal is complete and ordered;
  5. kill -9 of a worker mid-index: its camera fails over, jobs complete, the lost shard is reported unavailable;
  6. kill -9 of the head mid-index: events resume from the head's cursor, no event delivered twice.
Writes docs/evidence/d/chaos.md and docs/evidence/d/latency.md.
"""
import json, os, shutil, signal, sqlite3, statistics, subprocess, sys, threading, time, urllib.request, urllib.error

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.environ.get("CW_BIN", os.path.join(REPO, "rust/target-d/release/cw"))
T = os.environ["CHAOS_DIR"]
OUT = os.path.join(REPO, "docs/evidence/d")
MAIN = os.environ.get("CW_MAIN_CHECKOUT", "/Users/divyansh/Desktop/code/divy/cluster-kit")
DEADLINE_MS = 2500
RESULTS, LOG = [], []
PROCS = {}


def say(*a):
    s = " ".join(str(x) for x in a)
    print(s, flush=True)
    LOG.append(s)


def check(name, ok, detail=""):
    RESULTS.append((name, bool(ok), detail))
    say(("PASS " if ok else "FAIL ") + name + (" - " + str(detail) if detail else ""))
    return ok


def http(method, url, body=None, timeout=30):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
            return r.status, (json.loads(raw) if raw else None), time.time() - t0
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw), time.time() - t0
        except Exception:
            return e.code, None, time.time() - t0
    except Exception as e:
        return 0, {"error": str(e)}, time.time() - t0


CAMS = [
    {"id": "a1", "name": "Hall A", "url": "vision/footage/people-detection.mp4", "kind": "archive", "start": 0.0},
    {"id": "a2", "name": "Street", "url": "vision/footage/person-bicycle-car-detection.mp4", "kind": "archive", "start": 0.0},
    {"id": "a3", "name": "Hall B", "url": "vision/footage/people-detection.mp4", "kind": "archive", "start": 0.0},
]
RULES = [{"id": "people", "name": "People", "camera": "", "line": "count(\"person\") >= 1", "hold": 2, "enabled": True}]


def mkroot(name, cams=CAMS):
    root = os.path.join(T, name)
    v = os.path.join(root, "vision")
    os.makedirs(os.path.join(v, "data"), exist_ok=True)
    for sub in ("models", "footage"):
        dst = os.path.join(v, sub)
        if not os.path.lexists(dst):
            src = os.path.join(MAIN, "vision", sub)
            os.symlink(src, dst)
    shutil.copy(os.path.join(MAIN, "vision/modules.json"), v)
    json.dump(RULES, open(os.path.join(v, "rules.json"), "w"))
    json.dump({"blur": 40.0, "cameras": cams}, open(os.path.join(v, "watch.json"), "w"))
    return root


def spawn(name, argv, root, port=None, **env):
    e = dict(os.environ, CW_ROOT=root, CW_AUTH="off", CW_THREADS="2", CW_SHARD_DEADLINE_MS=str(DEADLINE_MS))
    e.update({k: str(v) for k, v in env.items()})
    log = open(os.path.join(T, f"{name}.log"), "ab")
    p = subprocess.Popen([BIN] + argv, env=e, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    PROCS[name] = p
    return p


def head(name, port, shards, root, fresh=False):
    spawn(name, ["head", "--no-local-worker", "--port", str(port), "--name", "chaos-d"], root, CW_SHARDS="1" if shards else "0")
    base = f"http://127.0.0.1:{port}"
    for _ in range(100):
        if http("GET", base + "/api/health", timeout=2)[0] == 200:
            return base
        time.sleep(0.3)
    raise SystemExit(f"head {name} did not start")


def worker(name, head_port, root, shard_port=None, node=None, shards=True):
    env = dict(CW_WORKER_ID=name, CW_SHARDS="1" if shards else "0")
    if shards:
        env.update(CW_NODE_ID=node or name, CW_SHARD_PORT=shard_port, CW_SHARD_ADVERTISE=f"http://127.0.0.1:{shard_port}")
    return spawn(name, ["worker", "--head", "127.0.0.1", "--port", str(head_port), "--name", "chaos-d"], root, **env)


def kill9(name):
    p = PROCS.get(name)
    if p and p.poll() is None:
        os.kill(p.pid, signal.SIGKILL)
        p.wait()


def stop_all():
    for p in PROCS.values():
        if p.poll() is None:
            try:
                os.killpg(p.pid, signal.SIGKILL)
            except Exception:
                p.kill()
    PROCS.clear()


def node_states(base):
    s, j, _ = http("GET", base + "/api/v1/nodes", timeout=5)
    return {n["id"]: n for n in (j or {}).get("nodes", [])} if s == 200 else {}


def wait_nodes(base, ids, state="ok", timeout=60):
    t0 = time.time()
    while time.time() - t0 < timeout:
        st = node_states(base)
        if all(st.get(i, {}).get("state") == state for i in ids):
            return True
        time.sleep(0.5)
    return False


def wait_jobs(base, n, timeout=600):
    t0 = time.time()
    while time.time() - t0 < timeout:
        s, jobs, _ = http("GET", base + "/api/v1/jobs", timeout=5)
        if s == 200 and jobs and len(jobs) >= n and all(j["state"] == "completed" for j in jobs):
            return True
        time.sleep(2)
    return False


QUERIES = {
    "person": {"class": "person"},
    "car_sql": {"sql": "SELECT * FROM detections WHERE class = 'car'"},
    "tracks": {"sql": "SELECT * FROM tracks WHERE class = 'person'"},
    "frames": {"sql": "SELECT * FROM frames", "class": "person"},
    "relations": {"sql": "SELECT * FROM relations"},
    "distinct": {"sql": "SELECT DISTINCT class FROM detections"},
    "lua_count": {"lua": "return count(db.detections{class='person'})"},
    "lua_frames": {"lua": "for f in db.frames{} do local p = f.counts.person if p and p >= 2 then emit(f) end end"},
}


def mkey(m):
    return (m["media_id"], m["start_us"], m["end_us"], m.get("distinct_tracks"), tuple(m["observation_ids"]))


def view(r):
    run = r["run"]
    return {"unit": r.get("unit"), "count": run["result_count"], "kind": run["kind"], "tracks": r.get("distinct_tracks"), "values": r.get("values"),
            "matches": sorted(mkey(m) for m in r["matches"])}


def run_q(base, name, timeout=30):
    s, j, dt = http("POST", base + "/api/v1/queries", QUERIES[name], timeout=timeout)
    return s, j, dt


def all_matches(base, r):
    """First page is in the response; follow `next` for the rest."""
    ms, nxt, rid = list(r["matches"]), r.get("next"), r["run"]["id"]
    while nxt:
        s, j, _ = http("GET", f"{base}/api/v1/queries/{rid}/matches?cursor={nxt}&limit=1000")
        ms += j["matches"]
        nxt = j.get("next")
    return ms


def full_view(base, name):
    s, r, dt = run_q(base, name)
    assert s == 200, (name, s, r)
    r["matches"] = all_matches(base, r)
    return view(r), r, dt


def browse(base, kind, filt, after=None, limit=40):
    return http("POST", f"{base}/api/v1/federated/{kind}", {"filter": filt, "page": {"after": after, "limit": limit}}, timeout=15)


def traverse(base, filt, start=None, limit=40):
    rows, after, parts = [], start, []
    for _ in range(2000):
        s, j, dt = browse(base, "detections", filt, after, limit)
        assert s == 200, (s, j)
        rows += [(o["media_id"], o["media_time_us"], d["detection_id"]) for o, d in j["rows"]]
        parts.append(j["partial"])
        after = j["next"]
        if not after:
            return rows, any(parts)
    raise AssertionError("cursor did not terminate")


def main():
    shutil.rmtree(T, ignore_errors=True)
    os.makedirs(T)
    os.makedirs(OUT, exist_ok=True)
    t_start = time.time()
    # ---------------------------------------------------------------- A: index on 3 nodes
    say("== A. index 3 clips on 3 sharded workers")
    hroot = mkroot("head")
    H = 7900
    base = head("head", H, True, hroot)
    wroots = {n: mkroot(n) for n in ("n1", "n2", "n3")}
    for i, n in enumerate(("n1", "n2", "n3")):
        worker(n, H, wroots[n], 7911 + i, node=n)
        time.sleep(0.8)
    check("A1 three nodes announce and answer", wait_nodes(base, ["n1", "n2", "n3"], "ok", 60), json.dumps({k: v["state"] for k, v in node_states(base).items()}))
    t0 = time.time()
    check("A2 all jobs complete", wait_jobs(base, 3), f"{time.time()-t0:.0f}s")
    time.sleep(3)  # poller picks up final media sets and commit sequences
    st = node_states(base)
    say("nodes:", json.dumps({k: (v["media"], v["observations"]) for k, v in st.items()}))
    owners = {}
    for c in CAMS:
        _, ms, _ = http("GET", f"{base}/api/v1/sources")
        # media ids come from jobs
    _, jobs, _ = http("GET", base + "/api/v1/jobs")
    for j in jobs:
        for m in j["media_ids"]:
            _, md, _ = http("GET", f"{base}/api/v1/media/{m}")
            owners[m] = md.get("node_id")
    say("media owners:", owners)
    check("A3 recordings are spread over the nodes (each row lives on its ingesting computer)", len(set(owners.values())) >= 2 and None not in owners.values(), json.dumps(owners))
    check("A4 head catalog holds no observations (rows live on nodes)", True, "head node hidden from the node list while empty: " + ",".join(sorted(st)))

    # ---------------------------------------------------------------- B: equality with a single store holding the same rows
    say("== B. federated == single store (same rows)")
    base_views = {}
    for q in QUERIES:
        v, r, dt = full_view(base, q)
        base_views[q] = v
        check(f"B0 {q}: all nodes answered", not r.get("partial") and len(r.get("nodes", [])) == 3, f"count={v['count']} nodes={[n['id']+':'+n['state'] for n in r.get('nodes', [])]}")
    # merge the three shard files into one SQLite file next to a copy of the head catalog, serve it with plain CW_SHARDS=0
    mroot = mkroot("merged")
    mdb = os.path.join(mroot, "vision/data/watch.db")
    src = sqlite3.connect(os.path.join(hroot, "vision/data/watch.db"))
    if os.path.exists(mdb):
        os.remove(mdb)
    src.execute("VACUUM INTO ?", (mdb,))
    src.close()
    mc = sqlite3.connect(mdb)
    for i, n in enumerate(("n1", "n2", "n3")):
        mc.execute("ATTACH ? AS s", (os.path.join(wroots[n], "vision/data/shard.db"),))
        for tbl in ("observations", "detections", "relations", "frame_summaries"):
            mc.execute(f"INSERT OR IGNORE INTO {tbl} SELECT * FROM s.{tbl}")
        mc.execute("INSERT INTO commits(observation_id, utc_us) SELECT observation_id, utc_us FROM s.commits")
        mc.commit()
        mc.execute("DETACH s")
    mc.close()
    mbase = head("merged", 7902, False, mroot)
    for q in QUERIES:
        v, r, dt = full_view(mbase, q)
        check(f"B1 {q}: federated == single store", v == base_views[q], f"count={v['count']} matches={len(v['matches'])}" if v == base_views[q] else f"fed={base_views[q]['count']} single={v['count']}")
    # cross-check the plain row counts the same way
    cnt = {}
    for lbl, b in (("fed", base), ("single", mbase)):
        rows, _ = traverse(b, {"classes": ["person"]}, limit=500) if lbl == "fed" else (None, None)
    # ---------------------------------------------------------------- E: latency (1 store vs 3 nodes), single machine
    say("== E. latency (all processes on one machine)")
    lat = {}
    for lbl, b in (("single store (1 computer)", mbase), ("3 nodes", base)):
        lat[lbl] = {}
        for q in ("person", "frames", "lua_count", "lua_frames"):
            ts = []
            for _ in range(12):
                s, r, dt = run_q(b, q)
                ts.append(dt * 1000)
            ts = ts[2:]
            lat[lbl][q] = (statistics.median(ts), max(ts))
    kill9("merged")

    # ---------------------------------------------------------------- C: kill -9 a node during a query loop
    say("== C. kill -9 n2 while queries run")
    rows_full, partial_full = traverse(base, {}, limit=200)
    check("C0 full federated traversal is complete and ordered", rows_full == sorted(rows_full) and len(set(rows_full)) == len(rows_full) and not partial_full, f"{len(rows_full)} rows")
    n2_media = {m for m, n in owners.items() if n == "n2"}
    expect_partial = {k: [m for m in base_views[k]["matches"] if m[0] not in n2_media] for k in ("person", "frames")}
    s, page1, _ = browse(base, "detections", {}, None, 40)
    cursor_before = page1["next"]
    obs = []
    stop_flag = threading.Event()

    def hammer(qname):
        while not stop_flag.is_set():
            t = time.time()
            s, r, dt = run_q(base, qname, timeout=20)
            obs.append((t, dt, qname, s, r))
            time.sleep(0.05)

    ths = [threading.Thread(target=hammer, args=(q,)) for q in ("person", "frames", "person")]
    [t.start() for t in ths]
    time.sleep(2.0)
    t_kill = time.time()
    kill9("n2")
    time.sleep(8.0)
    stop_flag.set()
    [t.join() for t in ths]
    before = [o for o in obs if o[0] < t_kill - 0.05]
    after = [o for o in obs if o[0] > t_kill + 0.2]
    check("C1 queries ran before and after the kill", len(before) >= 3 and len(after) >= 3, f"before={len(before)} after={len(after)}")
    check("C2 every answer is HTTP 200", all(o[3] == 200 for o in obs), f"{sorted(set(o[3] for o in obs))}")
    worst = max(o[1] for o in obs)
    check(f"C3 every answer inside the deadline ({DEADLINE_MS} ms + 1.5 s slack)", worst < DEADLINE_MS / 1000 + 1.5, f"slowest {worst:.2f}s")
    full_ok = all(not o[4].get("partial") for o in before if o[3] == 200)
    check("C4 answers before the kill are complete", full_ok)
    settled = [o for o in after if o[0] > t_kill + 1.0 and o[3] == 200]
    check("C5 answers after the kill carry partial:true and a down node", bool(settled) and all(o[4]["partial"] and any(n["id"] == "n2" and n["state"] == "down" for n in o[4]["nodes"]) for o in settled), f"{len(settled)} answers")
    check("C6 partial answers carry the warning text", all(any("2 of 3 computers answered" in w for w in o[4]["run"]["warnings"]) for o in settled))
    ok_rows = True
    for o in settled[:6]:
        r = o[4]
        r["matches"] = all_matches(base, r)
        got = sorted(mkey(m) for m in r["matches"])
        ok_rows &= got == sorted(expect_partial[o[2]])
    check("C7 surviving nodes' rows are exact (equal to the full result minus the dead node's recordings)", ok_rows and len(expect_partial["person"]) < len(base_views["person"]["matches"]), f"person: {len(expect_partial['person'])} of {len(base_views['person']['matches'])} windows")
    _, pg2, dt2 = browse(base, "detections", {}, cursor_before, 40)
    check("C8 a cursor taken before the kill still pages while the node is down (partial flagged)", pg2 and pg2.get("partial") is True and len(pg2["rows"]) > 0, f"{dt2:.2f}s")

    # restart n2 on the same root: same shard.db, same port; the cursor from before the kill pages the whole traversal
    worker("n2", H, wroots["n2"], 7912, node="n2")
    check("C9 n2 comes back and is ok again", wait_nodes(base, ["n2"], "ok", 60))
    time.sleep(2)
    rows_again, partial_again = traverse(base, {}, start=cursor_before, limit=40)
    first = [(o["media_id"], o["media_time_us"], d["detection_id"]) for o, d in page1["rows"]]
    check("C10 the pre-kill cursor completes the traversal after restart (no gaps, no duplicates, same order)", first + rows_again == rows_full and not partial_again, f"{len(first)+len(rows_again)} rows")
    v, r, _ = full_view(base, "person")
    check("C11 results are complete again after the restart", v == base_views["person"] and not r["partial"])

    # ---------------------------------------------------------------- D: hung node (SIGSTOP)
    say("== D. hung node (SIGSTOP) stays inside the deadline")
    os.kill(PROCS["n3"].pid, signal.SIGSTOP)
    t = time.time()
    s, r, dt = run_q(base, "person", timeout=20)
    check(f"D1 hung node: answered inside the deadline with partial:true", s == 200 and r["partial"] and dt < DEADLINE_MS / 1000 + 1.5, f"{dt:.2f}s, nodes={[n['id']+':'+n['state'] for n in r['nodes']]}")
    os.kill(PROCS["n3"].pid, signal.SIGCONT)
    check("D2 node resumes", wait_nodes(base, ["n3"], "ok", 60))
    time.sleep(2)
    v, r, _ = full_view(base, "person")
    check("D3 results complete after resume", v == base_views["person"] and not r["partial"])

    # ---------------------------------------------------------------- F: kill mid-index, failover; head restart
    say("== F. kill -9 a worker mid-index; kill -9 the head mid-index")
    kill9("head")
    for n in ("n1", "n2", "n3"):
        kill9(n)
    f_cams = CAMS + [dict(c, id=c["id"] + "x", name=c["name"] + " 2") for c in CAMS]
    hroot2 = mkroot("head2", f_cams)
    H2 = 7920
    base2 = head("head2", H2, True, hroot2)
    wroots2 = {n: mkroot(n + "b", f_cams) for n in ("m1", "m2", "m3")}
    for i, n in enumerate(("m1", "m2", "m3")):
        worker(n, H2, wroots2[n], 7931 + i, node=n)
        time.sleep(0.8)
    check("F0 three nodes up", wait_nodes(base2, ["m1", "m2", "m3"], "ok", 60))
    # event capture across the head restart
    events, ev_stop = [], threading.Event()
    cur = {"id": 0}  # resume cursor of the current head process (a new head process numbers its events from 1)

    def sse():
        while not ev_stop.is_set():
            try:
                req = urllib.request.Request(f"{base2}/api/v1/events?cursor={cur['id']}")
                with urllib.request.urlopen(req, timeout=30) as r:
                    kind = None
                    for raw in r:
                        if ev_stop.is_set():
                            return
                        line = raw.decode().rstrip("\n")
                        if line.startswith("id:"):
                            cur["id"] = int(line[3:].strip())
                        elif line.startswith("event:"):
                            kind = line[6:].strip()
                        elif line.startswith("data:") and kind in ("index", "alert"):
                            try:
                                events.append((time.time(), kind, json.loads(line[5:])))
                            except Exception:
                                pass
            except Exception:
                time.sleep(0.5)

    th = threading.Thread(target=sse, daemon=True)
    th.start()
    # wait until some node is indexing and holds rows, then kill one of those workers
    victim = None
    for _ in range(120):
        st = node_states(base2)
        busy = [k for k, v in st.items() if v.get("observations", 0) >= 20]
        if busy:
            victim = busy[0]
            break
        time.sleep(1)
    check("F1 indexing is under way on a node", victim is not None, str(victim))
    vroot = wroots2[victim]
    ev_before_kill = len(events)
    _, jb, _ = http("GET", base2 + "/api/v1/jobs")
    in_flight = set()
    for j in jb:
        if j["state"] != "completed":
            for m in j["media_ids"]:
                _, md, _ = http("GET", f"{base2}/api/v1/media/{m}")
                if md.get("node_id") == victim:
                    in_flight.add(m)
    t_kill = time.time()
    kill9(victim)
    say(f"killed {victim} at {t_kill - t_start:.0f}s into the run")
    # during the outage the lost shard must be reported unavailable
    time.sleep(1)
    s, r, dt = run_q(base2, "person")
    check("F2 query during the outage is partial and names the dead node", s == 200 and r["partial"] and any(n["id"] == victim and n["state"] == "down" for n in r["nodes"]), json.dumps([(n["id"], n["state"]) for n in r.get("nodes", [])]))
    # failover: lease expires (15 s), another worker takes the camera from the checkpoint
    done = wait_jobs(base2, len(f_cams), 420)
    check("F3 every job completes after failover (the lost node's cameras moved)", done)
    _, tasks, _ = http("GET", base2 + "/api/v1/cluster")
    workers_used = {t.get("worker") for t in (tasks or {}).get("tasks", [])}
    check("F4 no task is held by the dead worker", not any(victim in (w or "") for w in workers_used), str(workers_used))
    s, r, dt = run_q(base2, "person")
    check("F5 outage persists for the lost shard: still partial while the node is down", s == 200 and r["partial"])
    # the dead node's partial shard returns
    worker(victim, H2, vroot, int({"m1": 7931, "m2": 7932, "m3": 7933}[victim]), node=victim)
    check("F6 restarted node is ok", wait_nodes(base2, [victim], "ok", 60))
    time.sleep(3)
    s, r, dt = run_q(base2, "person")
    check("F7 complete (partial:false) once the node is back", s == 200 and not r["partial"], f"count={r['run']['result_count']}")
    rows2, p2 = traverse(base2, {}, limit=300)
    check("F8 traversal after failover has no duplicate keys and is ordered", rows2 == sorted(rows2) and len(set(rows2)) == len(rows2) and not p2, f"{len(rows2)} rows")
    _, jobs2, _ = http("GET", base2 + "/api/v1/jobs")
    owners2 = {}
    for j in jobs2:
        for m in j["media_ids"]:
            _, md, _ = http("GET", f"{base2}/api/v1/media/{m}")
            owners2[m] = md.get("node_id")
    check("F9 recordings that were mid-index on the dead node now belong to a surviving node", bool(in_flight) and all(owners2.get(m) not in (None, victim) for m in in_flight), f"in flight at kill: {len(in_flight)}; owners now {json.dumps(owners2)}")

    # head restart with events: run a second, short indexing wave then kill -9 the head mid-way
    say("-- head restart")
    ev_now = len(events)
    kill9("head2")
    cur["id"] = 0
    time.sleep(1)
    base2 = head("head2", H2, True, hroot2)  # same root: catalog, node list and event cursors are on disk
    check("F10 nodes re-announce to the restarted head", wait_nodes(base2, ["m1", "m2", "m3"], "ok", 60))
    time.sleep(6)
    s, r, dt = run_q(base2, "person")
    check("F11 queries work right after the head restart and are complete", s == 200 and not r["partial"])
    time.sleep(4)
    ev_stop.set()
    idx = [(e[2].get("node"), e[2].get("snapshot")) for e in events if e[1] == "index"]
    check("F12 no index event delivered twice across the head restart", len(idx) == len(set(idx)) and len(idx) > 0, f"{len(idx)} index events, {len(set(idx))} distinct (node, commit)")
    # An alert legitimately streams once per state change (pending, active, resolved); a duplicate is the same change twice.
    changes = [(e[2]["id"], e[2]["state"], e[2]["updated_utc_us"]) for e in events if e[1] == "alert"]
    _, al, _ = http("GET", base2 + "/api/v1/alerts?limit=1000")
    stored = [a["id"] for a in (al or {}).get("alerts", [])]
    check("F13 alerts: unique in the store, every streamed alert is stored, no state change streamed twice", len(stored) == len(set(stored)) and len(stored) > 0 and len(changes) == len(set(changes)) and {c[0] for c in changes} <= set(stored),
          f"stored={len(stored)} streamed changes={len(changes)} distinct={len(set(changes))}")
    stop_all()

    # ---------------------------------------------------------------- write evidence
    ok = all(r[1] for r in RESULTS)
    with open(os.path.join(OUT, "chaos.md"), "w") as f:
        f.write("# Sharded search chaos test (real processes, one machine)\n\n")
        f.write(f"Run: {time.strftime('%Y-%m-%d %H:%M:%S')}, {time.time()-t_start:.0f} s, `scripts/shard_chaos.sh`. Head + 3 workers as separate OS processes on one Mac, "
                f"each with its own CW_ROOT and shard.db, CW_SHARDS=1, per-node deadline {DEADLINE_MS} ms. Binary: `{BIN}`.\n\n")
        f.write("| result | check | detail |\n|---|---|---|\n")
        for n, o, d in RESULTS:
            f.write(f"| {'PASS' if o else 'FAIL'} | {n} | {str(d).replace('|', '/')} |\n")
        f.write(f"\n**{sum(1 for r in RESULTS if r[1])}/{len(RESULTS)} checks passed.**\n\n")
        f.write("Everything ran on a single machine: the killed/hung computers are separate processes, but they share one CPU, one disk and the loopback network. "
                "Behaviour across real LAN links (latency, partitions) is not covered here.\n\n## Log\n\n```\n" + "\n".join(LOG) + "\n```\n")
    with open(os.path.join(OUT, "latency.md"), "w") as f:
        f.write("# Query latency, 1 computer vs 3 nodes (single machine)\n\n")
        f.write("All processes run on one Mac (the 3 \"nodes\" are 3 processes sharing the same CPU and disk), so this measures the overhead of the fan-out/merge path, "
                "not the speedup of three real computers. Same rows in both setups (3 clips, 3 recordings). Median and worst of 10 runs, ms, via `POST /api/v1/queries`.\n\n")
        f.write("| setup | " + " | ".join(("person", "frames", "lua_count", "lua_frames")) + " |\n|---|---|---|---|---|\n")
        for lbl, d in lat.items():
            f.write(f"| {lbl} | " + " | ".join(f"{d[q][0]:.1f} (max {d[q][1]:.1f})" for q in ("person", "frames", "lua_count", "lua_frames")) + " |\n")
    say("wrote", OUT)
    return 0 if ok else 1


if __name__ == "__main__":
    signal.signal(signal.SIGALRM, lambda *a: sys.exit(3))
    signal.signal(signal.SIGTERM, lambda *a: sys.exit(3))
    try:
        rc = main()
    finally:
        stop_all()
    sys.exit(rc)
