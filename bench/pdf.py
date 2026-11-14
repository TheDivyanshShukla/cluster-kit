"""PDF report for an experiment.py run (run bench/report.py first for scaling.png).

  python3 bench/pdf.py bench/<run-dir> [--head http://127.0.0.1:7702]

Writes <run-dir>/report.html and <run-dir>/report.pdf (printed by headless Chrome).
"""
import base64, collections, csv, html, json, pathlib, statistics, subprocess, sys, time, urllib.request

d = pathlib.Path(sys.argv[1])
head = sys.argv[sys.argv.index("--head") + 1] if "--head" in sys.argv else "http://127.0.0.1:7702"
rows = list(csv.DictReader(open(d / "raw.csv")))
runs = [json.loads(l) for l in open(d / "runs.jsonl")]

by, secs = collections.defaultdict(list), collections.defaultdict(list)
for r in rows:
    by[int(r["pcs"])].append(float(r["hashes_per_s"]) / 1e6)
    secs[int(r["pcs"])].append(float(r["seconds"]))
pcs = sorted(by)
med = {p: statistics.median(by[p]) for p in pcs}
one = med[pcs[0]] / pcs[0]
top = pcs[-1]

# hardware of the PCs that took part (from the head; skipped if it is not reachable)
used = {pc["node_id"] for r in runs for pc in r["pcs"]}
hw = collections.Counter()
try:
    for n in json.load(urllib.request.urlopen(head + "/api/v1/nodes", timeout=5))["nodes"]:
        if n["node_id"] in used:
            i = n.get("inventory") or {}
            hw[(i.get("cpu_model", "?"), i.get("cores_physical"), i.get("cores_logical"), round((i.get("ram_bytes") or 0) / 2**30), i.get("os", "?"))] += 1
except Exception as e:
    print("hardware details skipped:", e)

r0 = runs[0]
when = time.strftime("%d %B %Y, %H:%M", time.localtime(min(r["wall_start"] for r in runs)))
dur = (max(r["finished_ms"] for r in runs) - min(r["started_ms"] for r in runs)) / 60000
per_pc = r0["n"] // int(r0["config"].split()[0]) if r0["config"].split()[0].isdigit() else r0["n"]
png = base64.b64encode((d / "scaling.png").read_bytes()).decode()
e = html.escape
hf = json.loads((d / "heff.json").read_text()) if (d / "heff.json").exists() else None  # written by report.py --head
heff = {int(k): v for k, v in hf["eff"].items()} if hf else {}
tail = {int(k): v for k, v in hf["tail"].items()} if hf else {}
geff = {int(k): v for k, v in hf.get("group", {}).items()} if hf else {}      # vs the sum of each hardware group's speed
gideal = {int(k): v for k, v in hf.get("group_ideal", {}).items()} if hf else {}
groups = hf.get("groups", {}) if hf else {}                                    # group -> [PCs, M/s each]

table = "".join(
    f"<tr><td>{p}</td><td>{med[p]:.1f}</td><td>{min(by[p]):.1f} – {max(by[p]):.1f}</td><td>{one * p:.1f}</td>"
    f"<td>{100 * med[p] / (one * p):.0f}%</td>" + (f"<td>{gideal[p]:.1f}</td><td><b>{geff[p]:.1f}%</b></td>" if geff else "") +
    (f"<td>{heff[p]:.1f}%</td><td>{tail[p]:.1f}%</td>" if heff else "") +
    f"<td>{med[p] / one:.1f}×</td><td>{statistics.median(secs[p]):.2f}</td></tr>" for p in pcs)
hwrows = "".join(f"<tr><td>{c}</td><td>{e(m)}</td><td>{ph} / {lg}</td><td>{ram} GiB</td><td>{e(o)}</td></tr>"
                 for (m, ph, lg, ram, o), c in hw.most_common()) or "<tr><td colspan=5>not available</td></tr>"
runrows = "".join(f"<tr><td>{r['repeat']}</td><td>{r['config']}</td><td>{r['job']}</td><td>{r['n'] / 1e9:.1f} G</td>"
                  f"<td>{r['seconds']:.3f}</td><td>{r['n'] / r['seconds'] / 1e6:.1f}</td></tr>" for r in runs)
eff_top = geff.get(top, heff.get(top, 100 * med[top] / (one * top)))
eff_naive = 100 * med[top] / (one * top)
models = sorted({m for (m, *_rest) in hw})
overhead = statistics.median(secs[top]) - statistics.median(secs[pcs[0]])

