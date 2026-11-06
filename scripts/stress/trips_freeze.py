#!/usr/bin/env python3
"""Round 4 (rt1) freeze tests for the folded ACK / pipelined chunk path (TASK2, ACKS, pipeline depth 2).

  CK_BIN=... BASE_PORT=14300 HB_DIR=... python3 -I scripts/stress/trips_freeze.py stop|kill|stopall|all

stop     SIGSTOP one agent process (4 virtual PCs, each holding a running chunk and a queued one) for 9 s, then SIGCONT.
kill     kill -9 one agent process mid-job; its running and prefetched chunks must be re-run elsewhere.
stopall  SIGSTOP the head for 6 s mid-job (every agent keeps its un-ACKed results and resends what the head lost), then SIGCONT.
Each case: 1 head + 3 agent processes x 4 virtual PCs, a synth job of ~20 s of work, exact count and a time bound. Exit 1 on failure.
"""
from __future__ import annotations

import os, signal, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import hier_bench as hb  # noqa: E402

N, CHUNK, NS = 240_000_000, 2_000_000, 1000  # 120 chunks of ~2 s of work; 12 PCs -> about 20 s
BAD: list[str] = []


def case(name: str, base: int) -> None:
    f = hb.Fleet("tf-" + name, False, base, labs=3, pcs=4, threads=1)
    try:
        th, box = f.job_async("--synth", str(NS), "--n", str(N), "--chunk", str(CHUNK))
        time.sleep(6)
        t = time.time()
        if name == "stop":
            f.sig("pcB", signal.SIGSTOP)
            time.sleep(9)
            f.sig("pcB", signal.SIGCONT)
        elif name == "kill":
            f.sig("pcB", signal.SIGKILL)
        elif name == "stopall":
            f.sig("root", signal.SIGSTOP)
            time.sleep(6)
            f.sig("root", signal.SIGCONT)
        th.join(150)
        wall = box.get("t", 999.0)
        got = hb.counted(box.get("out", ""))
        # ideal ~20 s on 12 PCs; losing a third of them for the rest of the job costs at most ~50 % more, plus the 5 s to notice
        bound = 20 * 1.5 + 25
        ok = got == N and wall <= bound
        print(f"{'PASS' if ok else 'FAIL'} {name}: counted={got} (want {N}) wall={wall:.1f}s bound={bound:.0f}s  (event at +{t - (time.time() - wall) :.0f}s)", flush=True)
        if not ok:
            BAD.append(name)
    finally:
        f.stop()


if __name__ == "__main__":
    what = sys.argv[1] if len(sys.argv) > 1 else "all"
    for i, c in enumerate(("stop", "kill", "stopall")):
        if what in (c, "all"):
            case(c, hb.BASE + 40 * i)
    print("FAILED: " + ", ".join(BAD) if BAD else "ALL PASSED")
    sys.exit(1 if BAD else 0)
