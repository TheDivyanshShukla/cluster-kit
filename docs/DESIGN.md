# Console design: power, heads and failover (wave 8)

Scope: how the console shows PC power states, the head and its standbys, failover and recruiting. It builds on what
exists (tokens in `console/src/app.css`, components in `console/src/lib/components/`). Requirement IDs: NET-06, HA-03..07,
HA-10, UI-02 (map), UI-10 (settings), UI-11 (wall).

## 1. Principles

- **One meaning per colour, always paired with an icon or a word** (never colour alone):
  - `ok`: healthy or in sync
  - `warn`: degraded, waiting or behind
  - `bad`: failed or lost
  - `busy` (= primary): working
  - `idle`/muted: idle or asleep
- **Icons are @lucide/svelte only:** Crown (head), Eye (standby), LoaderCircle (candidate), UserPlus (recruiting), Moon (asleep), Sunrise (waking), Power (power settings), ArrowRightLeft (switchover). No emoji or Unicode glyphs (☾ ☀) anywhere, including SVG text on the map: draw the lucide icon's path instead.
- **Motion explains a change, then stops:**
  - a failover animates once (about 600 ms);
  - the replication flow loops only on the wall page;
  - every animation sits under `motion-safe:`;
  - with reduced motion, the state just changes.
- **Every component has five states:** loading (skeleton the size of the content), empty (one sentence on what to do), error (what failed and what still works), success, and edge (0 standbys, 1,000 PCs, very long host names: truncate with a title attribute).
- **Phone (390 px):** no horizontal page scroll, wide content stacks vertically, and touch targets are at least 36 px.

## 2. PC power states (map tiles, node rows, chips)

| State | Tile on the map | Chip (`NodeStateChip`) | Meaning |
| --- | --- | --- | --- |
| awake, idle | normal fill | none | free for work |
| awake, working | `busy` fill, as today | none | has chunks |
| in use (student) | as today | "In use" | no new chunks |
| asleep | fill at 35 % opacity, 1 px dashed outline in `line`, Moon icon (12 px) top-right | Moon + "Asleep" (muted) | the head put it to sleep; it uses almost no power |
| waking | outline in `warn`, Sunrise icon, a slow pulse ring (motion-safe) | Sunrise + "Waking · 0:23" (warn, seconds since the packet) | magic packet sent, not back yet; after 120 s it shows "Did not wake" (bad) with a hint: "check Wake-on-LAN in the BIOS" |

Map legend (bottom-left, existing card): add rows for Asleep and Waking. The map toolbar's bulk selection gains Sleep and Wake (operator), with the same confirm dialog as Nodes. The fleet counter above the map reads "142 awake · 58 asleep · 3 waking" (`num` tabular figures).

## 3. Head and standbys: `HeadTopology.svelte` (new)

A small SVG diagram of who holds the cluster. It is used in three sizes:

| Size | Where | What it shows |
| --- | --- | --- |
| `mini` (h ≈ 120) | Overview, beside the HA card | the head, its standbys, and the term |
| `full` | Settings, above the "Head and standby" panel | adds lag, host names and the recruit slot |
| `wall` | the wall page, top-right, about 420 × 220 | big, with the live replication flow |

### Layout (full)

```
            term 4
   ┌──────────────────┐        ┌──────────────┐
   │  Crown  HEAD     │ ─────▶ │ Eye  lab-a-07│  in sync
   │  lab-b-01        │ ─────▶ │ Eye  lab-c-12│  2 behind
   │  29/30 PCs       │ - - -▶ │ UserPlus  recruiting… lab-d-03 │
   └──────────────────┘        └──────────────┘
        │  │  │
     Lab A  Lab B  Lab C   (one pill per lab: "Lab A 48 · 12 asleep")
```

