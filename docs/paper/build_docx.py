"""Build the beginner-friendly Word report: python build_docx.py  ->  Cluster_Kit_Project_Report.docx"""
import csv, pathlib, statistics as st
import matplotlib
matplotlib.use("agg")
import matplotlib.pyplot as plt
from matplotlib.patches import FancyBboxPatch
from docx import Document
from docx.enum.table import WD_TABLE_ALIGNMENT
from docx.enum.text import WD_ALIGN_PARAGRAPH
from docx.oxml import OxmlElement
from docx.oxml.ns import qn
from docx.shared import Cm, Pt, RGBColor

HERE = pathlib.Path(__file__).parent
OUT = HERE.parent.parent / "Cluster_Kit_Project_Report.docx"
FIG = HERE / "docx_fig"; FIG.mkdir(exist_ok=True)

# ---------- data (only measured values) ----------
rows = list(csv.DictReader(open(HERE / "data/raw.csv")))
by = {}
for r in rows:
    by.setdefault(r["config"], []).append(int(r["n"]) / float(r["seconds"]) / 1e6)
med = {k: st.median(v) for k, v in by.items()}
S = ["head", "192.168.10.200", "192.168.10.3", "192.168.10.5"]
M = ["head", "2 PCs", "3 PCs", "4 PCs"]
X = [med[c] for c in M]
avg1 = st.mean(med[s] for s in S)
ideal = [sum(med[s] for s in S[:k]) for k in range(1, 5)]
SPEED = [x / avg1 for x in X]
EFF = [x / i * 100 for x, i in zip(X, ideal)]
WIN_T, CL_T, LIVE_T = 36.98, 2.165, 3.87

BLUE, TEAL, ORANGE, PURPLE, RED, GREY, INK = "#1F3864", "#2E75B6", "#BF9000", "#548235", "#7F7F7F", "#A6A6A6", "#262626"
plt.rcParams.update({"font.family": "Arial", "font.size": 10.5, "axes.spines.top": False, "axes.spines.right": False,
                     "axes.grid": True, "grid.color": "#E7E6E6", "axes.axisbelow": True, "axes.edgecolor": "#A6A6A6"})

def save(fig, name):
    p = FIG / f"{name}.png"; fig.tight_layout(); fig.savefig(p, dpi=200); plt.close(fig); return p

# Windows laptop vs cluster
fig, (a, b) = plt.subplots(1, 2, figsize=(9, 3.4), gridspec_kw={"width_ratios": [1.6, 1]})
a.barh(["Windows laptop", "4-PC Ubuntu cluster"], [WIN_T, CL_T], color=[GREY, BLUE], height=0.55)
for i, v in enumerate([WIN_T, CL_T]):
    a.text(v + 0.6, i, f"{v} s", va="center", fontweight="bold")
a.invert_yaxis(); a.set_xlabel("execution time (seconds) - lower is better"); a.set_xlim(0, 44)
a.set_title("Execution time (same program, 766 matches)", fontsize=11)
b.bar(["speed-up"], [WIN_T / CL_T], color=TEAL, width=0.5)
b.text(0, WIN_T / CL_T + 0.4, f"{WIN_T/CL_T:.1f}x faster", ha="center", fontweight="bold")
b.text(0, WIN_T / CL_T / 2, f"{(1-CL_T/WIN_T)*100:.1f}%\nless time", ha="center", color="white", fontweight="bold")
b.set_ylim(0, 20); b.set_title("Speed-up", fontsize=11)
f_laptop = save(fig, "laptop_vs_cluster")

# throughput scaling
fig, ax = plt.subplots(figsize=(9, 3.5))
cols = [GREY, BLUE, TEAL, PURPLE]
ax.bar(range(1, 5), X, color=cols, width=0.6, label="measured")
ax.plot(range(1, 5), ideal, "o--", color=ORANGE, lw=2, label="ideal (perfect sharing)")
for i, x in enumerate(X):
    ax.text(i + 1, x / 2, f"{x:.1f}", ha="center", color="white", fontweight="bold", fontsize=12)
