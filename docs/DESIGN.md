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