- **Head card:** on the left, with its host, its PC count and a quorum shield (ShieldCheck ok / ShieldAlert warn).
- **Standbys:** one card each on the right, ordered by takeover priority, with a 1-based number badge. Each shows the host, lag in words ("in sync", "2 behind"), and Make head for admins (the existing flow).
- **Replication lines:**
  - solid when lag is 0, `warn` when behind;
  - on the wall, a dash pattern moves along the line (motion-safe).
- **Recruit slot:** a dashed outline card with UserPlus, "recruiting… <host>" and a spinner. It appears whenever the standby count is below the target. With no candidate it reads "no PC can be a standby (selector `!virtual`)".
- **Lab pills:** under the head, a row with the PC count and the asleep count per lab. A lab with no standby gets a small `warn` dot: "no standby in this lab".
- **Phone:** the cards stack (head, then standbys, then the recruit slot), and the lines become a vertical connector.

### Failover sequence (animated once, driven by `live.ha` and SSE `ha`)

1. **Head lost** (link reconnecting, or `role` = candidate on the new head):
   - the head card turns `bad`, with "Head lost" and an X over the crown;
   - its lines fade to 20 %;
   - the wall banner (existing) starts its timer.
2. **Candidate:** standby #1 gets a LoaderCircle and the label "taking over".
3. **Promoted:**
   - that card glides into the head slot (svelte/motion `tweened`, 600 ms, ease-out);
   - the term counter rolls from N to N+1;
   - the old head card leaves.
4. **Recruiting:** the recruit slot appears, then fills with the new standby ("joined, syncing" → "in sync").

A planned switchover (Make head) plays the same sequence, labelled "Handing over" in `primary` colour instead of `bad`.

## 4. Settings page

Two cards side by side at ≥ 1024 px, stacked below that:

- **Power (`PowerPanel`, exists):**
  - mode as a Segmented control (Off / Eco), with a one-line explanation of the chosen mode;
  - fields: "Sleep a PC after it is idle for [15] min", "Keep at least [4] PCs awake", "Wake PCs when work arrives" toggle;
  - a summary line: "Now: 142 awake, 58 asleep. Eco never sleeps the head, standbys or busy PCs.";
  - spacing: 12 px between the Segmented control and the first field label (fixes the 390 px tightness).
- **Standby heads (in `HaPanel` full, exists):**
  - "Keep [2] standby heads" as a stepper (0-5);
  - a selector field with a live preview ("23 PCs can be a standby");
  - HeadTopology `full` sits above both cards.

## 5. Wall page (UI-11)

- **Top-right: `HeadTopology wall`.**
- **Power strip:** a new strip under the big numbers: Moon "58 asleep", Sun "142 awake", Sunrise "3 waking".
  - When RAPL energy is known, add "12.4 kWh today".
  - With power mode Off, the strip shows only "all PCs awake".
- **Event ticker:** the existing failover banner stays. Add one line at the bottom for `ha.recruit` and `power.*` events ("lab-d-03 became a standby", "18 PCs went to sleep, 06:10"), newest first, the last 3 events, fading out.

## 6. Acceptance (lead review)

- Screenshots at 1440 × 900 and 390 × 844 of Overview, Map (with asleep and waking PCs), Settings and Wall, in light and dark.
- A failover in the mock (the role flips to candidate, then to primary with the epoch bumped, then a recruit) captured as 3 frames on the wall.
- `scrollWidth` equals the viewport width on every page, there are 0 console errors, and `bun run check`, `bun test` and `bun run build` pass.
- No Unicode glyph icons remain (`rg "☾|☀" console/src` finds nothing).

## 7. Live system canvas (`/live`, and the wall's main panel)

The one picture of everything happening in the cluster: PCs, heads, work moving and events, at 200 PCs on a lab PC
and 1,000+ in the simulator view. HTML `<canvas>` 2D (no library), devicePixelRatio aware, 60 fps target and never
below 30 fps with 1,000 PCs (draw only what changed; batch per frame with requestAnimationFrame; no DOM per PC).

### Layout (force-free, stable: a PC never jumps)
- **Labs** are rounded regions laid out in a grid (positions from the lab map when the PC is placed, else packed
  rows by lab, then by node id). A PC is a small circle, 6-10 px, sized by threads.
