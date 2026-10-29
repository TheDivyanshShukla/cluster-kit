# Watch demo runbook

Footage: four MEVA school CCTV cameras (G419, G420, G421, G423), one hour each, all covering the same hour (7 March 2018, about 09:55–10:55). Three VIRAT aerial videos (about 5 minutes each). Hall and Street are looping sample feeds.

Credit when showing the footage: MEVA (Kitware / IARPA, CC BY 4.0) and VIRAT Aerial (DARPA, public release; cite Oh et al., CVPR 2011).

## Start (about 20 seconds)

```
cd cluster-kit
CW_AUTH=off rust/target-c2/release/cw head
```

Open http://127.0.0.1:7710. `CW_AUTH=off` means no sign-in and only this Mac can reach it. For a shared demo with sign-in, start without `CW_AUTH=off`; the admin account is `admin` (password changed on `/login`).

Before going on stage:
1. Wait until indexing is finished: Sources page shows every job as "completed". Indexing of the four MEVA hours needs the Mac mostly to itself (about 3 hours when everything runs at once).
2. Close other heavy apps. The wall plays nine cameras and the detector runs on the Neural Engine.
3. Do one full click-through the evening before (script: `scratchpad/demo.cjs`, or follow the path below).

## Click path (about 6 minutes)

1. **Live** (`/`): the CCTV wall, nine cameras. Press `2` for 2×2, `A` for auto. Click a tile to solo it, `Esc` to return. Toggle "Trails". Say: boxes are drawn on the exact frame the detector analysed, so they always line up.
2. **Sources**: nine sources, each with indexing progress. Say: each source keeps its own timeline.
3. **Search** (`/find`): tick the four MEVA cameras, SQL tab, run
   `SELECT * FROM detections WHERE class = 'person' AND confidence >= 0.6`
   (about 5–8 s; it stops at 100,000 rows and says so). Click **Open in review**.
4. **Review**: four camera lanes in parallel, filmstrips under each match. Click a strip, press Space to play, `]` for next match, `+`/`-` zoom, "Matched only" to skip the gaps, `A` to add a clip, Save, then export the manifest.
5. **Search, Lua tab**: show a loop over frames with two or more people:
   ```lua
   for f in db.frames{} do
     local n = f.counts.person
     if n and n >= 2 then emit(f) end
   end
   ```
   Pick the four MEVA cameras in the scope first. Open it in review.
6. **Rules & models** (`/setup`): the "Measured on your cameras" panel: quiet cameras skip most detector work (for example 96% on the empty hall camera), moving drone cameras save nothing. Show a preset change but do not save.
7. **Activity**: every action above is in the audit log.

## What to say, and not to say

- Call tracker output **tracks**, not people. A distinct-tracks count (about 2,600 across the four cameras) is a count of tracker IDs; the tracker starts a new ID after each quiet gap.
- **"Holding a phone" is unsupported** (no installed model can tell). The query returns a clear error. Present it as an honest limit.
- Accuracy so far is a small check: 24 sampled MEVA frames, one annotator, 19 usable: 13 of 17 people found, no false detections. Misses were partly visible people and tiny figures far down a hallway. Models are marked "unvalidated".
- Capture clocks of the MEVA clips are unknown to Watch, so lanes align by media time (the cameras were recording the same hour), and "align UTC" is not available.
- Confidence is the detector's score, not a probability of being correct.
- Fight and Weapon rules are off: there is no model behind them.

## If something goes wrong

| Symptom | What to do |
| --- | --- |
| Wall tiles show "Waiting for first frame" | Wait 10–20 s after start; the worker is starting. Refresh once. |
| Search says "Running…" for a long time | The Mac is busy indexing. Scope the search to one or two cameras. |
| A page looks stale | Hard refresh (Cmd+Shift+R). |
| Server not responding | Start it again with the command above; nothing is lost, data is in `vision/data`. |
| Need to show something offline | Screenshots from the last full rehearsal are in `docs/evidence/demo/`. |

## Known limits (be upfront if asked)

- Indexing totals on the Sources page, for jobs restarted before the 29 October 2026 coverage fix, may be understated for jobs that were restarted; detections and timelines are unaffected.
- Search over all sources is capped at 100,000 rows.
- One-PC demo only; a cluster run needs a worker token on each extra PC (see `docs/WATCH_OPERATIONS.md`).
- Not production-ready: no TLS, no labelled evaluation set, single-annotator accuracy checks.
