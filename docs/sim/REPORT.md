# Simulation study: Cluster Kit at 200 PCs and beyond

Everything here comes from `python3 scripts/sim_study.py` (about 30 s), which runs `ck sim` (`rust/src/sim.rs`). The chunk-level simulator calls the real `sched::chunk_len`, and the day simulator calls the real `sched::place`. CSVs and PNG/PDF figures are in `docs/sim/`, and `results.json` holds every number below. Seeds are fixed, so a rerun gives the same numbers.

**Status.** The simulator passed SIM-04 (`VALIDATION.md`): the 4-PC paper points within 3.2 %, the head-ceiling points within 5 %, and the adaptive 200-agent job within 1.1 %. Validation covers only the dispatch, head and scheduling model. Anything that depends on an assumption (power, wake latency, failure rates, timetable, speed spread) is a projection until the practical runs in section 5 confirm it.

## 1. Method

**Chunk level (E1, E3, E4, E5).** One event per chunk.

- **PC speed:** each PC has a speed drawn once per PC (`speed_cv`) and once per run (`run_cv`), plus jitter per chunk, a fixed overhead per chunk and a one-way latency.
- **Head:** it serves one task message at a time, at 379 µs + 0.199 µs per connected agent.
- **Chunks:** sizes come from `sched::chunk_len`: fixed is total/(16p); adaptive is rate × 1 s, with a guided tail.
- **PC failures:** PCs fail by MTBF or at a set time, and their in-flight chunks are requeued.
- **Head crash:** a standby resumes dispatch after the promotion time (measured 5.7 s). Work the PCs finish meanwhile is delivered afterwards, so nothing is recomputed.
- **Relays:** one per 50-PC lab, 20 tasks per head message.

**Day level (placement, eco, E7).** 10 s ticks over 24 h.

- **Jobs:** served by priority, then by arrival. Each job's PC set comes from `sched::place`, and PCs reserved for a class are blocked for jobs. Job speed is the sum of PC speeds × 0.97, plus one chunk of tail.
- **Eco:** an idle PC sleeps after `idle_sleep_s`. A job with unmet demand wakes sleeping PCs, and class PCs wake `prewake_s` early.
- **Energy and failures:** energy is power per state × time. A PC failure loses `down_s` and redoes the chunk in flight.

