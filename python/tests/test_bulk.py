"""python3 -m unittest discover -s python/tests   (bulk distribution, offline: a fake head with a blob store; the pushed runner really runs)"""
from __future__ import annotations
import hashlib, http.server, json, os, shutil, sys, tempfile, threading, unittest
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path[:0] = [HERE, os.path.join(HERE, "..")]
from typing import Any
import clusterkit as ck
from clusterkit import _bulk, _hist, _runner
import bigfns, sdkfns
from test_sdk import FakeHead


class BulkHead(FakeHead):
    def __init__(self, **kw: Any) -> None:
        super().__init__(**kw)
        self.blobs, self.puts, self.heads, self.est, self.est_calls, self.net, self.holders = {}, 0, 0, None, [], None, {}
        self.dir = tempfile.mkdtemp()

    def __call__(self, method, path, body=None, text=False, timeout=60, raw=False, length=None) -> Any:
        if path == "/api/v1/blobs" and method == "GET":
            return {"blobs": [{"sha256": k, "size": len(v), "holders": self.holders.get(k, 0)} for k, v in self.blobs.items()]}
        if path == "/api/v1/blobs" and method == "PUT":
            data = body if isinstance(body, bytes) else b"".join(iter(lambda: body.read(8192), b""))
            assert length is None or length == len(data)
            sha = hashlib.sha256(data).hexdigest()
            existed = sha in self.blobs
            self.blobs[sha] = data
            self.puts += 1
            return {"sha256": sha, "size": len(data), "existed": existed}
        if path.startswith("/api/v1/blobs/"):
            sha = path.rsplit("/", 1)[1]
            if sha not in self.blobs:
                raise ck.ClusterError(f"{method} {path}: HTTP 404 not found")
            self.heads += method == "HEAD"
            return {} if method == "HEAD" else self.blobs[sha]
        if path == "/api/v1/net":
            if self.net:
                return self.net
            raise ck.ClusterError("HTTP 404")
        if path.startswith("/api/v1/estimate"):
            self.est_calls.append(path)
            if self.est:
                return self.est(path)
            raise ck.ClusterError("HTTP 404")
        if method == "POST" and path == "/api/v1/jobs":
            for sha in body.get("blobs", []):
                assert sha in self.blobs, "job names a blob the head does not have"
                with open(os.path.join(self.dir, sha), "wb") as f:
                    f.write(self.blobs[sha])
            b2 = dict(body, items=[os.path.join(self.dir, i[6:]) if i.startswith("@blob:") else i for i in body["items"]])  # what the head does
            os.environ["CK_BLOBS"] = self.dir
            try:
                r = super().__call__(method, path, b2)
            finally:
                os.environ.pop("CK_BLOBS", None)
            self.posted[-1] = body
            return r
        return super().__call__(method, path, body, text, timeout)


class PutServer:
    """Where a PC would send a big result: PUT /api/v1/blobs into the fake head's store."""
    def __init__(self, head) -> None:
        class H(http.server.BaseHTTPRequestHandler):
            def do_PUT(s) -> None:
                data = s.rfile.read(int(s.headers["Content-Length"]))
                sha = hashlib.sha256(data).hexdigest()
                head.blobs[sha] = data
                out = json.dumps({"sha256": sha, "size": len(data)}).encode()
                s.send_response(200)
                s.send_header("Content-Length", str(len(out)))
                s.end_headers()
                s.wfile.write(out)

            def log_message(s, *a: Any) -> None:
                pass
        self.srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.url = f"http://127.0.0.1:{self.srv.server_address[1]}"
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()