ax.set_xticks(range(1, 5), ["1 PC", "2 PCs", "3 PCs", "4 PCs"]); ax.set_ylabel("million hashes / second")
ax.set_title("Throughput vs number of PCs", fontsize=12); ax.legend(frameon=False)
f_tp = save(fig, "throughput")

# speed-up + efficiency
fig, (a, b) = plt.subplots(1, 2, figsize=(9, 3.3))
a.plot([1, 4], [1, 4], ":", color=GREY, label="perfect (linear)")
a.plot(range(1, 5), SPEED, "o-", color=BLUE, lw=2.5, ms=8, label="our cluster")
for i, s in enumerate(SPEED):
    a.annotate(f"{s:.2f}x", (i + 1, s), textcoords="offset points", xytext=(-14, 9), fontweight="bold", fontsize=10)
a.set_xticks(range(1, 5)); a.set_xlabel("number of PCs"); a.set_ylabel("speed-up"); a.legend(frameon=False, loc="upper left")
a.set_title("Speed-up (vs average single PC)", fontsize=11)
b.bar(range(1, 5), EFF, color=[TEAL if e >= 99 else ORANGE for e in EFF], width=0.6)
for i, e in enumerate(EFF):
    b.text(i + 1, e + 0.5, f"{e:.1f}%", ha="center", fontweight="bold", fontsize=10)
b.set_ylim(80, 106); b.set_xticks(range(1, 5)); b.set_xlabel("number of PCs"); b.set_ylabel("efficiency (%)")
b.set_title("Parallel efficiency", fontsize=11)
f_sp = save(fig, "speedup")

# per-PC work share
fig, (a, b) = plt.subplots(1, 2, figsize=(9, 3.2), gridspec_kw={"width_ratios": [1, 1.4]})
share = [23.9, 25.7, 25.2, 25.2]; names = ["PC 1 (head)", "PC 2", "PC 3", "PC 4"]
a.pie(share, labels=[f"{n}\n{s}%" for n, s in zip(names, share)], colors=[BLUE, TEAL, ORANGE, PURPLE],
      startangle=90, wedgeprops={"width": 0.45, "edgecolor": "white"}, textprops={"fontsize": 9})
a.set_title("Work share (4-PC run)", fontsize=11)
busy = [98.5, 97.1, 95.8, 95.7]
b.barh(names, busy, color=[BLUE, TEAL, ORANGE, PURPLE], height=0.55)
for i, v in enumerate(busy):
    b.text(v - 1, i, f"{v}%", va="center", ha="right", color="white", fontweight="bold")
b.set_xlim(0, 100); b.invert_yaxis(); b.set_xlabel("time spent computing (%)"); b.set_title("Time spent computing", fontsize=11)
f_share = save(fig, "share")

def box(ax, x, y, w, h, text, color, fc="white", bold=False, fs=10):
    ax.add_patch(FancyBboxPatch((x, y), w, h, boxstyle="round,pad=0.02,rounding_size=0.08", ec=color, fc=fc, lw=2))
    ax.text(x + w / 2, y + h / 2, text, ha="center", va="center", fontsize=fs, color=INK, fontweight="bold" if bold else "normal")

def arrow(ax, x1, y1, x2, y2, color=INK, text="", dy=0.12):
    ax.annotate("", (x2, y2), (x1, y1), arrowprops={"arrowstyle": "-|>", "color": color, "lw": 1.8})
    if text: ax.text((x1 + x2) / 2, (y1 + y2) / 2 + dy, text, ha="center", fontsize=8.5, color=color)

# architecture
fig, ax = plt.subplots(figsize=(9, 3.9)); ax.set_xlim(0, 10); ax.set_ylim(-0.25, 4.15); ax.axis("off")
box(ax, 0.1, 1.6, 1.8, 0.9, "User / client\nck run", GREY)
box(ax, 3.0, 1.2, 2.6, 1.7, "HEAD  (PC 1)\nsplits job into chunks\nsends chunks, collects results\nre-sends lost chunks", BLUE, "#DBEAFE", True, 9.5)
for i, (y, c) in enumerate(zip([3.0, 1.75, 0.5], [TEAL, ORANGE, PURPLE])):
    box(ax, 7.2, y, 2.6, 0.75, f"WORKER  PC {i+2}\n12 CPU threads", c, bold=True, fs=9.5)
    arrow(ax, 5.6, 2.05, 7.2, y + 0.37, c)
