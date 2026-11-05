"""python3 -m unittest discover -s python/tests   (offline; the head is faked, the pushed runner really runs)"""
import io, json, os, subprocess, sys, tempfile, unittest
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, ".."))
import clusterkit as ck
from clusterkit import _runner
import sdkfns


class FakeHead:
    """Stands in for _http: runs the pushed script for real, two pieces per job (like two threads), joins in order."""
    def __init__(self, workers=None):
        self.jobs, self.posted = {}, []
        self.workers = workers if workers is not None else [{"threads": 4, "mem_total": 8 << 30, "mem_used": 1 << 30, "node_id": f"n{i}"} for i in range(4)]

    def __call__(self, method, path, body=None, text=False, timeout=60):
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
        return {"job": {"state": "done"}}


class T(unittest.TestCase):
    def setUp(self):
        self.head = FakeHead()
        ck._http = self.head

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


if __name__ == "__main__":
    unittest.main()
