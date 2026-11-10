# Initial D THE ARCADE Season 5 — Time Attack timer / course-tracking findings

Status: **diagnosed deeply, not fixed.** The sections below are in investigation order; the
**UPDATE** immediately below supersedes the earlier "refill / verdict" theories — those were ruled
out by live instrumentation.

---

## UPDATE — the real chain (confirmed live) and how far the fix got

Using a payload-side logger un-stub + live pollers (see "Instrumentation built"), the mechanism is
now confirmed by in-game data, not inference:

1. **IndRun is the authoritative car simulation.** The game feeds it the controls via the exports
   `SetAccel(u8)` / `SetBrake(u8)` / `SetSteer(i8)` / `SetShift(i8)` / `OnTick(float)`, which store
   into a control object at `[IndRun.dll+0x21c718]` — `+8` accel, `+0xa` brake, `+0xc` steer, `+0xe`
   shift. IndRun simulates the car and owns race progress, checkpoints and the time-attack clock.
2. **The state machine works.** A live poll of the race state `+0x668` (chain
   `[[[*(IndRun+0x21c720)]+8]][+8]+0x668`) shows `3 → 4 → 6`, i.e. it reaches **state 4 (racing)**.
   So the verdict/anti-tamper and the state transitions are NOT the blocker. (The whole
   "refill / +0x668=2 fallback" theory below is moot.)
3. **The controls never reach IndRun.** Live sampling of `[ctrl+8]` showed `accel=0` the entire
   race under full throttle (`steer` did get through). The game does not forward the pedal to
   IndRun's sim, so IndRun's car reads as **Standing** all race, no checkpoint ever fires, the clock
   is never extended → early TIME UP — even though the game's own physics move the car on screen.
4. **Control-delivery fix (done, in the payload).** New module `payload-alls/src/indctl.rs`
   (`WAL_ALLS_INDRUN_INPUT=1`) writes the launcher's real accel/brake/steer/gear into
   `[ctrl+8..0xe]` every ~4 ms; plus `WAL_PATCHES` no-ops the game's zero-writes
   (`IndRun.dll!0x5ef80:C3` SetAccel, `0x5efb0:C3` SetBrake, `0x5f000:C3` SetShift). After this,
   `accel` holds at `255` under throttle (verified) and the **HUD timer became correct**.
5. **BUT the sim car still does not move.** With accel pinned at 255 reaching IndRun, the car is
   STILL "Standing" and section progress stays 0 (`PlayLogOnRaceExit … Max:[1]:0`). So the stationary
   car is **not** caused by the missing controls alone — IndRun's per-tick **physics / race-start
   release** does not integrate the fed accel into car velocity. `OnTick`'s only use of the control
   buffer is zeroing a scratch field `[ctrl+0x1d]`; the accel at `[ctrl+8]` is read by getters
   (`0x25c0`) and debug logs (`0xe085` "AccelBase.. AB %d %d", `0x52000`), but the velocity
   integration that would move the car is the unresolved layer.

6. **The course path is not loaded in IndRun (latest, strongest clue).** `GetPathPointsNum`
   (`0x612c0`) returns `[[[*(IndRun+0x21c358)]+0x48]+0x10]`. A live poll returns a null chain
   (`[+0x48]` is null) the whole race — i.e. IndRun has **no course racing line loaded**. Without a
   path, IndRun cannot project the car onto the course, so progress/sections can never advance no
   matter how the car moves — which is exactly the "Standing / section 0 / no checkpoint" symptom,
   and lines up with the degenerate `CheckPoint[0]` values (0/10) for these courses.

**Remaining open problem (reframed):** IndRun's course **path/racing-line** (`[*(0x21c358)]+0x48`,
read by `GetPathPointsNum` `0x612c0` / `GetPathPoint` `0x612d0`) stays **null** the whole race, so
IndRun can't project the car onto the course and progress/sections never advance. Note IndRun DOES
have the checkpoints (the game logs `CheckPoint[0]` values; `AddCourseSetting` is a thunk `0x5e820`
→ `0x5a940` onto a *different* object `[0x21c738]`), so checkpoints aren't the missing piece — the
**path spline** is.