arrow(ax, 1.9, 2.05, 3.0, 2.05, INK, "job")
box(ax, 3.3, 0.0, 2.0, 0.7, "local worker\non PC 1", BLUE, fs=9)
arrow(ax, 4.3, 1.2, 4.3, 0.7, BLUE)
ax.text(8.5, 3.95, "UDP 7701: \"I am the head\" broadcast", ha="center", fontsize=9, color=RED, fontweight="bold")
ax.text(8.5, 0.1, "TCP 7700: chunks and results", ha="center", fontsize=9, color=BLUE, fontweight="bold")
f_arch = save(fig, "architecture")

# job flowchart
fig, ax = plt.subplots(figsize=(9, 2.2)); ax.set_xlim(0, 10); ax.set_ylim(0, 2); ax.axis("off")
steps = [("1. Discover", "workers hear the\nUDP broadcast", RED), ("2. Connect", "one TCP link\nper worker", BLUE),
         ("3. Split", "head cuts job\ninto chunks", TEAL), ("4. Compute", "each worker uses\nall its cores", ORANGE),
         ("5. Combine", "head adds up\nall results", PURPLE)]
for i, (t, d, c) in enumerate(steps):
    x = 0.1 + i * 2.0
    box(ax, x, 0.3, 1.7, 1.4, "", c, fc="white")
    ax.text(x + 0.85, 1.35, t, ha="center", fontweight="bold", color=c, fontsize=10.5)
    ax.text(x + 0.85, 0.75, d, ha="center", fontsize=8.8, color=INK)
    if i < 4: arrow(ax, x + 1.7, 1.0, x + 2.0, 1.0, INK)
f_flow = save(fig, "flow")

# ---------- document ----------
doc = Document()
sec = doc.sections[0]
sec.page_width, sec.page_height = Cm(21), Cm(29.7)
for m in ("left_margin", "right_margin"): setattr(sec, m, Cm(2))
sec.top_margin = sec.bottom_margin = Cm(1.8)

base = doc.styles["Normal"]; base.font.name = "Calibri"; base.font.size = Pt(11)
base.element.rPr.rFonts.set(qn("w:eastAsia"), "Calibri")
base.paragraph_format.space_after = Pt(4); base.paragraph_format.line_spacing = 1.12
for lvl, size, color in ((1, 14, BLUE), (2, 12, TEAL)):
    s = doc.styles[f"Heading {lvl}"]; s.font.name = "Cambria"; s.font.size = Pt(size); s.font.bold = True
    s.font.color.rgb = RGBColor.from_string(color[1:]); s.element.rPr.rFonts.set(qn("w:ascii"), "Cambria"); s.element.rPr.rFonts.set(qn("w:hAnsi"), "Cambria")
    s.paragraph_format.space_before = Pt(12 if lvl == 1 else 8); s.paragraph_format.space_after = Pt(4)
    s.paragraph_format.keep_with_next = True

def shade(cell, hex_):
    tcPr = cell._tc.get_or_add_tcPr(); sh = OxmlElement("w:shd")
    sh.set(qn("w:val"), "clear"); sh.set(qn("w:color"), "auto"); sh.set(qn("w:fill"), hex_.lstrip("#")); tcPr.append(sh)

def para(text="", bold_lead=None, size=None, color=None, align=None, italic=False, after=None):
    p = doc.add_paragraph()
    if bold_lead:
        r = p.add_run(bold_lead); r.bold = True
        if color: r.font.color.rgb = RGBColor.from_string(color[1:])
    r = p.add_run(text); r.italic = italic
    if size: r.font.size = Pt(size)
    if align: p.alignment = align
    if after is not None: p.paragraph_format.space_after = Pt(after)
    return p

