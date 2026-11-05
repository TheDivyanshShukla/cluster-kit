"""python3 -m unittest discover -s python/tests   (offline; the head is faked, the pushed runner really runs)"""
from __future__ import annotations
from collections.abc import Iterator
from typing import Any
import io, json, os, subprocess, sys, tempfile, threading, time, unittest
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, ".."))
import clusterkit as ck
from clusterkit import _runner
import sdkfns, advfns
from clusterkit import _hist, _advise


class FakeHead:
    """Stands in for _http: runs the pushed script for real, two pieces per job (like two threads), joins in order."""
    def __init__(self, workers=None) -> None:
        self.jobs, self.posted, self.hang, self.cancelled, self.polls = {}, [], False, [], 0
        self.workers = workers if workers is not None else [{"threads": 4, "mem_total": 8 << 30, "mem_used": 1 << 30, "node_id": f"n{i}"} for i in range(4)]

    def __call__(self, method, path, body=None, text=False, timeout=60, raw=False, length=None) -> Any:
        if path.startswith(("/api/v1/blobs", "/api/v1/net", "/api/v1/estimate")):  # an older head: no blob store
            raise ck.ClusterError(f"{method} {path}: HTTP 404 not found")
        if path == "/api/v1/cluster":
            return {"workers": self.workers, "jobs": []}
        if path == "/api/v1/nodes":
            return {"nodes": []}
        if method == "POST" and path == "/api/v1/jobs":
            self.posted.append(body)
            assert body["cmd"].startswith("\x01PUSH\npython3\n")
            script = body["cmd"].split("\n", 3)[3]
            with tempfile.NamedTemporaryFile("w", suffix=".py", delete=False) as f:
                f.write(script)
            items, h = body["items"], len(body["items"]) // 2
            out = ""
            for piece in (items[:h], items[h:]):
                r = subprocess.run([sys.executable, f.name], input="".join(i + "\n" for i in piece), capture_output=True, text=True)
                assert r.returncode == 0, r.stderr
                out += r.stdout
            os.unlink(f.name)
            jid = len(self.jobs) + 1
            self.jobs[jid] = out
            return {"job": {"id": jid}}
        if path.endswith("/result"):
            return self.jobs[int(path.split("/")[4])]
        if method == "POST" and path.endswith("/cancel"):
            self.cancelled.append(path)
            return {"job": {"state": "cancelled"}}
        self.polls += 1
        n = len(self.posted[-1]["items"]) if self.posted else 1
        if self.hang:
            return {"job": {"state": "running", "done": 1, "total": n, "created_ms": 1000, "started_ms": 1010}}
        return {"job": {"state": "done", "done": n, "total": n, "created_ms": 1000, "started_ms": 1010, "finished_ms": 1510}}