The path **can** be populated: the setter is at `0x61249` — `mov [rcx+0x48], rbx` with
`rcx=[0x21c358]` (saves/releases the old `[+0x48]`, stores the new path `rbx`). Under the loader
this either is never called for Time Attack, or is called with an empty/null `rbx`. **Fix path for
whoever continues:** breakpoint `0x61249`, see whether it fires in a S5 Time Attack and with what
`rbx`; walk back to its caller and to where the course spline is built from the loaded course level
(`COURSE/<course>/…`, `CourseEvent/<course>/SL_<course>CP_Default`); find why it's empty under Wine/
the loader. A **Season 3 comparison is the right next probe** — but S3 ships a different `IndRun.dll`
with different offsets, so it must be done with a live debugger locating the equivalent
path-populate function on both, not with these hardcoded S5 pollers.

This is course-path loading — not the controls (now fixed) and not, by itself, the sim physics. The
control-feed fix and all pollers remain in the payload (gated by `WAL_ALLS_INDRUN_INPUT` /
`WAL_ALLS_INDLOG*`) for whoever continues. **This last layer needs a live debugger (the author's
workflow); static analysis + test runs have been exhausted here.**

Also note the courses tested (Akina Lake = course 0, Usui = course 9) have degenerate checkpoint data
(`CheckPoint[0]` = 0 and 10) vs normal courses (Hakone = 802); a main mountain course is a better
test once the car moves.

---

## (earlier investigation — superseded in part by the UPDATE above)

Original status note: Root cause first thought to be the IndRun anti-tamper work-around
(the WAL author's own "minimal verdict" patch, already flagged as an open issue in the Season 5
profile). This documents the investigation so it can be finished.

Game: Initial D THE ARCADE Season 5 (ALLS UX, game id SDGT, `GameProject-Win64-Shipping.exe`,
UE4 + `Plugins/IndRun/Binaries/Win64/IndRun.dll` anti-tamper plugin).
Loader: windowsarcadeloader, `system: alls`, `payload-alls` (`wal_alls.dll`), GE-Proton + Wine.

## Symptom

In **Time Attack**, the race clock runs out long before the first checkpoint, at full speed:

| Run (same engine) | Course | Duration | Result |
|---|---|---|---|
| Season 3 (works) | Hakone (13) | ~3 min (full) | normal finish |
| Season 5 | Akina Lake (0) | ~27 s | TIME UP |
| Season 5 | Usui (9) | ~43 s | TIME UP |

A Time Attack gives a short base time and **each checkpoint passed adds time**. Season 3 runs the
full ~3 min because checkpoints register and extend the clock. Season 5 ends after the base
~30-40 s: **the car's checkpoint progress is not tracked, so the clock never extends.** This
matches the comment the WAL author left in `systemprofiles/alls/initial-d-the-arcade-season-5.yaml`:
*"Time Attack still hits TIME UP early because the car's checkpoint progress is not tracked
(goal/section times save as 0), a separate open issue."*

## What was RULED OUT (with evidence)

- **Not the game dump.** Re-extracted the game; the content paks
  (`GameProject-WindowsNoEditor.pak` 16,425,299,290 B and `..._0_P.pak` 416,161,391 B) are
  **byte-identical** to the previous copy (`cmp` clean). Course/checkpoint data is unchanged.