def bullets(items):
    for it in items:
        p = doc.add_paragraph(style="List Bullet"); p.paragraph_format.space_after = Pt(1)
        if isinstance(it, tuple):
            r = p.add_run(it[0]); r.bold = True; p.add_run(it[1])
        else:
            p.add_run(it)

def table(head, body, color=BLUE, widths=None, zebra="#F2F2F2", size=9.5, bold_first=False):
    t = doc.add_table(rows=1, cols=len(head)); t.style = "Table Grid"; t.alignment = WD_TABLE_ALIGNMENT.CENTER
    for i, h in enumerate(head):
        c = t.rows[0].cells[i]; c.text = ""; r = c.paragraphs[0].add_run(h); r.bold = True
        r.font.color.rgb = RGBColor(255, 255, 255); r.font.size = Pt(size); shade(c, color)
    for k, row in enumerate(body):
        cells = t.add_row().cells
        for i, v in enumerate(row):
            cells[i].text = ""; r = cells[i].paragraphs[0].add_run(str(v)); r.font.size = Pt(size)
            if bold_first and i == 0: r.bold = True
            if k % 2: shade(cells[i], zebra)
    finish(t, widths)
    doc.add_paragraph().paragraph_format.space_after = Pt(2)
    return t

def finish(t, widths=None):
    """Fixed column widths, light borders, rows that never split, whole table kept on one page."""
    t.autofit = False
    if widths:
        grid = t._tbl.tblGrid
        for i, gc in enumerate(grid.findall(qn("w:gridCol"))): gc.set(qn("w:w"), str(int(widths[i] * 567)))
        for row in t.rows:
            for i, w in enumerate(widths): row.cells[i].width = Cm(w)
    borders = OxmlElement("w:tblBorders")
    for side in ("top", "left", "bottom", "right", "insideH", "insideV"):
        e = OxmlElement(f"w:{side}"); e.set(qn("w:val"), "single"); e.set(qn("w:sz"), "4"); e.set(qn("w:color"), "BFBFBF"); borders.append(e)
    t._tbl.tblPr.append(borders)
    mar = OxmlElement("w:tblCellMar")
    for side, v in (("top", 45), ("bottom", 45), ("left", 90), ("right", 90)):
        e = OxmlElement(f"w:{side}"); e.set(qn("w:w"), str(v)); e.set(qn("w:type"), "dxa"); mar.append(e)
    t._tbl.tblPr.append(mar)
    for k, row in enumerate(t.rows):
        trPr = row._tr.get_or_add_trPr(); cs = OxmlElement("w:cantSplit"); trPr.append(cs)
        for c in row.cells:
            for p in c.paragraphs:
                p.paragraph_format.space_after = Pt(1)
                if k < len(t.rows) - 1: p.paragraph_format.keep_with_next = True

def image(path, width=17, caption=None):
    doc.add_picture(str(path), width=Cm(width)); doc.paragraphs[-1].alignment = WD_ALIGN_PARAGRAPH.CENTER
    if caption:
        p = para(caption, size=9, italic=True, align=WD_ALIGN_PARAGRAPH.CENTER, after=6)
        p.runs[0].font.color.rgb = RGBColor(0x64, 0x74, 0x8B)

def callout(title, text, color=TEAL, fill="#F2F6FC"):
    t = doc.add_table(rows=1, cols=1); t.alignment = WD_TABLE_ALIGNMENT.CENTER
    c = t.rows[0].cells[0]; shade(c, fill); c.text = ""
    p = c.paragraphs[0]; r = p.add_run(title + "  "); r.bold = True; r.font.color.rgb = RGBColor.from_string(color[1:])
    p.add_run(text)
    finish(t, [17])
    doc.add_paragraph().paragraph_format.space_after = Pt(2)