- **Head** sits at the centre top with a Crown glyph; **standbys** to its right in takeover order (Eye glyph and a
  number). A thin replication line runs head → standby (solid in sync, `warn` when behind).
- **Fill** = state, the same as the map: busy (`busy`), idle (surface), in use (outline `warn`), asleep (35 % and
  dashed), waking (pulse), lost (`bad`), cordoned or draining (hatched). There is always a legend in the corner.

### Motion (each motion means one real event)
- **Work flow:** a dispatched chunk is a small particle from the head to the PC, and a result returns as a particle
  back. Batch them: at most ~300 particles alive, so high task rates show as density, not a firehose. The colour is
  per job (up to 8 job hues derived from `primary`, then grey), and a legend lists the jobs.
- **Events** draw a ripple at their PC or head and a line in the event rail:
  - PC lost: a `bad` ripple;
  - PC joined: an `ok` ripple;
  - sleep / wake: a Moon / Sunrise fade;
  - speculation copy: a dashed particle;
  - head failover: the head node cracks to `bad`, the promoted standby glides into the head spot (600 ms), and the term
    counter rolls;
  - recruit: a new standby slot fades in.
- `prefers-reduced-motion`: there are no particles or ripples; state changes still recolour, and the rail still lists
  everything.

### Interaction
- Hover a PC: a tooltip with host, lab, state, current job, rate and last event. Click: pin a side panel with its
  recent events and actions (operator: Drain, Sleep, Wake).
- Wheel or pinch to zoom and drag to pan; double-click resets. A filter bar takes a selector (`lab=A`, `job=12`,
  `state=lost`), and PCs that do not match dim to 15 %.
- **Event rail** (right, 320 px; at 390 px a bottom sheet): the newest events at the top, with an icon, a short sentence
  and the time. Click one: the canvas flashes the PCs involved.
- **Timeline scrubber** (bottom): the last 30 minutes as a sparkline (busy PCs, tasks/s), with event ticks coloured by
  kind. Drag it into the past to replay: the canvas redraws from the event history (GET /api/v1/events; the log API when
  present); "Live" returns to now. Replay is read-only.

### States
Loading: a skeleton of lab regions. Empty (no PCs): "No PCs yet: add one with a join code" and a link. Head unreachable:
the last frame frozen, greyed, with "Reconnecting…". Huge fleet (> 2,000 PCs): circles become 3 px points, particles turn
off and per-lab totals show instead.

### Acceptance
The mock (scripts/mock-api.ts) streams chunk and node events at a realistic rate (about 50 tasks/s, some lost, a
failover at 30 s, a recruit). There are screenshots at 1440 and 390 in light and dark, and a 3-frame capture of a failover.
An FPS probe with 1,000 mock PCs stays ≥ 30 fps (log `performance.now()` frame times in a test page). check, test and
build pass; scrollWidth equals the viewport; 0 console errors.

## 8. App shell: the page never scrolls; panels do (user, 2026-11-05)

"The whole page should not scroll. Some areas or widgets can scroll (logs, search). Sorting and filters too, and it
should all make sense."

### Shell
- `html, body` are fixed at `100dvh`, `overflow: hidden`. The layout is a CSS grid: a sidebar (desktop) or top bar
  (phone) and a main area. The main area is `min-height: 0` and a grid of panels sized to the viewport (no
  `min-h-screen` stacks that overflow).
- Every page is a **dashboard of panels**. A panel has a fixed header (title, count, the panel's own filter / sort
  controls, actions) and a body that scrolls on its own (`overflow: auto`, `overscroll-behavior: contain`, a thin
  styled scrollbar, and a shadow fade at the top or bottom edge when there is more). Only these scroll: tables, lists,
  logs/event rails, long forms (inside their panel), and the settings list.