**E1 weak scaling.** Two job sizes: 70 M hashes per PC (about 3 s, the paper's size) and 700 M per PC (about 30 s). Each point has 7 repeats, and the head PC (10 % slower) is always one of the PCs.

## 2. Assumptions

| Parameter | Value | Source |
| --- | --- | --- |
| Worker speed (SHA-256, 12 threads) | 23.34 MH/s | measured (paper) |
| Head PC speed | 20.92 MH/s | measured (paper) |
| Run-to-run CV, workers / head PC | 0.55 % / 4.78 % | measured (paper) |
| Chunk rules | fixed total/(16p); adaptive rate × 1 s | paper / sched.rs |
| Head service time | 379 µs + 0.199 µs per agent | fitted to 2 measured points (200 agents 2,100–2,670 tasks/s; 1,000 agents 1,560–1,900) |
| Head cost beyond 1,000 agents | straight-line extrapolation | assumption |
| One-way latency | 0.3 ms | assumption |
| Standby promotion / planned switchover | 5.7 s / 3.7 s | measured |
| Manual head restart, no standby | 5 min / 30 min | assumption |
| Relays: 1 per 50 PCs, batch 20 (relays not built, no batching delay) | | assumption |
| Speed spread of a real lab (CV) | 3 % | assumption |
| 4 labs × 50 PCs at 18 / 23.3 / 23.3 / 28 MH/s | | assumption |
| Power per PC: idle / busy / asleep / boot / class | 25 / 65 / 2 / 65 / 45 W | assumption (no meter yet) |
| Wake latency | 45 s (30–120 s tested) | assumption |
| Eco idle timeout / prewake | 10 min / 2 min | assumption |
| Weekday classes | Lab1 9–11 (50 PCs) and 14–16 (40); Lab2 10–12 (50) and 13–15 (45); Lab3 9–12 (50); Lab4 11–13 (50) and 15–17 (35) | assumption |
| User jobs | 0.2–2 "10-min whole-lab" units at 02:00, 11:00, 13:30, 15:00, 19:30 | assumption |
| PC MTBF; down time per failure | 6 h to 30 days; 3 min | assumption |
| Head MTBF | 7 days, with a standby | assumption |
| Not modelled | tail speculation, relay batching delay, eco `wake_max` rounds, class load on jobs (E6), fair sharing among equal priorities | omitted |

## 3. Results

**E1: scaling (`fig_e1_scaling`).**

- **Long jobs (about 30 s):** at 200 PCs, median throughput is 4.57 GH/s against an ideal 4.67 GH/s, 98.0 % efficiency.
- **Short jobs (about 3 s):** with fixed chunks it is 4.37 GH/s, 93.7 %. With adaptive chunks it reaches only 68 % at 200 PCs, 83 % at 100 and 89 % at 64, because a 3 s job is mostly ramp-up and tail. A 10 s job measured 91 %.
- **For the paper:** use jobs of at least 30 s for scaling claims. For short jobs, prefer fixed chunks or a longer `target_chunk_s`.

**E3: losing k of 200 PCs (`fig_e3_lose_k`).** The PCs are lost halfway through a 10-minute job.

| PCs lost | Added time, PCs stay down | Added time, PCs back after 3 min |
| --- | --- | --- |
| 1 | +0.35 % | +0.25 % |
| 5 | +1.4 % | +0.8 % |
| 10 | +2.6 % | +1.5 % |
| 20 | +5.6 % | +3.1 % |
| 50 | +16.8 % | +7.6 % |
| 100 | +49.9 % | +15.1 % |

Even at k = 100, only 0.17 % of the job is redone. Losing PCs costs the capacity they carried and nothing more.

**E4: head crash (`fig_e4_head_crash`).**

- **With a standby:** the job ends 4.7 s later at 20, 200 and 1,000 PCs, with no lost work and no duplicates. A planned switchover costs 2.7 s.
- **Without a standby:** the delay equals the restart time: 299 s for a 5-minute restart, 1,799 s for 30 minutes. If the head is never restarted, the job stalls at 50 %.
- **Target:** the 15 s recovery target holds as long as promotion stays under about 16 s.

**E5: head architectures (`fig_e5_architectures`).** Efficiency on a 10-minute job:

| PCs | single | standby | relays | relays+standby |
| --- | --- | --- | --- | --- |
| 200 | 99.8 % (head 9 %) | 99.8 % | 100 % | 100 % |
| 1,000 | 98.1 % (head 59 %) | 98.1 % | 100 % | 100 % |
| 10,000 | 4.1 % (head 100 %) | 4.1 % | 99.4 % (head 20 %) | 99.4 % |

A head crash at 300 s adds 4.6–5.7 s in the standby columns. So 200 PCs need one head plus a standby; relays pay off only above roughly 1,000–2,000 PCs. The 10,000-PC rows extrapolate the head cost.

**Placement and `max_pcs` (`fig_place_modes`).** Four labs, classes, 5 user jobs, no background job, eco on.

| Setting | Mean user-job time | Energy per day |
| --- | --- | --- |
| No limit (spread) | 1,228 s | 54.3 kWh |
| `max_pcs` 100 | 1,402 s | 52.6 kWh |
| `max_pcs` 50 | 2,094 s | 51.5 kWh |
| `max_pcs` 20 | 5,100 s | 50.9 kWh |

- **`fastest`:** at `max_pcs` 20 it is 7 % quicker than `spread` (4,741 s against 5,100 s).
- **`efficient`:** with no limit it is 17 % slower and saves no energy in this model (54.5 kWh).
- **`pack`:** equals `spread` here, because every PC has 12 threads.
- **With a background PoW job:** energy (298.2 kWh) and total hashes (321 T) are identical in every cell. Placement only decides who waits.

**Eco power mode (`fig_eco_day`).** Over a weekday, always-on uses 139.8 kWh and eco uses 54.3 kWh: **61 % saved**.

- **Idle timeout:** the saving is 46 % with a 1-hour timeout and 63 % with 5 minutes.
- **Wake latency:** 30 s against 120 s moves it by less than 1 point.
- **Idle power:** at 15 / 25 / 40 W idle, the saving is 48 / 61 / 71 %.
- **Waiting for a wake:** user jobs that land on sleeping PCs wait about 30 s. With a 2-minute prewake, no class PC is ever asleep; without prewake there are 16,000 PC-seconds of delay per day, up to 45 s each.
- **When it saves nothing:** the saving needs idle time, so it is zero when PoW keeps every PC busy overnight. Prewake must be at least the wake latency.

**E7: 24-hour soak (`fig_e7_soak`).**

- **Output:** 200 PCs running PoW for 24 h give 4,494 MH/s, 312 kWh per day and 0.35 MH/J.
- **PC failures per hour:** they follow 200 / MTBF: 33 at a 6 h MTBF, 8.2 at 1 day, 2.7 at 3 days, 1.2 at 7 days and 0.3 at 30 days.
- **PoW work lost:** against a 30-day MTBF, 0.8 % at 6 h and 0.2 % at 1 day.
- **Head failovers in 24 h:** about 0.1–0.4.
- **Not modelled:** the real soak will also be shaped by PCs being switched off or used by students.

## 4. What it means for the 200-PC lab

1. One head plus one standby is enough: the head is 9 % busy at 200 PCs. No relays are needed.
2. Scaling is near-ideal (98 %) for jobs of 30 s or longer. Short jobs lose 6 % with fixed chunks and 32 % with adaptive chunks.
3. A head crash costs about 5 s. A lost PC costs its share of capacity plus under 0.2 % recomputation.
4. Eco sleep saves about half to two thirds of daily energy when nothing runs overnight. With PoW running every night the saving is zero, and the lab produces about 4.5 GH/s for 312 kWh a day.

## 5. What the practical runs must confirm

| Number | Simulated | Command |
| --- | --- | --- |
| E1, 3 s jobs | 4.37 GH/s, 93.7 % at 200 (fixed); adaptive 68 % | `python3 scripts/experiment.py --head http://HEAD:7702 --pcs 1,2,4,8,16,32,64,128,200 --repeats 7 --n-per-pc 70e6 --policy fixed,adaptive --out exp-e1-3s` |
| E1, 30 s jobs | 4.57 GH/s, 98.0 % | the same with `--n-per-pc 700e6 --policy fixed --out exp-e1-30s` |
| Head ceiling and CPU, 200 PCs | 2,274 tasks/s at saturation; 9 % busy on a 10-minute job | `python3 scripts/experiment.py --head http://HEAD:7702 --pcs all --kind synth --synth-ns 1000 --n-per-pc 1e6 --policy fixed --chunk 5000 --repeats 3 --out exp-e2`, plus the head's CPU from `top` |
| E3 | +5.6 % at k = 20, +16.8 % at k = 50; redone work 0.03 % / 0.08 % | `experiment.py --pcs all --repeats 1 --n-per-pc 1.4e10` (about 10 min); power off k PCs at 5 min by hand (no script does it yet) |
| E4, standby | +4.7 s, 0 duplicates | the same job; `kill -9` the primary at 5 min (chaos.sh scenario e) |
| E4, switchover | +2.7 s | the same job; `ck admin switchover NODE` at 5 min |
| E7 | 4,494 MH/s, 312 kWh, 0.35 MH/J, 200/MTBF failures per hour | `python3 scripts/soak.py --head http://A:7702,http://B:7702 --hours 24 --out soak-day1` |
| Power per PC: idle / busy / asleep / boot | 25 / 65 / 2 / 65 W | a wall meter on 3 PCs, or the energy fields `soak.py` reports |
| Wake latency | 45 s | console Power page: wake 10 PCs and time them to `active`, at 3 different times |
| Eco saving over a weekday | 61 % | wall meter on two matched weekdays, always-on against eco |
| Placement | `max_pcs` 20 / 50 gives 5,100 / 2,094 s mean job time; `fastest` 7 % quicker | the same job with `placement` and `max_pcs`, on an idle lab, no PoW |
| Real PC speed spread | CV 3 % | `experiment.py --pcs 1 --selector node=NODE` for each PC |
| PC MTBF | unknown | the `soak.py` report: PCs lost per hour |

If a measured number is more than 10 % off, change the matching parameter at the top of `scripts/sim_study.py`, rerun it, and check that the validation table still passes.