# Cover
p = para("PROJECT REPORT", size=10, color=GREY, align=WD_ALIGN_PARAGRAPH.CENTER, after=2)
p.runs[0].font.color.rgb = RGBColor(0x64, 0x74, 0x8B)
p = doc.add_paragraph(); p.alignment = WD_ALIGN_PARAGRAPH.CENTER
r = p.add_run("Cluster Kit"); r.bold = True; r.font.size = Pt(28); r.font.name = "Cambria"; r.font.color.rgb = RGBColor.from_string(BLUE[1:])
p = doc.add_paragraph(); p.alignment = WD_ALIGN_PARAGRAPH.CENTER
r = p.add_run("Supercomputing, Built from Lab Desktops"); r.font.size = Pt(14); r.font.name = "Cambria"; r.font.color.rgb = RGBColor.from_string(TEAL[1:]); r.italic = True
para("Divyansh Shukla  ·  Abhinav Dhoke  ·  Sneha Kumari  ·  Vanshika Wadhwa  ·  Ankush Kumar",
     size=10.5, align=WD_ALIGN_PARAGRAPH.CENTER, after=0)
para("Sagar Institute of Science and Technology (SISTec), Gandhi Nagar, Bhopal  ·  October 2026", size=9.5, align=WD_ALIGN_PARAGRAPH.CENTER, after=8)

t = doc.add_table(rows=2, cols=4); t.alignment = WD_TABLE_ALIGNMENT.CENTER
kpis = [("17.1x", "faster than a\nWindows laptop", BLUE), ("3.86x", "speed-up with\n4 PCs (controlled)", TEAL),
        ("96.4%", "parallel\nefficiency", BLUE), ("48", "CPU threads\non 4 PCs", TEAL)]
for i, (v, lbl, c) in enumerate(kpis):
    top, bot = t.rows[0].cells[i], t.rows[1].cells[i]
    for cell in (top, bot): shade(cell, c)
    top.text = ""; r = top.paragraphs[0].add_run(v); r.bold = True; r.font.size = Pt(20); r.font.color.rgb = RGBColor(255, 255, 255)
    bot.text = ""; r = bot.paragraphs[0].add_run(lbl); r.font.size = Pt(8.5); r.font.color.rgb = RGBColor(255, 255, 255)
    top.paragraphs[0].alignment = bot.paragraphs[0].alignment = WD_ALIGN_PARAGRAPH.CENTER
finish(t, [4.25] * 4)
doc.add_paragraph().paragraph_format.space_after = Pt(2)

doc.add_heading("Abstract", 1)
para("Cluster Kit joins ordinary lab PCs into one small cluster. One PC (the head) splits a big job into small pieces "
     "and sends them to every PC over the network; each PC solves its pieces on all its cores and sends the answer back. "
     "PCs find each other automatically using UDP, so no setup is needed. On four identical Ubuntu lab PCs, a SHA-256 "
     "search ran 3.86 times faster than on one PC, with 96.4% efficiency. The same Python program that took 36.98 s on a "
     "Windows laptop finished in 2.165 s on the cluster.")

# 1 Intro
doc.add_heading("1. Introduction", 1)
para("Supercomputers are many computers working together on one problem. Our labs already have many PCs that sit idle "
     "for most of the day. This project shows that the same idea works on ordinary lab desktops: connect them, split the "
     "work, and finish much sooner.")
para("", "Objectives:  ", color=BLUE, after=1)
bullets(["Join several lab PCs into one cluster with almost no setup.",
         "Split a heavy computation across all PCs automatically.",
         "Measure the real speed gain fairly and explain it clearly."])

# 2 Concepts
doc.add_heading("2. Key Concepts (in short)", 1)
table(["Term", "Simple meaning"], [
    ["HPC", "High-Performance Computing: using many processors together to solve big problems faster."],
    ["Cluster", "A group of computers on a network that act like one bigger computer."],
    ["Head / Worker", "The head gives out work and collects answers; workers do the computing."],
    ["Speed-up", "How many times faster: time on 1 PC ÷ time on many PCs."],
    ["Efficiency", "Speed-up ÷ number of PCs. 100% means no time is wasted."],
    ["SHA-256", "A hash function. Our test checks millions of numbers to find hashes that start with zeros."],
    ["MPI", "The standard tool used on real supercomputers to make programs run on many computers."],
], BLUE, [3.2, 13.8], bold_first=True)