- Charts, the canvas, the topology and the big numbers never scroll: they scale to their panel (ResizeObserver).
- **Desktop ≥ 1024 px:** panels sit side by side in a page-specific grid (`grid-template-areas`).
- **Tablet 640–1023 px:** two columns, panels resize.
- **Phone < 640 px:** a single column with one panel visible at a time, switched by a segmented tab strip under the
  page title (e.g. Live: Canvas | Events | Timeline; Nodes: List | Map | Details). The active tab lives in the URL
  (`?tab=`). There is still no page scroll; the active panel's body scrolls.
- Keyboard: `Tab` moves through the panels; `/` focuses the current page's search; `Esc` clears it.

### Lists: one toolbar pattern everywhere (`ListToolbar.svelte` + `lib/listing.ts`, pure and tested)
- **Search** (debounced 150 ms; matches the useful text fields, highlights matches).
- **Filters as chips:** the facets that make sense for that list only, each with counts. For example:
  - Nodes: state, lab, power, in use;
  - Jobs: state, kind, priority, submitter;
  - Events / Logs: kind group (job, node, ha, power, tune, auth, api), level, actor, time range (15 m, 1 h, 24 h,
    custom);
  - Candidates: eligible only;
  - Tunables: group, changed only, source.
- **Sort:** a click on a column header toggles asc / desc with an arrow. Default sorts:
  - Nodes: state severity (lost first), then lab, then name (natural sort, so pc-2 comes before pc-10);
  - Jobs: running first, then newest;
  - Events: newest;
  - Candidates: score.
- **Filters persist:** filter, sort and search live in the URL (`?q=&state=&sort=`), so a view can be shared and
  survives a reload.
- **Row count:** "Showing 37 of 212" plus a Clear filters button when anything is active. There is an empty state per
  list ("No PCs match lab=B, lost. Clear filters").
- **Big lists** (> 500 rows) render windowed: only the visible rows plus a buffer, with a fixed row height. Logs and
  events load more as you reach the end (cursor `next`). A live-updating list keeps your scroll position, and when you
  are scrolled away from the top a "12 new" pill appears instead of jumping.

### Pages under the shell (examples)

| Page | Desktop panels | What scrolls |
| --- | --- | --- |
| Overview | numbers, head topology, mini canvas, jobs, recent events | jobs, events |
| Live | canvas, event rail, timeline | event rail |
| Nodes | list (toolbar) + details or actions | list, details |
| Jobs | list + selected job's detail | list, parts/log |
| Events / Logs | toolbar + list + a detail drawer for a JSON line | list |
| System | head and standbys, candidates, power, all settings | candidates, settings |

### Acceptance
- At 1440 × 900, 1024 × 768 and 390 × 844, on every page, `document.scrollingElement.scrollHeight <= innerHeight`
  and `scrollWidth <= innerWidth`, in light and dark, with real-sized data (mock: 200 nodes, 50 jobs, 2,000 events).
- Every list has search, its facet chips, header sort and URL state, with tests for `listing.ts` (filter, sort,
  natural sort, URL round-trip).
- Windowing holds 10,000 rows without jank (frame ≤ 16 ms on scroll in the probe).

## 9. Research-grade polish: professional, usable, impressive (user, 2026-11-05)

Audience: senior scientists and researchers. They want dense, precise, honest information, with no decoration that
carries no data. The tone is that of an instrument panel (think a lab instrument or a good observability tool), not
a marketing page.

### Visual language
- **Typography:** one sans family (the current one) plus tabular monospaced numbers (`num` class, `font-variant-numeric:
  tabular-nums`) for every figure.
  - Units are always shown, in a muted style after the number ("4.57 GH/s", "98.0 %", "312 kWh").
  - Uncertainty shows where we have it: "± 1.4 %" and the range of repeats on charts.
- **Density:** compact rows (32 px), 13 px body text, 11 px captions. Whitespace separates groups, not items.
- **Colour:**
  - state colours only (ok / warn / bad / busy / idle), plus one accent;
  - job hues come from a fixed 8-colour scheme that is colour-blind safe (Okabe-Ito order), defined as tokens;
  - no gradients except in the heat overlays.