class B(unittest.TestCase):
    def setUp(self):
        self.head = BulkHead()
        ck._http = self.head
        self.home = tempfile.TemporaryDirectory()
        self.tmp = tempfile.mkdtemp()
        os.environ["CK_SDK_HOME"] = self.home.name
        self._cfg = dict(ck._cfg)
        ck.config(push=False, head="http://fake", bulk_results=False)
        ck._cmds.clear()
        _bulk._ok.clear()
        _bulk._by_sha.clear(), _bulk._by_id.clear(), _bulk._by_file.clear()

    def tearDown(self):
        ck._cfg.clear()
        ck._cfg.update(self._cfg)
        self.home.cleanup()
        shutil.rmtree(self.tmp, ignore_errors=True)
        shutil.rmtree(self.head.dir, ignore_errors=True)

    def file(self, lines=1000, name="data.txt"):
        p = os.path.join(self.tmp, name)
        with open(p, "w") as f:
            f.writelines(f"line {i} {'x' * (i % 17)}\n" for i in range(lines))
        return p

    def test_put_dedup(self):
        a, b = ck.put({"k": list(range(50))}), ck.put({"k": list(range(50))})
        self.assertEqual(a.sha, b.sha)
        self.assertEqual(self.head.puts, 1)  # same bytes: one upload, the second call is a local hit plus a HEAD at most
        p = self.file()
        h = ck.put(p)
        self.assertEqual(h.sha, hashlib.sha256(open(p, "rb").read()).hexdigest())
        ck.put(p)
        self.assertEqual(self.head.puts, 2)
        self.assertEqual(h.size, os.path.getsize(p))
        self.assertEqual(h.local(), os.path.abspath(p))
        self.assertTrue(_hist.link("http://fake") == {})  # tiny transfers do not teach a link speed

    def test_handle_resolution_in_runner(self):
        obj, p = ck.put(list(range(100))), ck.put(self.file(10))
        self.assertEqual(ck.map(bigfns.total, [obj, obj], local="never"), [4950, 4950])
        self.assertEqual(ck.map(bigfns.lines, [p, p], local="never"), [10, 10])
        # handles inside tuple/dict items (star / kw modes)
        out = ck._run(bigfns.star_len, "star", [(obj, obj), ([1], obj)], ck._opts({"local": "never"}))
        self.assertEqual(out, [200, 101])
        # a blob is shipped with the job and named in `blobs`
        self.assertEqual(set(self.head.posted[-1]["blobs"]), {obj.sha})
        # the same call locally resolves through the registry
        self.assertEqual(ck.map(bigfns.total, [obj], local="always"), [4950])

    def test_shard_file_roundtrip(self):
        p = self.file(1000)
        hs = ck.shard(p, 7)
        self.assertEqual(len(hs), 7)
        self.assertEqual(sum(h.size for h in hs), os.path.getsize(p))
        before = self.head.puts
        out = ck.map(bigfns.lines, hs, local="never")
        self.assertEqual(sum(out), 1000)
        self.assertEqual(self.head.puts, before)  # already on the head: the map moves nothing
        b = self.head.posted[-1]
        self.assertTrue(all(i.startswith("@blob:") for i in b["items"]) or b["items"], b["items"][:1])
        self.assertEqual(set(b["blobs"]) & {h.sha for h in hs}, {h.sha for h in hs})
        ck.shard(p, 7)
        self.assertEqual(self.head.puts, before)  # sharding again dedups on the sha
        # list shards
        ls = ck.shard(list(range(100)), 4)
        self.assertEqual(sum(ck.map(bigfns.total, ls, local="never")), 4950)

    def test_batched_shards_still_resolve(self):
        hs = ck.shard(list(range(400)), 40)
        out = ck.map(bigfns.total, hs, local="never", batch=True)
        self.assertEqual(sum(out), sum(range(400)))

    def test_auto_blob_threshold(self):
        small, big = b"s" * 100, os.urandom(6000)  # random: does not compress under max_item_bytes=4096
        out = ck.map(sdkfns.ident, [small, small, big], local="never", batch=False)
        self.assertEqual(out, [small, small, big])
        self.assertEqual(len(self.head.blobs), 1)
        self.assertEqual(len(self.head.posted[-1]["blobs"]), 1)
        # a big shared argument in star mode: one blob for all items, the small part stays inline
        shared = os.urandom(20000)
        items = [(shared, i) for i in range(6)]
        out = ck._run(bigfns.shared_plus, "star", items, ck._opts({"local": "never", "batch": False}))
        self.assertEqual(out, [20000 + i for i in range(6)])
        self.assertEqual(len([s for s in self.head.blobs.values() if len(s) > 15000]), 1)
        # threshold knob
        ck.config(blob_auto_kb=0)
        n = len(self.head.blobs)
        ck.map(sdkfns.ident, ["a" * 1000, "b" * 1000], local="never", batch=False)
        self.assertGreater(len(self.head.blobs), n)

    def test_bulk_results_come_back_as_blobs(self):
        srv = PutServer(self.head)
        ck.config(bulk_results=True, result_head=srv.url, blob_auto_kb=64)
        out = ck.map(bigfns.make, [200_000, 300_000, 10], local="never", batch=False)
        self.assertEqual([len(x) for x in out], [200_000, 300_000, 10])
        st = ck._last["st"]
        self.assertGreater(st["down_bytes"], 0)
        # batched results too
        out = ck.map(bigfns.make, [100_000] * 5, local="never", batch=True)
        self.assertEqual([len(x) for x in out], [100_000] * 5)
        srv.srv.shutdown()

    def test_bulk_result_falls_back_inline_when_pc_cannot_reach_head(self):
        ck.config(bulk_results=True, result_head="http://127.0.0.1:9", blob_auto_kb=64)
        out = ck.map(bigfns.make, [200_000, 5], local="never", batch=False)
        self.assertEqual([len(x) for x in out], [200_000, 5])
        self.assertNotIn("down_bytes", ck._last["st"])

    def test_big_module_data_and_code_become_blobs(self):
        ck.config(code_blob_kb=0, blob_auto_kb=64)
        out = ck.map(bigfns.lookup, list(range(8)), local="never", batch=False)
        self.assertEqual(out, [i + 1 for i in range(8)])
        b = self.head.posted[-1]
        self.assertIn("PAYLOAD_BLOB", b["cmd"])
        self.assertNotIn("\nPAYLOAD = ", b["cmd"])
        self.assertGreaterEqual(len(b["blobs"]), 2)  # code + DATA
        self.assertLess(len(b["cmd"]), 16000)  # the runner is about 13 KB (warm worker included)
        n = self.head.puts
        ck.map(bigfns.lookup, list(range(8)), local="never", batch=False)
        self.assertEqual(self.head.puts, n)  # code and data are not sent again
        self.assertEqual(len(ck._cmds), 1)

    def test_old_head_keeps_old_behaviour(self):
        ck._http = FakeHead()
        _bulk._ok.clear()
        with self.assertRaises(ck.ClusterError) as e:
            ck.map(sdkfns.ident, [os.urandom(6000)] * 2, local="never")
        self.assertIn("max_item_bytes", str(e.exception))
        with self.assertRaises(ck.ClusterError):
            ck.put([1])

    def test_formulas_and_strategy_choice(self):
        # no estimate API: BULK.md formulas on a known uplink (125 MB/s = 1 Gbit/s), one lab
        self.head.net = {"head_uplink_mbps": 1000, "nodes": [{"net_mbps": 1000}] * 4, "labs": [{"lab": "10.0.0", "pcs": 64}]}
        x = _bulk.Xfer(ck, ck._opts({}), 1_000_000_000, 0, 10.0, 100, 64)
        d1, d64 = x.formula(1e9, 1), x.formula(1e9, 64)
        self.assertAlmostEqual(d1["direct"], 8.0, places=3)  # 1 GB at 125 MB/s
        self.assertAlmostEqual(d64["direct"], 64 * 8.0, places=2)  # the head sends every copy
        self.assertAlmostEqual(d64["peer"], 8.0 * (1 + 3) + 8.0, places=2)  # fanout 4: ceil(log4 64) = 3 levels, plus one seed from the head
        self.assertLess(d64["peer"], d64["direct"])
        self.assertEqual(x.at(1)[1], "direct")
        t, strat, alt = x.at(64)
        self.assertEqual((strat, round(t, 2)), ("peer", round(d64["peer"], 2)))
        self.assertEqual(x.source, "formula")
        # per-item bytes are limited by the uplink and the PCs' links
        y = _bulk.Xfer(ck, ck._opts({}), 0, 500_000_000, 10.0, 50, 20)
        self.assertAlmostEqual(y.at(1)[0], 4.0, places=3)
        self.assertAlmostEqual(y.at(20)[0], 4.0, places=3)  # uplink-bound
        self.assertEqual(y.at(4)[1], "direct")

    def test_head_estimate_api_is_used_when_present(self):
        def est(path) -> dict[str, Any]:
            return {"best": {"strategy": "peer", "pcs": 1, "finish_s": 30}, "transfer_s": {"direct": 1.0, "peer": 2.0}, "assumed": ["link_speed"],
                    "alternatives": [{"strategy": "direct", "pcs": 1, "transfer_s": 1.0}, {"strategy": "peer", "pcs": 1, "transfer_s": 2.0},
                                     {"strategy": "direct", "pcs": 4, "transfer_s": 90.0}, {"strategy": "peer", "pcs": 4, "transfer_s": 12.0}]}
        self.head.est = est
        x = _bulk.Xfer(ck, ck._opts({}), 123456789, 0, 5.0, 10, 4)
        t, strat, alt = x.at(3)
        self.assertEqual((t, strat, alt), (12.0, "peer", {"direct": 90.0, "peer": 12.0}))
        self.assertEqual(x.source, "head")
        q = self.head.est_calls[0]
        self.assertIn("bytes=123456789", q)
        self.assertIn("max_pcs=4", q)  # power-of-two buckets: 3 PCs asked as 4
        x.at(4)
        self.assertEqual(len(self.head.est_calls), 1)  # memoised

    def test_items_with_handles_inside_name_their_blobs(self):
        a, b = _bulk.from_bytes(b"shard-a" * 50), _bulk.from_bytes(b"shard-b" * 50)
        lines = ck._encode([(1, a), (2, {"x": b}), 3, a])  # a tuple, a dict inside, a plain item, a bare handle
        self.assertTrue(lines[0].startswith(f"@refs:{a.sha}|"), lines[0][:90])
        self.assertTrue(lines[1].startswith(f"@refs:{b.sha}|"), lines[1][:90])
        self.assertEqual(lines[3], "@blob:" + a.sha)
        self.assertFalse(lines[2].startswith("@"))
        self.assertEqual(_runner.dec(lines[0]), (1, {"__ck_blob__": a.sha}))  # the runner drops the prefix (a handle is a marker dict on the PC)
        batch = ck._groups([(1, a), (2, b)], 2, 1 << 20)
        self.assertEqual(batch[0].split("|")[0], "@refs:" + ",".join(sorted([a.sha, b.sha])))

    def test_plan_has_transfer_and_strategy(self):
        p = self.file(2000)
        hs = ck.shard(p, 20)
        self.head.workers = [{"threads": 4, "mem_total": 8 << 30, "mem_used": 1 << 30, "node_id": f"n{i}"} for i in range(8)]
        self.head.net = {"head_uplink_mbps": 80, "nodes": [{"net_mbps": 80}] * 8, "labs": [{"lab": "a", "pcs": 8}]}  # 10 MB/s: slow
        ck.config(local="never")
        pl0 = ck.plan(bigfns.lines, hs, blob_per_chunk=False)  # a head from before `@refs:` items: every PC that gets a chunk holds all the job's blobs
        self.assertEqual((pl0.shared_bytes, pl0.item_bytes), (os.path.getsize(p), 0))
        pl = ck.plan(bigfns.lines, hs)  # the default: each shard goes only to the PC that runs its item
        self.assertEqual(pl.item_bytes, os.path.getsize(p))
        self.assertGreater(pl.bytes, 0)
        self.assertIn(pl.strategy, ("direct", "peer"))
        self.assertTrue(pl.finish_by_pcs)
        self.assertIn("data", str(pl))
        self.assertIn("to move", pl.transfer())
        # cached on a PC already: counts as moved
        self.head.holders = {h.sha: 1 for h in hs}
        pl2 = ck.plan(bigfns.lines, hs)
        self.assertEqual(pl2.item_bytes, 0)

    def test_transfer_dominates_advice(self):
        from clusterkit import _advise
        pl = ck.Plan(where="cluster", pcs=8, cluster_s=60.0, bytes=2 * 10**9, shared_bytes=2 * 10**9, item_bytes=0, transfer_s=45.0, strategy="peer",
                     transfer_alt={"direct": 300.0, "peer": 45.0}, finish_by_pcs={1: 80.0, 8: 60.0})
        s = _advise._transfer({"plan": pl})
        self.assertEqual(s.id, "transfer_dominates")
        self.assertIn("shard", s.hint)
        self.assertIn("fewer PCs", s.hint)
        pl.transfer_s = 5.0  # small share of the finish time: no advice
        self.assertIsNone(_advise._transfer({"plan": pl}))

    def test_link_speed_is_learned_from_uploads(self):
        ck.put(os.urandom(2_000_000))
        L = _hist.link("http://fake")
        self.assertGreater(L["up"], 0)
        ck.config(local="never")
        got = _bulk.Xfer(ck, ck._opts({}), 10**9, 0, 1.0, 5, 4)
        self.assertEqual(got.assumed, [])  # a measured link: nothing assumed
        self.assertAlmostEqual(got.up, L["up"])

    def test_runner_markers_have_no_clusterkit_dependency(self):
        h = ck.put([1, 2, 3])
        import pickle
        m = pickle.loads(pickle.dumps(h))
        self.assertEqual(m, {"__ck_blob__": h.sha})
        self.assertEqual(_runner.resolve((m, 5)), ([1, 2, 3], 5))


if __name__ == "__main__":
    unittest.main()