table(["", "Single PC", "Cluster Kit (4 PCs)"], [
    ["Computers", "1", "4 (can add more)"],
    ["CPU threads", "12", "48"],
    ["Network needed", "No", "Yes (LAN)"],
    ["If one PC fails", "Job stops", "Its work is given to other PCs"],
    ["Best for", "Small jobs", "Big jobs that split into pieces"],
], TEAL, [4, 6, 7], bold_first=True)

# 3 Networking
doc.add_heading("3. Networking: UDP + TCP", 1)
para("We use two protocols, each for what it does best:")
table(["Protocol", "Port", "Used for", "Why this protocol"], [
    ["UDP", "7701", "Finding the head", "One broadcast reaches every PC at once. No IP addresses to type. A lost message is fine; the next one comes soon."],
    ["TCP", "7700", "Sending work and results", "Guarantees every piece of work and every answer arrives, in order."],
    ["HTTP", "7702", "Live dashboard", "View workers, CPU use and progress in a browser."],
], RED, [2, 1.5, 4, 9.5], bold_first=True)

# 4 Architecture
doc.add_heading("4. System Architecture", 1)
image(f_arch, 16.5, "Figure 1. The head (PC 1) shares chunks of work with three worker PCs and its own local worker.")
doc.add_heading("How one job runs", 2)
image(f_flow, 16.5, "Figure 2. Step by step: from finding the head to combining the final answer.")
callout("Smart scheduling:", "A worker asks for the next chunk as soon as it finishes one, and always keeps one chunk "
        "waiting in line. So fast PCs automatically do more work, and no PC sits idle waiting for the network.", BLUE, "#F2F6FC")

# 5 Implementation
doc.add_heading("5. Implementation", 1)
table(["Part", "Technology", "What it does"], [
    ["Cluster engine (ck)", "Rust", "Head, worker and client in one small program; fast and safe."],
    ["Multi-core use", "Rayon (Rust library)", "Splits each chunk over all CPU cores of a PC."],
    ["Networking", "UDP + TCP sockets", "Auto-discovery and reliable data transfer (own small binary format)."],
    ["Example programs", "Python", "simple_sha256.py and others run on the cluster unchanged."],
    ["Python engine", "Dask", "A second, Python-based way to run jobs on the cluster."],
    ["Dashboard", "HTML / HTTP", "Live view of workers, chunks and CPU use."],
    ["Operating system", "Ubuntu Linux (also Windows, macOS)", "The engine is one portable program."],
], PURPLE, [4, 4.5, 8.5], bold_first=True)
bullets([("Fault tolerance: ", "if a worker disconnects, the head gives its unfinished chunks to other workers."),
         ("Send the script: ", "the head can send a script to workers that don't have it (no shared folder needed)."),
         ("One command per PC: ", "start the head on one PC and a worker on each other PC; they find each other.")])

# 6 Setup
doc.add_heading("6. Test Setup", 1)
table(["Item", "Details"], [
    ["Computers", "4 identical lab desktops: PC 1 = head (192.168.10.122), PC 2-4 = workers (.200, .3, .5)"],
    ["Each PC", "12 CPU threads, 7.1 GB RAM, Ubuntu 26.04.1 LTS"],
    ["Network", "Wired LAN (ping under 1 ms)"],
    ["Test", "SHA-256 of every number from 1 to n; count hashes starting with the required zeros"],
    ["Fairness", "Each setup run 7 times in rotation, after one warm-up run"],
], TEAL, [3.2, 13.8], bold_first=True)
image(HERE / "img/testbed.jpg", 13, "Figure 3. Our testbed in the lab: PC 1 (\"Master\") is the head; PCs 2-4 are workers.")

# 7 Results
doc.add_heading("7. Results", 1)
doc.add_heading("7.1  Windows laptop vs our Ubuntu cluster", 2)
para("We ran the same program (simple_sha256.py) on a normal Windows laptop and on the 4-PC cluster. Both runs found "
     "the same 766 matching hashes, so both did the same work.")