- **Charts** (one small helper, no library unless it earns its size):
  - hairline axes, labelled units, no chart junk;
  - hover gives exact values;
  - every chart has "Download CSV" and "Copy as PNG".

### Canvas v2: more information, better shapes (`/live` and the wall)

**Shapes carry meaning:**
- the head is a hexagon (Crown), standbys are smaller hexagons, and a relay (future) would be a diamond;
- a PC is a rounded square;
- a lab is a soft region with a header strip.

**Semantic zoom** (the level of detail grows as you zoom in, never cluttered at any zoom):

| Zoom | What a PC shows |
| --- | --- |
| far, under 6 px per PC | a filled point, coloured by the active layer |
| mid | a rounded square: fill = layer value; a ring = CPU busy share; a left stripe = current job hue; a small corner badge for in use / asleep / waking / lost |
| near, over 40 px | a mini card: host, job, rate (MH/s or items/s), CPU % and RAM % bars, threads, last event |

**Layers** (a segmented switch, with a legend and a scale bar for each):

| Layer | Fill |
| --- | --- |
| Status | default |
| Load | CPU % |
| Throughput | rate relative to the fleet's best |
| Job | job hue |
| Power | awake / asleep / energy when known |
| Reliability | losses in 24 h |
| Head fitness | head_score from /api/v1/ha/candidates |

**Lab header strip:** name, PCs (busy / awake / total), lab throughput, the share of the cluster's work, and power
(W, when known).

**Overlays:**
- flow particles, kept;
- optional "flow bundles": one curved band per lab from the head, with width = tasks/s;
- event ripples, kept;
- a selection halo.

**Navigation:**
- a minimap in the corner at near zoom;
- "fit all", "fit lab", "follow job J" (keeps that job's PCs in view);
- the URL holds the layer, zoom and selection.

**Side panel on click:** tabs for Info (inventory, calibration, head score parts), Activity (sparkline of rate and
CPU for the last 30 minutes, from history), Events, Actions.

### Global search and command palette (UI-12): `Ctrl/Cmd+K` and the `/` key in the top bar
- **One box finds everything:**
  - PCs (host, id, lab, label), jobs (id, name, submitter), users, labs, pools;
  - settings (tunable key, help text), pages;
  - recent events and log lines (the server's logs API with a text filter; debounced, cancellable);
  - actions ("drain pc-12", "sleep lab B", "make pc-18 head", "new job", "set lost_after_s").
- **Ranking:** fuzzy, with type prefixes (`pc:`, `job:`, `set:`, `>` for actions) and recent items first.
- **Results are grouped by type and keyboard driven:** ↑ / ↓ to move, Enter to open, ⇥ to switch group. An action
  opens its confirm dialog; nothing runs silently.
- **Role-aware:** actions above your role are shown disabled, with the role they need.
- **Selector awareness:** typing a selector (`lab=A state=lost`) offers "show these on the canvas / in Nodes".

### Research features worth adding (proposals, built when approved)
1. **Experiment notebook:** each `experiment.py` run, with its provenance, lives on a page showing speed-up,
   efficiency and Karp-Flatt plots, with the sim prediction overlaid on the measured points (SIM-04 by eye), plus
   export as CSV, PDF figures and a BibTeX-ready caption.
2. **Job comparison:** pick 2–4 jobs and compare makespan, efficiency, chunk-size distribution, per-PC rates and the
   time lost to stragglers.
3. **Per-job efficiency breakdown:** where the time went: compute, dispatch waiting, tail, recomputation after a loss,
   failover pause.
4. **Alerts** (OBS-05), with thresholds as tunables:
   - a PC slower than its peers (median − 3 MAD), a flaky PC, the head busy above 70 %;
   - a standby behind by more than N changes, a reserve about to break.
5. **A Run report** for the press and paper: one page per day, with throughput, PCs, failures recovered, energy and
   headline numbers, plus a printable PDF.
