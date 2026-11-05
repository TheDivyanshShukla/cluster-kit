import base64, json, os, pathlib, socket, subprocess, sys, tempfile, time, unittest, zlib

from clusterkit import _runner

SRC = "import os\nimport time\nT0 = time.time()\ndef f(x): return [x * 2, os.getpid(), T0]\n"


def _script(batch: bool = False) -> str:
    payload = {"mods": [["__ck_user__", SRC]], "main": "__ck_user__", "attr": "f", "mode": "call"}
    g = base64.b64encode(zlib.compress(json.dumps(payload).encode())).decode()
    return f"BATCH = {batch!r}\nPAYLOAD = {g!r}\n" + pathlib.Path(_runner.__file__).read_text()


@unittest.skipIf(os.name == "nt", "warm runner is off on Windows")
class WarmRunner(unittest.TestCase):
    def go(self, script: str, x: int, env: dict) -> list:
        p = subprocess.run([sys.executable, script], input=_runner.enc(x) + "\n", capture_output=True, text=True, env=env, timeout=60)
        self.assertEqual(p.returncode, 0, p.stderr)
        return _runner.dec(p.stdout.strip())[1]

    def test_second_task_reuses_worker_and_matches_cold(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as t:
            s = os.path.join(t, "r.py")
            pathlib.Path(s).write_text(_script())
            env = dict(os.environ, TMPDIR=t, CK_WARM_IDLE_S="3")
            cold = self.go(s, 21, dict(env, CK_WARM="off"))
            first = self.go(s, 21, env)  # cold, leaves a worker behind
            self.assertEqual(first[0], cold[0])
            d = os.path.join(t, "ck-warm-" + str(os.getuid()))
            self.assertEqual(oct(os.stat(d).st_mode & 0o777), "0o700")
            for _ in range(100):
                if any(n.endswith(".s") for n in os.listdir(d)):
                    break
                time.sleep(0.1)
            a, b = self.go(s, 21, env), self.go(s, 5, env)
            self.assertEqual((a[0], b[0]), (42, 10))
            self.assertEqual(a[2], b[2])  # same warm worker: module code ran once (same T0)
            self.assertNotEqual(a[2], first[2])
            time.sleep(4.5)  # idle exit removes the socket
            self.assertFalse(any(n.endswith(".s") for n in os.listdir(d)))

    def test_peer_uid_is_own_uid(self):
        a, b = socket.socketpair(socket.AF_UNIX)
        self.assertEqual(_runner._peer_uid(a), os.getuid())
        a.close(); b.close()


if __name__ == "__main__":
    unittest.main()