image(f_laptop, 15, "Figure 4. Execution time of the same job on a Windows laptop and on the 4-PC cluster.")
table(["Measurement", "Windows laptop", "4-PC Ubuntu cluster", "Difference"], [
    ["Execution time", f"{WIN_T} s", f"{CL_T} s", f"{WIN_T-CL_T:.3f} s saved"],
    ["Matching hashes", "766", "766", "same answer"],
    ["Speed-up", "1x", f"{WIN_T/CL_T:.2f}x", f"{(1-CL_T/WIN_T)*100:.1f}% less time"],
], RED, [4, 4, 4.5, 4.5], bold_first=True)
callout("Why 17x and not 4x?", "The laptop and the cluster are different machines, and the laptop's run was a single "
        "measurement (whether it used one core or all cores was not recorded). The cluster used 48 CPU threads on 4 PCs. "
        "So this number shows the real-world gain over a laptop, not the gain from clustering alone. The fair "
        "clustering result is in 7.2.", ORANGE, "#FBF6E9")
para("Live demo: in the lab, the cluster ran the same program (n = 50 million) in 3.87 s, as shown on the head PC "
     "(Figure 8).", size=9.5, italic=True)

doc.add_heading("7.2  Controlled test: 1, 2, 3 and 4 PCs", 2)
para("Here everything was kept the same (same PCs, same program, same test); only the number of PCs changed.")
image(f_tp, 14.5, "Figure 5. Hashes per second. Bars = measured, orange line = ideal.")
table(["PCs", "Speed (M hashes/s)", "Speed-up", "Efficiency", "Time for 100 billion hashes"], [
    [str(k), f"{x:.2f}", f"{s:.2f}x", f"{e:.1f}%", f"{100e9/(x*1e6)/60:.0f} min"] for k, x, s, e in zip(range(1, 5), X, SPEED, EFF)
], BLUE, [2, 4, 3, 3, 5])
para("Speed-up is measured against an average single PC (22.66 M hashes/s). PC 1 alone was a little slower than "
     "average, which is why its speed-up shows 0.91x.", size=9, italic=True)
image(f_sp, 15, "Figure 6. Left: speed-up grows almost in a straight line. Right: almost no time is wasted.")
image(f_share, 15, "Figure 7. Work was split evenly, and every PC was busy computing 96-99% of the time.")
callout("In simple words:", f"4 PCs did the job {SPEED[3]:.2f} times faster than one PC, using {EFF[3]:.1f}% of their "
        "combined power. Less than 4% was lost in coordination. A job that takes 81 minutes on one PC takes 19 minutes "
        "on four. Every added PC made a statistically significant improvement (p = 0.0003), and all repeats gave the "
        "same answer.", TEAL)
para("Note: PC 1 (head) was about 12% slower alone than the other three identical PCs, probably because it was also "
     "running other services. So we compare against the average PC (22.66 M/s), not PC 1 alone (which would give 4.25x).",
     size=9.5, italic=True)

image(HERE / "img/dashboard.jpg", 11.5, "Figure 8. Live dashboard on the head PC: all four workers at 95-99% CPU.")

# 8 MPI
doc.add_heading("8. Cluster Kit vs MPI", 1)
para("MPI is what real supercomputers use. It is built for maximum speed on special hardware. Cluster Kit is built for "
     "zero setup on ordinary lab PCs. We compared the design; we did not measure MPI's speed.")
table(["Feature", "MPI", "Cluster Kit"], [
    ["Finding PCs", "Type every IP in a host file", "Automatic (UDP broadcast)"],
    ["Starting a job", "SSH keys + mpirun on every PC", "One command per PC"],
    ["Splitting work", "Fixed split", "PCs pull work when free"],
    ["Slow or uneven PCs", "Slowest PC holds everyone up", "Fast PCs simply do more"],
    ["A PC fails", "Whole job usually stops", "Lost work is re-sent to other PCs"],
    ["Windows + Linux mix", "Difficult", "Supported"],
    ["Complex communication", "Yes (any pattern)", "No (independent tasks only)"],
], PURPLE, [4.5, 6, 6.5], bold_first=True)