page = f"""<!doctype html><meta charset=utf-8><title>Cluster Kit benchmark</title>
<style>
@page {{ size: A4; margin: 16mm 15mm; }}
body {{ font: 10pt/1.45 -apple-system, "Helvetica Neue", Arial, sans-serif; color: #1f2328; }}
h1 {{ font-size: 18pt; margin: 0 0 2mm; }} h2 {{ font-size: 12pt; margin: 7mm 0 2mm; border-bottom: 1px solid #d0d7de; padding-bottom: 1mm; }}
.sub {{ color: #57606a; margin-bottom: 5mm; }}
.kpis {{ display: flex; gap: 4mm; margin: 4mm 0; }} .kpi {{ flex: 1; border: 1px solid #d0d7de; border-radius: 2mm; padding: 3mm; }}
.kpi b {{ display: block; font-size: 16pt; }} .kpi span {{ color: #57606a; font-size: 8.5pt; }}
table {{ border-collapse: collapse; width: 100%; font-size: 9pt; }} th, td {{ padding: 1.2mm 2mm; border-bottom: 1px solid #eaeef2; text-align: right; }}
th {{ background: #f6f8fa; }} td:first-child, th:first-child {{ text-align: left; }} .left td, .left th {{ text-align: left; }}
img {{ width: 100%; }} .break {{ break-before: page; }} .note {{ color: #57606a; font-size: 8.5pt; }}
</style>
<h1>Cluster Kit benchmark report</h1>
<div class=sub>Weak scaling, SHA-256 search · {when} · {len(runs)} recorded runs in {dur:.0f} min · run <code>{e(d.name)}</code></div>
<div class=kpis>
<div class=kpi><b>{med[top]:.1f} M/s</b><span>throughput with all {top} PCs</span></div>
<div class=kpi><b>{med[top] / one:.1f}×</b><span>speed-up over one PC</span></div>
<div class=kpi><b>{eff_top:.1f}%</b><span>{'of what this hardware can do' if geff else 'scaling efficiency'} at {top} PCs</span></div>
<div class=kpi><b>{one:.1f} M/s</b><span>one PC on its own</span></div>
</div>

<h2>Summary</h2>
<p>Every PC was given the same amount of work ({per_pc / 1e9:.1f} billion SHA-256 hashes), and the number of PCs grew from {pcs[0]} to {top}.
With perfect scaling the run time stays flat and the throughput rises in a straight line. The cluster reached {med[top]:.1f} M hashes/s on {top} PCs.
{f'''That is <b>{eff_top:.1f}%</b> of what this hardware can do: the ideal adds up every PC at the speed of its own hardware group
({"; ".join(f"{n} × {e(g)} at {v:.1f} M/s" for g, (n, v) in groups.items())}), {gideal[top]:.0f} M hashes/s in total.
A group is one CPU model with one thread count; its speed is the median compute rate of its PCs in the {top}-PC runs.
The head's own time accounting agrees: the PCs spent {heff[top]:.1f}% of the job computing, {tail[top]:.1f}% idle at the end of the job.
Compared with {top} copies of the first PC the figure is {eff_naive:.0f}%; that number mostly says the other PCs are slower models, not that work was lost.''' if geff else
f'''That is {eff_top:.0f}% of {top} × the single-PC rate.'''}
Going from {pcs[0]} PC to {top} PCs added {overhead:.2f} s per run ({100 * overhead / statistics.median(secs[pcs[0]]):.1f}%).
Efficiency figures slightly above 100% are measurement noise: the single-PC reference itself varied between {min(by[pcs[0]]):.1f} and {max(by[pcs[0]]):.1f} M/s.</p>

<h2>Results</h2>
<table><tr><th>PCs</th><th>median M/s</th><th>range M/s</th><th>N × one PC</th><th>vs N × one PC</th>{'<th>ideal for this hardware</th><th>hardware-group eff.</th>' if geff else ''}{'<th>head-measured</th><th>tail idle</th>' if heff else ''}<th>speed-up</th><th>median s</th></tr>{table}</table>
<p class=note>Median of {len(by[top])} repeats per size. "N × one PC" = number of PCs × the single-PC median.{' Ideal for this hardware = sum over the PCs in the run of their hardware group’s speed; hardware-group efficiency = measured ÷ that.' if geff else ''}{' Head-measured = compute time ÷ (PCs × wall time), from the head’s per-job time breakdown; tail idle = PCs waiting for the last chunks.' if heff else ''}</p>

<h2>Charts</h2>
<img src="data:image/png;base64,{png}">
<p class=note>Left: measured throughput against the ideal line (bars show min–max). Middle: efficiency per size. Right: run time per size; flat means no coordination cost.</p>

<h2 class=break>Setup</h2>
<table class=left>
<tr><th>Workload</th><td>SHA-256 prefix search ({e(r0['kind'])}), {per_pc / 1e9:.1f} G hashes per PC per run</td></tr>
<tr><th>Scheduling</th><td>{e(r0['policy'])} policy, chunk size {r0['chunk'] or 'chosen by the policy'}</td></tr>
<tr><th>Method</th><td>1 unrecorded warm-up per size, then {len(by[top])} repeats, interleaved (each repeat runs every size once); the same PCs in the same order every time</td></tr>
<tr><th>Head</th><td>{e(r0['head'])}, open mode (CK_FLEET=open, CK_AUTH=off), no local worker</td></tr>
<tr><th>Software</th><td>Cluster Kit commit {e(r0['commit'])}{' (with uncommitted changes)' if r0.get('dirty') else ''}</td></tr>
<tr><th>Virtual PCs</th><td>{'yes' if r0['virtual'] else 'none, all real lab machines'}</td></tr>
</table>

<h2>Hardware</h2>
<table class=left><tr><th>PCs</th><th>CPU</th><th>cores (physical / logical)</th><th>RAM</th><th>OS</th></tr>{hwrows}</table>

<h2>Every run</h2>
<table><tr><th>repeat</th><th>size</th><th>job</th><th>hashes</th><th>seconds</th><th>M/s</th></tr>{runrows}</table>
"""
(d / "report.html").write_text(page)
chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
subprocess.run([chrome, "--headless", "--disable-gpu", "--no-pdf-header-footer", f"--print-to-pdf={(d / 'report.pdf').resolve()}",
                (d / "report.html").resolve().as_uri()], check=True, capture_output=True)
print(d / "report.pdf")