class T(unittest.TestCase):
    def setUp(self):
        self.head = FakeHead()
        ck._http = self.head
        self.home = tempfile.TemporaryDirectory()
        os.environ["CK_SDK_HOME"] = self.home.name  # never touch the real ~/.clusterkit
        ck.config(push=False, replan_min_s=2.0)
        ck._cmds.clear()

    def tearDown(self):
        self.home.cleanup()
        ck.config(push=True)

    def history(self):
        if not os.path.exists(_hist.path()):
            return []
        with open(_hist.path()) as f:
            return [json.loads(l) for l in f]

    def test_codec_roundtrip_small_and_compressible(self):
        for v in [1, "x", {"a": [1.5, None]}, "z" * 5000, list(range(100))]:
            self.assertEqual(_runner.dec(_runner.enc(v)), v)

    def test_map_order_and_reduce(self):
        items = list(range(40))
        self.assertEqual(ck.map(sdkfns.inc, items, local="never"), [i + 1 for i in items])
        self.assertEqual(ck.map(sdkfns.inc, items, local="never", reduce=sum), sum(i + 1 for i in items))
        b = self.head.posted[0]
        self.assertEqual((b["kind"], b["max_pcs"] >= 1, b["mem_mb"] >= 1), ("list", True, True))

    def test_remote_error_has_traceback(self):
        with self.assertRaises(ck.RemoteError) as c:
            ck.map(sdkfns.boom, range(6), local="never")
        self.assertIn("bad item 3", c.exception.remote_traceback)
        self.assertIn("ValueError", str(c.exception))

    def test_sweep_and_train_modes(self):
        r = ck.sweep(sdkfns.kw, {"a": [1, 2], "b": [3, 4]}, key=lambda v: v, top=1, local="never")
        self.assertEqual(r[0], {"params": {"a": 2, "b": 4}, "result": 24})
        p = ck.train(sdkfns.two, [0.0, 5.0], [1, 2, 3], rounds=2, local="never")  # each round adds mean(1,2,3)=2
        self.assertEqual(p, [4.0, 5.0])

    def test_mean_params(self):
        self.assertEqual(ck.mean_params([{"w": [1.0, 2.0], "b": 1.0}, {"w": [3.0, 4.0], "b": 3.0}]), {"w": [2.0, 3.0], "b": 2.0})

    def test_plan_local_for_tiny_job(self):
        pl = ck.plan(sdkfns.inc, range(5000))
        self.assertEqual(pl.where, "local")

    def test_plan_cluster_for_heavy_job_and_pcs_and_mem(self):
        # 8 PCs x 16 threads: clearly faster than any dev laptop, so the decision does not hinge on machine load
        self.head.workers = [{"threads": 16, "mem_total": 8 << 30, "mem_used": 1 << 30, "node_id": f"n{i}"} for i in range(8)]
        ck.config(target_wall_s=1.0)
        try:
            pl = ck.plan(sdkfns.slow, range(40000))
        finally:
            ck.config(target_wall_s=10.0)
        self.assertEqual(pl.where, "cluster")
        self.assertTrue(1 <= pl.pcs <= 8)
        self.assertEqual(pl.placement, "fastest")  # est under 60 s
        self.assertGreaterEqual(pl.mem_mb, 1)
        self.assertIn("cluster", str(pl))

    def test_plan_mem_excludes_small_pcs(self):
        self.head.workers = [{"threads": 4, "mem_total": 1 << 30, "mem_used": 1 << 30 - 1, "node_id": "n"}]
        pl = ck.plan(sdkfns.slow, range(2000), mem_mb=5000)
        self.assertEqual(pl.where, "local")
        self.assertIn("room", str(pl))

    def test_oversize_item_message(self):
        with self.assertRaises(ck.ClusterError) as c:
            ck.map(sdkfns.ident, [os.urandom(6000)] * 2, local="never")
        self.assertIn("max_item_bytes", str(c.exception))

    def test_push_limit_message(self):
        ck.config(push_max_kb=1)
        try:
            with self.assertRaises(ck.ClusterError) as c:
                ck.map(sdkfns.inc, [1, 2], local="never")
        finally:
            ck.config(push_max_kb=64)
        self.assertIn("push_max_kb", str(c.exception))

    def test_main_module_not_rerun(self):
        payload = {"mods": [["__ck_user__", "def f(x): return x*2\nif __name__ == '__main__':\n    raise SystemExit('ran')\n"]],
                   "main": "__ck_user__", "attr": "f", "mode": "call"}
        out = io.StringIO()
        saved = sys.modules.get("__main__")
        try:
            _runner.run(payload, [_runner.enc(21)], out)
        finally:
            sys.modules["__main__"] = saved
            sys.modules.pop("__ck_user__", None)
        self.assertEqual(_runner.dec(out.getvalue().strip()), ("ok", 42))

    def test_lambda_runs_locally(self):
        self.assertEqual(ck.map(lambda x: x + 1, [1, 2]), [2, 3])

    # ---- self-optimizing runtime
    def test_batching_keeps_order_and_results(self):
        items = list(range(300))
        self.assertEqual(ck.map(sdkfns.inc, items, local="never"), [i + 1 for i in items])
        b = self.head.posted[0]
        self.assertLess(len(b["items"]), 300)  # grouped into fewer remote calls
        self.assertTrue(_runner.dec(b["items"][0]) == items[:len(_runner.dec(b["items"][0]))])  # each line is a contiguous run
        self.assertEqual(ck.map(sdkfns.inc, items, local="never", batch=False), [i + 1 for i in items])
        self.assertEqual(len(self.head.posted[1]["items"]), 300)

    def test_batching_error_names_the_item_and_big_items_split(self):
        with self.assertRaises(ck.RemoteError) as c:
            ck.map(sdkfns.boom, range(60), local="never")
        self.assertIn("bad item 3", c.exception.remote_traceback)
        big = [os.urandom(1500) for _ in range(12)]  # a batch of these would exceed max_item_bytes: it must split
        self.assertEqual(ck.map(sdkfns.ident, big, local="never"), big)

    def test_code_is_built_once_per_session(self):
        ck.map(sdkfns.inc, range(40), local="never")
        ck.map(sdkfns.inc, range(40), local="never")
        self.assertEqual(self.head.posted[0]["cmd"], self.head.posted[1]["cmd"])
        self.assertEqual(len(ck._cmds), 1)

    def test_replan_falls_back_locally_when_cluster_loses(self):
        self.head.hang = True
        ck.config(replan_min_s=0.0)
        items = list(range(100))
        pl = ck.Plan(where="cluster", pcs=1, batch=1, local_s=0.01, cluster_s=0.5, mem_mb=1, placement="fastest", priority="normal",
                     n=100, item_s=1e-4, local_procs=1, slots=2, all_slots=2, reasons=[], learned=False)
        out = ck._run(sdkfns.inc, "call", items, ck._opts({}), pl)
        self.assertEqual(out, [i + 1 for i in items])
        self.assertEqual(len(self.head.cancelled), 1)
        rec = self.history()[-1]
        self.assertEqual((rec["where"], rec["failed"]), ("replanned", True))

    def test_no_replan_when_local_is_forbidden(self):
        self.head.hang, _polls = True, []
        ck.config(replan_min_s=0.0)
        with self.assertRaises(ck.ClusterError):
            ck.map(sdkfns.inc, range(40), local="never", timeout_s=0.3)
        self.assertEqual(len(self.head.cancelled), 1)  # cancelled by the timeout, not by a re-plan

    def test_results_pushed_by_stream_not_polled(self):
        ev = [b"event: job\n", b'data: {"id": 1, "state": "done", "finished_ms": 5}\n', b"\n"]
        self.head.hang = True
        saved = ck._stream_open
        stop = threading.Event()

        def fake_stream(head, token) -> Iterator[bytes]:  # like the real one: delivers the events, then stays open
            yield from ev
            stop.wait(5)
        ck._stream_open = fake_stream
        ck.config(push=True)
        ck._stream.wins = [True, True, True]  # pushes have been beating polls
        try:
            ck._stream.ensure()
            t0 = time.time()
            row = ck._await(1, None)
            self.assertEqual(row["state"], "done")
            self.assertLess(time.time() - t0, 1.5)
            self.assertLessEqual(self.head.polls, 1)  # at most the start-up poll
        finally:
            ck._stream.gen += 1
            stop.set()
            ck._stream_open = saved
            ck._stream.wins, ck._stream.rows, ck._stream.live = [], {}, False

    def test_stream_not_trusted_after_late_events(self):
        s = ck._Stream()
        s.arrived[9] = 100.0
        s.judge = (9, 98.0)  # poll saw the job 2 s before the event came
        self.assertFalse(s.trusted())
        self.assertEqual(s.wins, [False])

    # ---- learned model
    def test_history_fit_uses_medians_and_plan_says_so(self):
        head = ck._cfg["head"]
        for fx, sp in [(0.2, 0.8), (0.3, 0.9), (0.25, 1.0), (9.0, 0.1), (0.22, 0.85)]:  # one wild outlier run
            _hist.record({"head": head, "where": "cluster", "fixed_s": fx, "item_c": 1e-5, "speed": sp, "run_s": 3.0})
        _hist.record({"head": "http://other", "where": "cluster", "fixed_s": 50.0, "item_c": 1.0, "speed": 9, "run_s": 3.0})
        m = _hist.fit(head)
        self.assertEqual(m["runs"], 5)
        self.assertAlmostEqual(m["fixed_s"], 0.25)
        self.assertAlmostEqual(m["speed"], 0.85)
        pl = ck.plan(sdkfns.slow, range(2000))
        self.assertTrue(pl.learned)
        self.assertIn("learned from 5 earlier runs", str(pl))
        self.assertIsNone(_hist.fit("http://nobody"))

    def test_run_is_recorded_with_predicted_and_actual(self):
        ck.map(sdkfns.slow, range(40), local="never")
        r = self.history()[-1]
        self.assertEqual((r["where"], r["n"], r["head"]), ("cluster", 40, ck._cfg["head"]))
        for k in ("pred_s", "actual_s", "item_s", "fixed_s", "item_c", "run_s", "pcs", "slots"):
            self.assertIn(k, r)
        self.assertAlmostEqual(r["run_s"], 0.5)  # finished_ms - started_ms of the fake job row

    # ---- advisor: one fixture per rule
    def ids(self, fn, items, **kw: Any) -> list[str]:
        return ck.advise(fn, items, sample=kw.pop("sample", 8), **kw).ids()

    def test_advise_tiny_items_and_cannot_help(self):
        ids = self.ids(advfns.tiny, range(2000))
        self.assertIn("tiny_items", ids)
        self.assertIn("cluster_cannot_help", ids)
        s = [x for x in ck.advise(advfns.tiny, range(2000)) if x.id == "cluster_cannot_help"][0]
        self.assertIn("need_total_s", s.evidence)
        self.assertIn("break-even", s.why.lower())

    def test_advise_big_args_and_results(self):
        self.assertIn("big_args", self.ids(sdkfns.ident, [os.urandom(6000) for _ in range(6)]))
        self.assertIn("big_results", self.ids(advfns.blob, range(6)))

    def test_advise_hot_loop_and_gil(self):
        a = ck.advise(advfns.loop, range(40), sample=8)
        hot = [x for x in a if x.id == "hot_loop"]
        self.assertTrue(hot)
        self.assertGreater(hot[0].evidence["py_share"], 0.5)
        self.assertIn("numpy", hot[0].hint)
        if _advise.gil_enabled():
            self.assertIn("gil_bound", a.ids())
        else:  # free-threaded: threads do scale, so the GIL must not be blamed (threads_ok shows when the host has the cores)
            self.assertNotIn("gil_bound", a.ids())

    def test_advise_io_bound(self):
        self.assertIn("io_bound", self.ids(advfns.waits, range(30)))

    def test_advise_high_memory(self):
        a = ck.advise(advfns.hog, range(4), sample=2)
        m = [x for x in a if x.id == "high_memory"]
        self.assertTrue(m)
        self.assertGreaterEqual(m[0].evidence["mem_mb"], 180)

    def test_advise_side_effects(self):
        a = ck.advise(advfns.remember, range(10), sample=4)
        s = [x for x in a if x.id == "side_effects"]
        self.assertTrue(s)
        self.assertIn("CACHE", s[0].evidence["globals"])

    def test_advise_unpicklable_function(self):
        a = ck.advise(lambda x: x + 1, range(10), sample=3)
        self.assertEqual(a[0].id, "unpicklable")  # a blocker ranks first
        self.assertIn("lambda", a[0].why)
        self.assertIn("unpicklable", ck.advise(sdkfns.ident, [threading.Lock()] * 3, sample=2).ids())

    def test_advise_nondeterministic(self):
        a = ck.advise(advfns.noisy, range(10), sample=4)
        n = [x for x in a if x.id == "nondeterministic"]
        self.assertTrue(n and n[0].severity == "warn" and n[0].evidence["differs"])
        self.assertNotIn("nondeterministic", self.ids(advfns.tiny, range(10)))

    def test_advise_missing_imports(self):
        a = ck.advise(advfns.uses_pandas, range(10), sample=3)
        m = [x for x in a if x.id == "missing_imports"]
        self.assertTrue(m)
        self.assertEqual(m[0].evidence["third_party"], ["pandas"])

    def test_advise_ranking_and_fields(self):
        a = ck.advise(advfns.loop, range(40), sample=8)
        for s in a:
            self.assertTrue(s.id and s.title and s.why and s.hint)
        gains = [s.gain_s or 0 for s in a if s.severity == "gain"]
        self.assertEqual(gains, sorted(gains, reverse=True))
        self.assertIn("1.", str(a))


if __name__ == "__main__":
    unittest.main()