# 9 Adv / Limits / Future
doc.add_heading("9. Advantages, Limitations and Future Scope", 1)
t = doc.add_table(rows=2, cols=3); t.alignment = WD_TABLE_ALIGNMENT.CENTER
cols3 = [("Advantages", TEAL, "#F2F6FC", ["Near-linear speed-up (96.4%)", "Zero setup: auto-discovery", "Uses PCs the lab already has", "Survives a PC failing", "Fast PCs do more work automatically"]),
         ("Limitations", RED, "#F2F2F2", ["Only for work that splits into independent pieces", "Needs all PCs on one LAN", "No login/security: trusted network only", "Head PC is a single point of control"]),
         ("Future Scope", BLUE, "#F2F6FC", ["Test with more than 4 PCs", "Add GPU workers", "Add security (sign-in, encryption)", "Measure against an MPI version", "Run science and data jobs"])]
for i, (h, c, fill, items) in enumerate(cols3):
    top, bot = t.rows[0].cells[i], t.rows[1].cells[i]
    shade(top, c); shade(bot, fill)
    top.text = ""; r = top.paragraphs[0].add_run(h); r.bold = True; r.font.color.rgb = RGBColor(255, 255, 255)
    bot.text = ""
    for j, it in enumerate(items):
        p = bot.paragraphs[0] if j == 0 else bot.add_paragraph()
        r = p.add_run("• " + it); r.font.size = Pt(9.5); p.paragraph_format.space_after = Pt(1)
finish(t, [5.67] * 3)
doc.add_paragraph().paragraph_format.space_after = Pt(2)

# 10 Team
doc.add_heading("10. Team and Task Distribution", 1)
table(["Member", "Responsibilities", "Technologies", "Work completed / contribution"], [
    ["Divyansh Shukla", "System design and development of the cluster engine; deployment on lab PCs",
     "Rust, Python, TCP/UDP, Ubuntu", "Built the head/worker engine, networking, dashboard and examples (all code commits in the repository)"],
    ["Abhinav Dhoke", "[Responsibility]", "[Technologies]", "[Work completed]"],
    ["Sneha Kumari", "[Responsibility]", "[Technologies]", "[Work completed]"],
    ["Vanshika Wadhwa", "[Responsibility]", "[Technologies]", "[Work completed]"],
    ["Ankush Kumar", "[Responsibility]", "[Technologies]", "[Work completed]"],
], TEAL, [3.2, 4.8, 3.5, 5.5], size=9, bold_first=True)

# 11 Conclusion
doc.add_heading("11. Conclusion", 1)
para(f"Cluster Kit turns ordinary lab PCs into a small supercomputer-style cluster. Four PCs were {SPEED[3]:.2f}x faster "
     f"than one PC at {EFF[3]:.1f}% efficiency, and the same program ran {WIN_T/CL_T:.1f}x faster than on a Windows laptop. "
     "PCs find each other on their own using UDP, share work fairly, and keep going if one fails. For jobs that split into "
     "independent pieces, a few lab desktops can deliver close to perfect speed-up.")

doc.add_heading("References", 1)
for ref in ["G. M. Amdahl, \"Validity of the single processor approach to achieving large scale computing capabilities,\" AFIPS Conf., 1967.",
            "J. L. Gustafson, \"Reevaluating Amdahl's law,\" Communications of the ACM, 31(5), 1988.",
            "T. Sterling et al., \"Beowulf: A parallel workstation for scientific computation,\" ICPP, 1995.",
            "Message Passing Interface Forum, MPI: A Message-Passing Interface Standard, Version 4.0, 2021.",
            "M. Rocklin, \"Dask: Parallel computation with blocked algorithms and task scheduling,\" SciPy, 2015.",
            "NIST, \"Secure Hash Standard (SHS),\" FIPS PUB 180-4, 2015.",
            "Cluster Kit source code: github.com/TheDivyanshShukla/cluster-kit"]:
    p = doc.add_paragraph(style="List Number"); r = p.add_run(ref); r.font.size = Pt(9); p.paragraph_format.space_after = Pt(1)

doc.save(OUT)
print("wrote", OUT)
