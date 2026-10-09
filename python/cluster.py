"""
pc-cluster: turn ordinary Windows/Ubuntu PCs on one LAN into a Dask cluster.

  uv run cluster.py head        # on the master PC   (scheduler + beacon + local worker)
  uv run cluster.py worker      # on every other PC  (auto-discovers the head)
  uv run cluster.py status      # who is connected
  uv run cluster.py bench       # 1 -> N PC scaling benchmark (run on the head)
  uv run cluster.py fw-clean    # remove any leftover firewall rules
  uv run cluster.py fwrun --role ck-head --tcp 7700,7702 -- ck head
                                # open ports, run any program, close ports when it exits

Firewall ports are opened only while head/worker runs and are closed again on
Ctrl+C, window close, logoff/shutdown, kill (SIGTERM/SIGHUP) or normal exit.
If a PC loses power, the stale rules are removed the next time it starts.
"""

from __future__ import annotations

import argparse
import atexit
import csv
import ipaddress
import json
import os
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import threading
import time

SCHED_PORT = 8786
DASH_PORT = 8787
DISC_PORT = 8785          # UDP discovery beacon
WORKER_PORTS = "9000:9100"
NANNY_PORTS = "9101:9200"
RULE_TAG = "daskcluster"  # every firewall rule we create carries this tag
ROLES = ("head", "worker", "ck-head", "ck-worker")  # rule groups fw-clean removes
MAGIC = "pc-cluster/1"
IS_WIN = os.name == "nt"


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


# --------------------------------------------------------------------------- #
# Networking helpers
# --------------------------------------------------------------------------- #
def lan_interfaces() -> list[tuple[str, ipaddress.IPv4Network]]:
    """(ip, network) for every usable IPv4 interface (no loopback / link-local)."""
    import psutil  # ships with distributed

    out = []
    for addrs in psutil.net_if_addrs().values():
        for a in addrs:
            if a.family != socket.AF_INET or not a.netmask:
                continue
            ip = ipaddress.IPv4Address(a.address)
            if ip.is_loopback or ip.is_link_local:
                continue
            out.append((a.address, ipaddress.IPv4Network(f"{a.address}/{a.netmask}", strict=False)))
    return out


