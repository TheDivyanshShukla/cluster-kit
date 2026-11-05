"""Python-version behaviour: pickle protocol, syntax check against the PCs' Python, local multiprocessing under any start method, mean_params."""
from __future__ import annotations
import os, sys, unittest
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path[:0] = [HERE, os.path.join(HERE, "..")]
import clusterkit as ck
from clusterkit import _runner
import pickle
import sdkfns


class Versions(unittest.TestCase):
    def tearDown(self) -> None:
        ck._cfg["pc_python"] = None
        _runner.PROTO = ck._proto()

    def test_protocol_is_the_lower_of_both_sides(self):
        self.assertEqual(ck._proto(), min(pickle.HIGHEST_PROTOCOL, 5))
        ck.config(pc_python="3.9")
        self.assertEqual(_runner.PROTO, min(pickle.HIGHEST_PROTOCOL, 5))
        ck.config(pc_python="3.7")
        self.assertEqual(_runner.PROTO, 4)
        self.assertEqual(_runner.dec(_runner.enc({"a": [1, 2]})), {"a": [1, 2]})
        with self.assertRaises(TypeError):
            ck.config(pc_python="three")

    @unittest.skipIf(sys.version_info < (3, 10), "ast.parse(feature_version=) on 3.9 does not flag 3.8-era syntax; the PC-side check still does")
    def test_new_syntax_is_refused_before_shipping(self):
        ck.config(pc_python="3.7")  # sdkfns.walrus uses positional-only parameters, new in 3.8
        with self.assertRaises(ck.ClusterError) as c:
            ck._payload(sdkfns.walrus, "call")
        self.assertIn("pc_python=3.7", str(c.exception))
        ck.config(pc_python="3.9")
        ck._payload(sdkfns.walrus, "call")

    def test_runner_names_its_python_on_bad_syntax(self):
        payload = {"mods": [["m", "def f(:\n"]], "main": "m", "attr": "f", "mode": "call"}
        with self.assertRaises(SyntaxError) as c:
            _runner.load(payload)
        self.assertIn("this PC runs Python {}.{}".format(*sys.version_info[:2]), str(c.exception))

    def test_local_parallel_matches_serial(self):
        items = list(range(40))
        self.assertEqual(ck._local(sdkfns.ident, "call", items, 2), items)

    def test_mean_params_plain(self):
        self.assertEqual(ck.mean_params([{"w": [1.0, 3.0], "b": 2.0}, {"w": [3.0, 5.0], "b": 4.0}]), {"w": [2.0, 4.0], "b": 3.0})


if __name__ == "__main__":
    unittest.main()