- **Not the date / expiry check.** IndRun has exactly two clock reads: the expiry at
  `IndRun.dll+0x74c92` (`cmp edx,0x4091f` ≈ 26 Jun 2026), already neutralised by the shipped
  `IndRun.dll!0x74c98:9090` NOP (forces the in-date branch regardless of today's date); and
  `fcn.0x852a0`, which is only an error-log timestamper. The game EXE imports no time functions.
- **Not expansion/DLC content.** `LogExpansionModeSubsystem: Error ... ContentId:-1` appears
  **20× in both** the working and broken logs — harmless noise.
- **Not Usui-specific.** Base courses (Akina Lake / course 0) break the same way. It is every
  Season 5 Time Attack, not just the new Usui event course.
- **Not the server/payload records.** The payload faithfully stores what the game posts
  (`idac/user.rs` `update_time_trial`); the game posts `goal_time=0` because the in-race timer
  already failed. Payload changes cannot fix this.

## Root cause

Season 5 added an IndRun anti-tamper the earlier seasons lack. The author's work-around
(`WAL_PATCHES` in the S5 profile) replaces the **verdict routine** span
`IndRun.dll+0x750ba..0x7520b` with a stub that forces a "clean" verdict and un-sticks the car, and
**skips the per-tick refill** (`IndRun.dll!0x766d5:EB`) to avoid a 4-slot-table overflow crash.

That stub gets the car moving but does **not** reproduce the genuine per-tick car/progress state
the race system reads, so checkpoint passing never registers → the clock is never extended.

Confirmed from the game's own `LogIndRaceSys` at `CreateRace` (checkpoint time budgets):

```
Season 3 Hakone (works):   CheckPoint[0..4] = 802, 2147, 3471, 4810, 6184   → full 3-min race
Season 5 Hakone (course 13): same 802…6184 data, but overflows the 4-slot table (crash logs)
Season 5 Akina (course 0):  CheckPoint[0]=0,  [1]=1597                      → TIME UP
Season 5 Usui  (course 9):  CheckPoint[0]=10, [1]=1996                      → TIME UP
```

The checkpoint **budgets** come through fine; the broken part is the car's **progress/position**
feed, driven by the `+0xf20` table that the refill fills.

## Code map (IndRun.dll RVAs, image base 0x180000000)

| RVA | Role | Author's patch |
|---|---|---|
| `0x75050` | anti-tamper **verdict** routine (grades car from `+0x4bc/+0x4c0/+0x4c4`; sets tamper flags `+0x7c8/+0x7cc/+0x7d0`, penalty `+0x10f4`, phase `+0x1120`) | stub at `0x750ba` |
| `0x763a8` | **re-grade** (checks `+0x10f0==+0x1128`, `+0x1120==10`, `+0x1124==0`, `+0x1110==0`; sets `+0x10f4=0x7ff` on mismatch) | stub values chosen to pass this |
| `0x759c0` | **per-tick refill** — rebuilds the 4-slot progress table at `ctx+0xf20` (count at `[table]`, 0x18-byte entries at `+0x118/+0x128/+0x178`) | call skipped at `0x766d5:EB`; overflow capped at `0x75fe6`/`0x7605e`/`0x87f00` |
| `0x7ece0` | race-update **reader** of the `+0xf20` table (float scaling → progress) | — |
| `0x74c40` | car-setup: environment + expiry checks; writes status codes to `+0x1004/+0x1014` | expiry NOP `0x74c98:9090` |
| `0x6ed90` | **checkpoint-passed handler** — records index at `[rcx+0x28]`, would log `"Checkpoint %d passed (time:%d)(sectiontime:%d)"` | — |
| `0x001df0` | IndRun debug logger — **a no-op stub** (stores varargs, returns) | — |

Exports are `cUE4Interface::*` (`CreateRace`, `AddCourseSetting`, `BindCheckPointPassed`,
`BindRaceTimerUpdater`, `GetRaceTime`, …) — the game↔IndRun bridge.

## Instrumentation built (works)

The IndRun debug logger `IndLog(fmt, ...)` at `IndRun.dll+0x1df0` is a **no-op stub** in the
release build (stores its varargs, returns) — which is why none of its messages ever appear, even
in the working Season 3 run. It is un-stubbed by a new payload module
`crates/payload-alls/src/indlog.rs` (modelled on `payload-seganu`'s `amlog`), enabled with
`WAL_ALLS_INDLOG=0x1df0` in the profile and installed by `indrun.rs` when `IndRun.dll` loads. Two
gotchas, both handled in that module:
- it fires at very high frequency and faults if you `vsnprintf` every call — it filters to the
  checkpoint/progress messages and never formats a `%s`/`%ls` argument;
- the stub **must preserve every volatile register** (rax, rcx, rdx, r8-r11, xmm0-5). The release
  no-op preserved them and IndRun relies on it: `fcn 0x704b0` keeps `r10` across the logger call
  and dereferences `[r10+8]` right after (crash at `0x704d3` until the stub was made register-safe).

## What the instrumentation showed (the real mechanism)

A full Season 5 Time Attack logs only:

```
indrun: Actor:… Standing(time:-200000)   ← car at the start line
indrun: Actor:… Aborted (time:57997)      ← race ends, un-finished
```

**No `Checkpoint passed`, no `setCurrentSection`, no `PlayerPointer` the entire race.** IndRun
sees the car as *Standing* (stationary) for the whole race even though the game physics drives it
at full speed, then aborts. IndRun's internal progress for the player never advances → no
checkpoint registers → the clock is never extended → early "TIME UP".

## Experiments (runtime `WAL_PATCHES`, no rebuild)

- **Exp 1** — drop the author's `0x766d5:EB` (revert the refill skip to the original `je`). The
  gate at `0x766cd` is `cmp [r15+0x1110],0; je skip`, and `+0x1110` is 0 (set by car-setup), so it
  **still skips**. Marginal effect, no real tracking.
- **Exp 2** — NOP both skip branches (`0x766d5`, `0x766df`) to force the refill `0x759c0` every
  tick. Result: **the whole race locks in its start state — car frozen, clock frozen** (does not
  count down), exactly the author's warning "re-runs the race-start fill every tick: the engine
  revved but the car never moved." Reverted.
- **Exp 3 (clock workaround)** — force the `GetRaceTime` export (`0x5ee60`) to return 0
  (`31C0C3`). Goal: if the game drives its Time-Attack countdown from that getter, the clock
  would never reach zero. Result: **no effect — the race still TIME-UPs at the normal budget**
  (`RaceFinish … eNORMAL` at ~43 s), so the TIME-UP is decided **inside IndRun's race-update**,
  not via the getter. Reverted. (A real clock-freeze therefore needs the internal time-up/abort
  trigger, which is the same race-update code path that needs a live debugger.)

## Refined root cause (where it stands)

The symptom is unambiguous: IndRun's race loop classifies the player car as **Standing** for the
entire race (no `Checkpoint passed`, no `setCurrentSection`, no `PlayerPointer`). The game physics
move the car, but IndRun's per-tick race-update never registers forward progress, so no checkpoint
fires and the clock is never extended → early TIME UP.

The refill `0x759c0` was investigated and ruled out as the fix lever:
- it is the **race-start fill + reset**; forcing it per-tick locks the race in its start state —
  car and clock frozen (Exp 2), matching the author's warning;
- its gate `+0x1110` has only two writers — init sets it to `3` (`0x742ef`), then car-setup sets it
  to `0` (`0x74d5f`) whenever the current directory starts with "X". A **real cabinet runs from
  `X:\`**, so `+0x1110` is 0 on legit hardware too, and the refill is skipped there as well. So the
  refill is a special-case path, **not** the normal per-tick progress mechanism.

That means the live progress feed is a **different** per-tick path — the race-update that calls the
Standing/Checkpoint/Aborted handlers (near `0x6ed70`-`0x6ed90`, `0x704b0`) after reading the car's
position/speed. That input is what reads as stationary under the loader. Locating exactly which
field it reads, and why it stays zero while the game moves the car, is the remaining unknown — and
static analysis kept mis-identifying it (the refill theory above was one such miss). This needs a
**live debugger** on the race-update loop (watch the car-position/pointer field each tick and walk
back to its writer), which is how the author did the original anti-tamper work.

## Recommended next step (for whoever finishes it)

1. With `WAL_ALLS_INDLOG=0x1df0` active, attach a debugger and breakpoint the caller of the
   Standing handler (`~0x6ed70`) / Checkpoint handler (`0x6ed90`); read the car-progress/pointer
   field it tests and find its per-tick writer. That writer (or its missing input) is the fix site.
2. Check whether the game pushes the car transform into IndRun each tick (an `UpdateActor`-style
   call or a shared field) and whether any shipped patch, the stubbed verdict, or the faked
   environment (computer/user name, `X:` current dir) severs it.
3. Only then patch the specific field/feed. Avoid blind `WAL_PATCHES` edits to the verdict/refill
   region — each one here either freezes, crashes, or does nothing, as the experiments above show.

## Environment / repro

- Launch: `./dist/arcade-launcher run "<dump>/initial-d-the-arcade-season-5.windowsloader"`
- Logs: `<wine-prefix>/.../GameProject/Saved/Logs/GameProject.log` (`LogIndRaceSys`,
  `PlayLogOnRaceExit`).
- The S5 user override currently carries the refill-re-enable experiment (`WAL_PATCHES` minus the
  `0x766d5:EB` entry). Delete that `env:` block to restore the shipped behaviour.
- The S3 user override currently has `WAL_ALLS_AUTO_CARD: "0"` (the S3 auto-card hit
  `ERROR 6504 Illegal Card Found`); delete that `env:` block to restore the auto-card.