def ip_toward(dest: str) -> str:
    """Local IP the OS would use to reach `dest` (no packet is actually sent)."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect((dest, 9))
        return s.getsockname()[0]
    except OSError:
        ifs = lan_interfaces()
        return ifs[0][0] if ifs else "127.0.0.1"
    finally:
        s.close()


def primary_ip() -> str:
    return ip_toward("8.8.8.8")


def subnet_of(ip: str) -> str:
    for addr, net in lan_interfaces():
        if addr == ip:
            return str(net)
    return f"{ip}/32"


def tcp_ok(ip: str, port: int, timeout: float = 2.0) -> bool:
    try:
        with socket.create_connection((ip, port), timeout=timeout):
            return True
    except OSError:
        return False


# --------------------------------------------------------------------------- #
# Cleanup: runs exactly once on any exit path
# --------------------------------------------------------------------------- #
_cleanup_lock = threading.Lock()
_cleanup_done = False
_procs: list[subprocess.Popen] = []
_firewall: "Firewall | None" = None
_client = None  # head's monitoring client, closed before the scheduler stops


def run_cleanup(reason: str = "exit", firewall_first: bool = False) -> None:
    global _cleanup_done
    with _cleanup_lock:
        if _cleanup_done:
            return
        _cleanup_done = True
    log(f"Shutting down ({reason})...")
    if firewall_first and _firewall:   # window close: Windows only waits ~5 s, close ports first
        _firewall.close()
    if _client is not None:
        try:
            _client.close(timeout=2)
        except Exception:
            pass
    for p in _procs:
        if p.poll() is None:
            try:
                p.terminate()
            except OSError:
                pass
    deadline = time.time() + 5
    for p in _procs:
        try:
            p.wait(timeout=max(0.1, deadline - time.time()))
        except subprocess.TimeoutExpired:
            p.kill()
    if _firewall:
        _firewall.close()
    log("Stopped.")


def install_exit_handlers() -> None:
    atexit.register(run_cleanup, "exit")

    stopping = False

    def to_interrupt(signum, _frame):
        # Interrupt only once: a second Ctrl+C / SIGTERM (uv forwards them too) must not
        # abort run_cleanup halfway and orphan the scheduler and workers.
        nonlocal stopping
        if not stopping:
            stopping = True
            raise KeyboardInterrupt(f"signal {signum}")

    signal.signal(signal.SIGINT, to_interrupt)  # Ctrl+C, even if inherited as ignored
    signal.signal(signal.SIGTERM, to_interrupt)
    if hasattr(signal, "SIGHUP"):            # terminal closed (Linux)
        signal.signal(signal.SIGHUP, to_interrupt)
    if hasattr(signal, "SIGBREAK"):          # Ctrl+Break (Windows)
        signal.signal(signal.SIGBREAK, to_interrupt)

    if IS_WIN:                               # window close / logoff / shutdown
        import ctypes
        from ctypes import wintypes

        handler_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.DWORD)

        def console_handler(event: int) -> bool:
            if event in (2, 5, 6):  # CTRL_CLOSE, CTRL_LOGOFF, CTRL_SHUTDOWN
                run_cleanup("window closed / shutdown", firewall_first=True)
                return True
            return False            # Ctrl+C -> normal KeyboardInterrupt path

        install_exit_handlers._ref = handler_type(console_handler)  # keep alive
        ctypes.windll.kernel32.SetConsoleCtrlHandler(install_exit_handlers._ref, True)


def spawn(cmd: list[str]) -> subprocess.Popen:
    env = dict(os.environ, DASK_LOGGING__DISTRIBUTED="warning")  # keep consoles readable
    p = subprocess.Popen(cmd, env=env)
    _procs.append(p)
    return p


# --------------------------------------------------------------------------- #
# Firewall management (Windows Defender Firewall / Ubuntu ufw)
# --------------------------------------------------------------------------- #
class _RootShell:
    """One long-lived root shell (sudo asked once). It ignores SIGHUP/SIGINT,
    so firewall rules can still be removed after the terminal is closed."""

    def __init__(self) -> None:
        prefix = [] if os.geteuid() == 0 else ["sudo"]
        loop = 'trap "" HUP INT; while IFS= read -r l; do eval "$l" 2>&1; echo "__RC__ $?"; done'
        if prefix:
            log("Firewall: sudo password needed once to manage ufw rules")
        self.p = subprocess.Popen(prefix + ["sh", "-c", loop], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, text=True, bufsize=1)

    def run(self, cmd: str) -> tuple[int, str]:
        self.p.stdin.write(cmd + "\n")
        self.p.stdin.flush()
        out = []
        for line in self.p.stdout:
            if line.startswith("__RC__ "):
                return int(line.split()[1]), "".join(out)
            out.append(line)
        return 1, "".join(out)

    def close(self) -> None:
        try:
            self.p.stdin.close()
            self.p.wait(timeout=5)
        except Exception:
            pass


class Firewall:
    def __init__(self, enabled: bool, rules: list[tuple[str, str]], subnet: str, role: str,
                 exes: tuple[str, ...] = ()) -> None:
        self.rules = rules          # [(proto, "8786:8787"), ...]
        self.exes = exes            # extra programs whose Windows BLOCK rules get removed
        self.tag = f"{RULE_TAG}-{role}"   # e.g. daskcluster-head; fw-clean matches every role
        self.subnet = subnet
        self.backend = None
        self.opened = False
        self.sh: _RootShell | None = None
        if not enabled:
            log("Firewall: management disabled (--no-firewall)")
        elif IS_WIN:
            import ctypes
            if ctypes.windll.shell32.IsUserAnAdmin():
                self.backend = "windows"
            else:
                log("Firewall: NOT admin -> skipping. Use the .ps1/.bat launcher (it self-elevates).")
        elif shutil.which("ufw"):
            self.sh = _RootShell()
            rc, out = self.sh.run("ufw status")
            if rc != 0:
                log(f"Firewall: cannot run ufw ({out.strip()}) -> skipping")
            elif "Status: active" in out:
                self.backend = "ufw"
            else:
                log("Firewall: ufw is inactive -> ports already open, nothing to do")
        else:
            log("Firewall: no ufw found -> assuming ports are open")

    # -- Windows ----------------------------------------------------------- #
    @staticmethod
    def _ps(command: str) -> subprocess.CompletedProcess:
        return subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", command],
                              capture_output=True, text=True)

    # -- common ------------------------------------------------------------ #
    def remove_tagged(self, tag: str | None = None) -> None:
        tag = tag or self.tag
        if self.backend == "windows":
            groups = [f"{RULE_TAG}-{r}" for r in ROLES] if tag == RULE_TAG else [tag]
            for g in groups:  # exact -Group lookup is fast (matters: window-close gives us ~5 s)
                self._ps(f"Get-NetFirewallRule -Group '{g}' -ErrorAction SilentlyContinue "
                         f"| Remove-NetFirewallRule")
        elif self.backend == "ufw":
            _, out = self.sh.run("ufw status numbered")
            nums = [int(l.split("]")[0].strip(" [")) for l in out.splitlines()
                    if tag in l and l.strip().startswith("[")]
            for n in sorted(nums, reverse=True):
                self.sh.run(f"ufw --force delete {n}")

    def open(self) -> None:
        if not self.backend:
            return
        self.remove_tagged()  # stale rules from a crash / power cut
        if self.backend == "windows":
            # Dismissing Windows' "allow Python?" popup creates BLOCK rules that beat any allow rule.
            exes = {sys.executable, getattr(sys, "_base_executable", sys.executable), *self.exes}
            for exe in exes:
                self._ps(f"Get-NetFirewallApplicationFilter -Program '{exe}' -ErrorAction SilentlyContinue "
                         f"| Get-NetFirewallRule | Where-Object Action -eq 'Block' | Remove-NetFirewallRule")
        for proto, ports in self.rules:
            if self.backend == "windows":
                r = self._ps(
                    f"New-NetFirewallRule -DisplayName '{self.tag} {proto.upper()} {ports}' "
                    f"-Group '{self.tag}' -Direction Inbound -Action Allow -Protocol {proto.upper()} "
                    f"-LocalPort {ports.replace(':', '-')} -RemoteAddress LocalSubnet -Profile Any | Out-Null")
                ok = r.returncode == 0
            else:
                rc, _ = self.sh.run(f"ufw allow from {self.subnet} to any port {ports} "
                                    f"proto {proto} comment {self.tag}")
                ok = rc == 0
            log(f"Firewall: {'opened' if ok else 'FAILED to open'} {proto.upper()} {ports}")
        self.opened = True

    def close(self) -> None:
        if self.backend and self.opened:
            self.opened = False
            self.remove_tagged()
            log("Firewall: rules removed, ports closed")
        if self.sh:
            self.sh.close()


def setup_firewall(args, rules: list[tuple[str, str]], local_ip: str, role: str | None = None,
                   exes: tuple[str, ...] = ()) -> None:
    global _firewall
    _firewall = Firewall(not args.no_firewall, rules, subnet_of(local_ip), role or args.cmd, exes)
    _firewall.open()


# --------------------------------------------------------------------------- #
# Auto-discovery (UDP broadcast)
# --------------------------------------------------------------------------- #
def beacon_loop(name: str, stop: threading.Event) -> None:
    payload = json.dumps({"magic": MAGIC, "name": name, "port": SCHED_PORT}).encode()
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    while not stop.is_set():
        targets = {"255.255.255.255"} | {str(net.broadcast_address) for _, net in lan_interfaces()}
        for t in targets:
            try:
                s.sendto(payload, (t, DISC_PORT))
            except OSError:
                pass
        stop.wait(2)


def discover(name: str) -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("", DISC_PORT))
    s.settimeout(1.0)
    log(f"Discovery: listening for head of cluster '{name}' on UDP {DISC_PORT} ...")
    last = time.time()
    try:
        while True:
            try:
                data, (ip, _) = s.recvfrom(2048)
            except socket.timeout:
                if time.time() - last > 15:
                    log("Discovery: still waiting (is the head running? same LAN?)")
                    last = time.time()
                continue
            try:
                msg = json.loads(data)
            except ValueError:
                continue
            if msg.get("magic") == MAGIC and msg.get("name") == name and tcp_ok(ip, msg["port"]):
                log(f"Discovery: found head at {ip}:{msg['port']}")
                return ip
    finally:
        s.close()


# --------------------------------------------------------------------------- #
# Commands
# --------------------------------------------------------------------------- #
def physical_cores() -> int:
    import psutil
    return psutil.cpu_count(logical=False) or os.cpu_count() or 1


def worker_cmd(head_ip: str, my_ip: str, procs: int) -> list[str]:
    return [sys.executable, "-m", "distributed.cli.dask_worker", f"tcp://{head_ip}:{SCHED_PORT}",
            "--nworkers", str(procs), "--nthreads", "1", "--host", my_ip,
            "--worker-port", WORKER_PORTS, "--nanny-port", NANNY_PORTS,
            "--death-timeout", "60", "--no-dashboard"]


def cmd_head(args) -> None:
    my_ip = primary_ip()
    setup_firewall(args, [("tcp", f"{SCHED_PORT}:{DASH_PORT}"), ("tcp", "9000:9200")], my_ip)

    sched = spawn([sys.executable, "-m", "distributed.cli.dask_scheduler",
                   "--port", str(SCHED_PORT), "--dashboard-address", f":{DASH_PORT}"])
    for _ in range(60):
        if tcp_ok("127.0.0.1", SCHED_PORT, 0.5):
            break
        if sched.poll() is not None:
            sys.exit("Scheduler failed to start (port 8786 busy?)")
        time.sleep(0.5)

    stop = threading.Event()
    threading.Thread(target=beacon_loop, args=(args.name, stop), daemon=True).start()

    if not args.no_local_worker:
        procs = args.procs or physical_cores()
        spawn(worker_cmd(my_ip, my_ip, procs))

    log(f"HEAD ready  scheduler tcp://{my_ip}:{SCHED_PORT}  dashboard http://{my_ip}:{DASH_PORT}")
    log(f"Broadcasting cluster '{args.name}' - start workers on the other PCs. Ctrl+C to stop.")

    global _client
    from distributed import Client
    client = _client = Client(f"tcp://127.0.0.1:{SCHED_PORT}", timeout=30)
    seen: dict[str, int] = {}
    while sched.poll() is None:
        hosts: dict[str, int] = {}
        try:
            workers = client.scheduler_info(n_workers=-1)["workers"].values()
        except Exception:
            time.sleep(3)
            continue
        for w in workers:
            hosts[w["host"]] = hosts.get(w["host"], 0) + 1
        for h in hosts.keys() - seen.keys():
            log(f"+ node joined  {h}  ({hosts[h]} procs)")
        for h in seen.keys() - hosts.keys():
            log(f"- node left    {h}")
        if hosts != seen:
            log(f"  cluster now: {len(hosts)} PCs, {sum(hosts.values())} worker procs")
        seen = hosts
        time.sleep(3)
    stop.set()


def cmd_worker(args) -> None:
    head_ip = args.head or None
    first_ip = ip_toward(head_ip) if head_ip else primary_ip()
    setup_firewall(args, [("tcp", "9000:9200"), ("udp", str(DISC_PORT))], first_ip)
    procs = args.procs or physical_cores()

    while True:
        ip = head_ip or discover(args.name)
        my_ip = ip_toward(ip)
        log(f"WORKER {my_ip} -> head {ip}  ({procs} procs). Ctrl+C to stop.")
        p = spawn(worker_cmd(ip, my_ip, procs))
        p.wait()
        _procs.remove(p)
        log("Lost the head - rediscovering in 3s ...")
        time.sleep(3)


def _connect(addr: str):
    from distributed import Client
    try:
        return Client(addr, timeout=10)
    except OSError:
        sys.exit(f"Cannot reach scheduler at {addr}. Is `cluster.py head` running?")


def host_table(client) -> tuple[list[str], dict[str, list[str]]]:
    hosts: dict[str, list[str]] = {}
    for addr, w in client.scheduler_info(n_workers=-1)["workers"].items():
        hosts.setdefault(w["host"], []).append(addr)
    head = client.scheduler_info()["address"].split("://")[1].rsplit(":", 1)[0]
    order = sorted(hosts, key=lambda h: (h != head, ipaddress.ip_address(h)))  # head PC first
    return order, hosts


def cmd_status(args) -> None:
    client = _connect(args.scheduler)
    order, hosts = host_table(client)
    info = client.scheduler_info(n_workers=-1)["workers"]
    print(f"\n{'#':<3}{'host':<18}{'procs':>6}{'memory':>12}")
    for i, h in enumerate(order, 1):
        mem = sum(info[a]["memory_limit"] for a in hosts[h]) / 2**30
        print(f"{i:<3}{h:<18}{len(hosts[h]):>6}{mem:>10.1f} GB")
    print(f"\n{len(order)} PCs, {sum(len(v) for v in hosts.values())} worker processes\n")


# --- benchmark ------------------------------------------------------------- #
def monte_carlo(n: int, seed: int) -> int:
    """CPU-bound pure-Python task: count random points inside the unit circle."""
    import random
    r = random.Random(seed)
    hit = 0
    for _ in range(n):
        x, y = r.random(), r.random()
        if x * x + y * y <= 1.0:
            hit += 1
    return hit


def cmd_bench(args) -> None:
    client = _connect(args.scheduler)
    order, hosts = host_table(client)
    max_nodes = min(args.max_nodes or len(order), len(order))
    total_procs = sum(len(hosts[h]) for h in order[:max_nodes])
    chunks = args.chunks or 4 * total_procs
    print(f"\nNodes (in order used): {', '.join(f'{h}[{len(hosts[h])}]' for h in order[:max_nodes])}")
    print(f"Chunks per run: {chunks}   repeats: {args.repeats}\n")

    # warm-up: imports + pickling on every worker
    client.gather(client.map(monte_carlo, [1000] * total_procs, range(total_procs), pure=False))

    raw, summary = [], []
    for total in args.total:
        total = int(total)
        print(f"== workload {total:,} samples ==")
        print(f"{'PCs':>4}{'procs':>7}{'median s':>11}{'speedup':>9}{'effic.':>9}{'pi':>10}")
        t1 = None
        for n in range(1, max_nodes + 1):
            allowed = [a for h in order[:n] for a in hosts[h]]
            times = []
            for rep in range(args.repeats):
                t = time.perf_counter()
                futs = client.map(monte_carlo, [total // chunks] * chunks,
                                  [rep * 10_000 + i for i in range(chunks)],
                                  workers=allowed, allow_other_workers=False, pure=False)
                hits = sum(client.gather(futs))
                dt = time.perf_counter() - t
                times.append(dt)
                raw.append({"workload": total, "pcs": n, "procs": len(allowed),
                            "repeat": rep + 1, "seconds": round(dt, 4)})
            med = statistics.median(times)
            t1 = t1 or med
            sp, eff = t1 / med, t1 / med / n
            pi = 4 * hits / ((total // chunks) * chunks)
            summary.append({"workload": total, "pcs": n, "procs": len(allowed),
                            "median_s": round(med, 4), "speedup": round(sp, 3), "efficiency": round(eff, 3)})
            print(f"{n:>4}{len(allowed):>7}{med:>11.2f}{sp:>8.2f}x{eff:>8.0%}{pi:>10.5f}")
        print()

    for fname, rows in (("results_raw.csv", raw), ("results_summary.csv", summary)):
        with open(fname, "w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=list(rows[0]))
            w.writeheader()
            w.writerows(rows)
    plot(summary, max_nodes)
    print("Saved: results_raw.csv, results_summary.csv, scaling.png")


def plot(summary: list[dict], max_nodes: int) -> None:
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    xs = list(range(1, max_nodes + 1))
    fig, (a1, a2) = plt.subplots(1, 2, figsize=(11, 4.2))
    a1.plot(xs, xs, "--", color="gray", label="ideal (linear)")
    for wl in sorted({r["workload"] for r in summary}):
        rows = [r for r in summary if r["workload"] == wl]
        lbl = f"{wl:,} samples"
        a1.plot([r["pcs"] for r in rows], [r["speedup"] for r in rows], "o-", label=lbl)
        a2.plot([r["pcs"] for r in rows], [r["efficiency"] * 100 for r in rows], "o-", label=lbl)
    a1.set(title="Speedup vs number of PCs", xlabel="PCs", ylabel="speedup (T1 / Tn)", xticks=xs)
    a2.set(title="Parallel efficiency", xlabel="PCs", ylabel="efficiency (%)", xticks=xs, ylim=(0, 110))
    a2.axhline(100, ls="--", color="gray")
    for a in (a1, a2):
        a.grid(alpha=0.3)
        a.legend()
    fig.tight_layout()
    fig.savefig("scaling.png", dpi=150)


def cmd_fwrun(args) -> None:
    """Open ports, run another engine's program (e.g. the Rust `ck`), close ports when it exits."""
    cmd = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not cmd:
        sys.exit("fwrun: give the program to run after --")
    rules = [(proto, p.strip().replace("-", ":")) for proto, ports in (("tcp", args.tcp), ("udp", args.udp))
             for p in ports.split(",") if p.strip()]
    exe = shutil.which(cmd[0]) or os.path.abspath(cmd[0])
    setup_firewall(args, rules, primary_ip(), args.role, (exe,))
    sys.exit(spawn(cmd).wait())


