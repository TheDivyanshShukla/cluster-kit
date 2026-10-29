# Watch demo runbook

Footage: four MEVA school CCTV cameras (G419, G420, G421, G423), one hour each, all covering the same hour (7 March 2018, about 09:55–10:55). Three VIRAT aerial videos (about 5 minutes each). Hall and Street are looping sample feeds.

Credit when showing the footage: MEVA (Kitware / IARPA, CC BY 4.0) and VIRAT Aerial (DARPA, public release; cite Oh et al., CVPR 2011).

## Start (about 20 seconds)

```
cd cluster-kit
CW_AUTH=off rust/target-c2/release/cw head
```

Open http://127.0.0.1:7710. `CW_AUTH=off` means no sign-in and only this Mac can reach it. For a shared demo with sign-in, start without it (admin account `admin`; change the password on `/login`). For other PCs to join, start with `CW_BIND=0.0.0.0` (see docs/CLUSTER_DEMO.md).

All nine cameras are replays of recorded footage that behave like live cameras: red LIVE dot, an honest "replayed from recording" label, real-time playback on a loop. They are set to `analyze: false` in `vision/watch.json`: the detections were computed earlier and the wall draws them on the playing video, so the Mac stays nearly idle. The four school cameras start at about minute 51 of their hour (the busiest part) every time the server starts, and stay in step with each other.

Before going on stage:
1. Start Watch about a minute before presenting, so the cameras open on the busy stretch and the first video seeks are cached.
2. Close other heavy apps and check the wall plays (nine tiles, boxes on people).
3. Do one full click-through the evening before (`scratchpad/demo.cjs` runs the same path and saves screenshots to `docs/evidence/demo/`).

## Click path (about 6 minutes)

1. **Live** (`/`): the CCTV wall, nine cameras. Press `2` for 2x2, `A` for auto, `V` cycles Video + boxes / Analyzed frames / Live preview. Click a tile to solo it, `Esc` to return. Toggle "Trails". Say: boxes glide between stored detections and sit on the exact video frame.
2. **Sources**: nine sources with indexing coverage.
3. **Search** (`/find`): tick the four MEVA cameras, SQL tab. Start typing to show autocomplete (`SEL`, `FROM d`, `WHERE c`, `class = '`). Run
   `SELECT * FROM detections WHERE class = 'person' AND confidence >= 0.6`
   (about 5 s; it stops at 100,000 rows and says so). Click **Open in review**.
4. **Review**: four camera lanes in parallel, filmstrips under each match. Click a strip, press Space to play, `]` for next match, `+`/`-` zoom, "Matched only", `A` to add a clip, Save, export the manifest.
5. **Search, Lua tab**: type `for f in db.` to show suggestions, then run a loop over frames with two or more people:
   ```lua
   for f in db.frames{} do
     local n = f.counts.person
     if n and n >= 2 then emit(f) end
   end
   ```
   Pick the four MEVA cameras in the scope first. Open it in review.
6. **Rules & models** (`/setup`): the "Measured on your cameras" panel (quiet cameras skip most detector work; moving drone cameras save nothing). Show a preset change but do not save.
7. **Cluster** (`/analytics`): joined computers, the Realtime panel (camera to computer map, lag, capacity) and the Benchmark panel. **Join a PC** (`/join`) shows the one-line command and pairing code. Follow docs/CLUSTER_DEMO.md for the multi-PC part.
8. **Activity**: every action above is in the audit log.

## What to say, and not to say

- Call tracker output **tracks**, not people. A distinct-tracks count (about 2,600 across the four cameras) is a count of tracker IDs; the tracker starts a new ID after each quiet gap.
- **"Holding a phone" is unsupported** (no installed model can tell). The query returns a clear error. Present it as an honest limit.
- Accuracy so far is a small check: 24 sampled MEVA frames, one annotator, 19 usable: 13 of 17 people found, no false detections. Misses were partly visible people and tiny figures far down a hallway. Models are marked "unvalidated".
- Capture clocks of the MEVA clips are unknown to Watch, so lanes align by media time (the cameras were recording the same hour), and "align UTC" is not available.
- Confidence is the detector's score, not a probability of being correct.
- Fight and Weapon rules are off: there is no model behind them.
- The cameras are recorded footage replayed as live cameras; say so if asked. Because the footage was analysed in advance, replays do not raise new alerts.

## If something goes wrong

| Symptom | What to do |
| --- | --- |
| Wall tiles are black for a moment | The first video seek into a big file takes a few seconds; the analysed still shows meanwhile. Refresh once. |
| Search says "Running…" for a long time | Scope the search to one or two cameras (searches over everything stop at 100,000 rows). |
| A page looks stale | Hard refresh (Cmd+Shift+R). |
| Server not responding | Start it again with the command above; nothing is lost, data is in `vision/data`. |
| Need to show something offline | Screenshots from the last full rehearsal are in `docs/evidence/demo/`. |

## Known limits (be upfront if asked)

- Indexing totals on the Sources page, for jobs restarted before the 29 October 2026 coverage fix, may be understated for jobs that were restarted; detections and timelines are unaffected.
- Search over all sources is capped at 100,000 rows.
- One-PC demo only; a cluster run needs a worker token on each extra PC (see `docs/WATCH_OPERATIONS.md`).
- Not production-ready: no TLS, no labelled evaluation set, single-annotator accuracy checks.