def cmd_fw_clean(args) -> None:
    global _firewall
    _firewall = Firewall(True, [], "0.0.0.0/0", "clean")
    if _firewall.backend:
        _firewall.remove_tagged(RULE_TAG)
        log("Firewall: all daskcluster rules removed")
    if _firewall.sh:
        _firewall.sh.close()
    _firewall = None


# --------------------------------------------------------------------------- #
def main() -> None:
    common = argparse.ArgumentParser(add_help=False)  # accepted after any subcommand
    common.add_argument("--name", default="lab", help="cluster name (lets several clusters share a LAN)")
    common.add_argument("--no-firewall", action="store_true", help="don't touch firewall rules")
    ap = argparse.ArgumentParser(description="4-PC Dask cluster toolkit")
    sub = ap.add_subparsers(dest="cmd", required=True)

    def add(name: str, **kw) -> argparse.ArgumentParser:
        return sub.add_parser(name, parents=[common], **kw)

    h = add("head", help="run scheduler + beacon (+ local worker)")
    h.add_argument("--no-local-worker", action="store_true", help="head only schedules, no compute")
    h.add_argument("--procs", type=int, help="worker processes on this PC (default: physical cores)")

    w = add("worker", help="auto-discover the head and join")
    w.add_argument("--head", help="skip discovery and use this head IP")
    w.add_argument("--procs", type=int, help="worker processes on this PC (default: physical cores)")

    for name in ("status", "bench"):
        p = add(name)
        p.add_argument("--scheduler", default=f"tcp://127.0.0.1:{SCHED_PORT}")
    b = sub.choices["bench"]
    b.add_argument("--total", nargs="+", type=float, default=[2e7, 1e8],
                   help="workload sizes in samples (default: 2e7 1e8)")
    b.add_argument("--chunks", type=int, help="tasks per run (default: 4 x total procs)")
    b.add_argument("--repeats", type=int, default=3)
    b.add_argument("--max-nodes", type=int, help="limit the scaling test to N PCs")

    add("fw-clean", help="remove leftover firewall rules")

    f = add("fwrun", help="open ports, run a program, close ports when it exits")
    f.add_argument("--role", required=True, help="rule tag, e.g. ck-head")
    f.add_argument("--tcp", default="", help="comma-separated ports or ranges, e.g. 7700,9000-9100")
    f.add_argument("--udp", default="")
    f.add_argument("command", nargs=argparse.REMAINDER, help="-- program args...")

    args = ap.parse_args()
    long_running = args.cmd in ("head", "worker", "fwrun")
    if long_running:
        install_exit_handlers()
    try:
        {"head": cmd_head, "worker": cmd_worker, "status": cmd_status, "bench": cmd_bench,
         "fw-clean": cmd_fw_clean, "fwrun": cmd_fwrun}[args.cmd](args)
    except KeyboardInterrupt:
        pass
    finally:
        if long_running:
            run_cleanup("stopped")


if __name__ == "__main__":
    main()
