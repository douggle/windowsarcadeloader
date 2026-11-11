# Initial D THE ARCADE Season 5 — Time Attack timer / course-tracking findings

Status: **both race modes now run to their own end.** The early `RetireRace(1)` cancel that
ended Story races seconds after the lights and Time Attack races about a minute in is filtered
for the whole race (see "Both race modes ending early: the same `RetireRace(1)` (fixed)" at the
end of this file; it supersedes the "not fixed" note that stood here). Time Attack's
checkpoint-driven clock extension is still the open problem described below. The sections below are in investigation order; the
**UPDATE** immediately below supersedes the earlier "refill / verdict" theories — those were ruled
out by live instrumentation. **UPDATE 2** (next) retires the course-path lead of UPDATE step 6 and
narrows the search to the course setting's `.pa4` paths and the sim car state.

---

> **Rebased onto upstream.** `aderumier/windowsarcadeloader` now ships its own Season 5 support
> (`ff17b52`, SDGT 3.10). It fixes the same race differently from this branch: instead of stubbing
> the anti-tamper verdict, it makes the three race-setting check values the constants IndRun
> expects (`0x6e23af` / `0x6e241d`: `mov al,1`) and removes IndRun's date limit. Its note is that
> with wrong checks "the player's car is driven by IndRun (steering ignored, the pedals barely
> felt), and its race start state never advances" — the same family of symptom this document
> chased. The verdict-stub `WAL_PATCHES` below is therefore obsolete and the profile is upstream's;
> the modules kept from this branch (`indctl.rs`, `indlog.rs`, `indstate.rs`) are diagnostics only.
> Re-check the symptom on the rebased profile before trusting the chain below.

## UPDATE 2 — the "missing course path" lead is ruled out (static proof); where the path data really comes from

The `+0x48` racing-line theory of UPDATE step 6 does not hold. `[*(IndRun+0x21c358)]+0x48` is
written by exactly one function: the `LoadPathPoint` export (`0x611b0`). It allocates a path
object, stores it at `+0x48`, releases the old one, and tail-jumps to the loader `0x5b810`, which
opens `[*(IndRun+0x21c700)+0x20c8]` (the app root the game passes to `Start`) + the name it was
given, with `fopen_s`, and logs `Load Path %s.`. But:

- the Season 5 game EXE (`GameProject-Win64-Shipping.exe`) **does not import** `LoadPathPoint`,
  `UnLoadPathPoint` or `GetPathPointsNum`, and contains **no such strings** (its `GetPathPoint`
  hits are UE4 spline names: `GetPathPoints`, `GetPathPoint`, `UpdateCoursePath`);
- a call/jmp scan of `IndRun.dll`'s `.text` finds calls to the loader `0x5b810` only at `0x59685`,
  `0x5a4f3`, `0x5a508`, `0x61281` (the export's own tail) and `0x69115` — never from a caller of
  the `LoadPathPoint` export;
- Season 3 has the same dead export (its EXE does not import it either) and its Time Attack tracks
  checkpoints and extends the clock normally.

So a null `+0x48` is the normal state in **both** seasons: `GetPathPointsNum`/`GetPathPoint` are a
unused (debug/test) API, not the race's racing line. Nothing to fix there — drop the lead.

Where IndRun actually gets course path data:

- **The game's course setting.** The setter just before `AddCourseSetting` (`0x5a3c0` region) takes
  two `std::string` names, bails if either is empty (`cmp byte [name],0; je`), allocates a path
  object at `[courseSetting+0x58]` and `[courseSetting+0x60]`, loads each with `0x5b810`, and only
  when both succeed sets `[courseSetting+0x68] = 1` ("course paths loaded"). Those names are the
  `[F]/[B] …PathName` fields of `stCourseSetting`, which `AddCourseSetting` logs:
  `[F] ColliPathName`, `[F] CenterPathName`, `[F] LeftPathName`, `[F] RightPathName`, and the four
  `[B]` ones. The `.pa4` files exist in the dump at
  `App/DAC/WindowsNoEditor/GameProject/Plugins/IndRun/Content/COURSE/<course>/path/`
  (`akina_cmn_path_road_c.pa4`, `hakone_path_road_l.pa4`, …), so the data is present; the question
  is whether the game passes usable names and whether `fopen_s` resolves them from the app root.
- **A debug/test loader** at `0x69115` falls back to the hardcoded
  `./data/COURSE/akina_lake/path/akinaLake_path_road_l.pa4` when a `Selector` setting is absent.
  Dev-only; not the arcade path.

This leaves a much narrower question than "the spline is missing": does the game send the
`stCourseSetting` path names under the loader, and does the loader find the files from the app
root (`Start`'s string, `[*(0x21c700)+0x20c8]`)? An empty name set, or an app root that does not
resolve, leaves the course without its road/collision paths — the "game draws the car where it
thinks it is, IndRun never sees progress" symptom.

### Instrumentation added with UPDATE 2

- `crates/payload-alls/src/indlog.rs`: the keep list now includes the course-setting path lines and
  `Load Path`, and those two formats are formatted with their `%s` argument (they are C strings
  IndRun itself dereferences), so a run prints the path names the game sends and the exact file
  IndRun opens.
- `crates/payload-alls/src/indstate.rs` (`WAL_ALLS_INDRUN_STATE=1`): while the race state `+0x668`
  is 4 (racing), calls `cUE4Interface::FindCarStateInfo(0)` (`IndRun.dll+0x60c90`, struct through
  the hidden return pointer in `rcx`) every 500 ms and logs the first `0x40` bytes when they
  change. A frozen sample while the on-screen car moves proves the sim never runs; samples that
  change move the target past the sim, to the checkpoint/section code.
- `userprofiles/alls/initial-d-the-arcade-season-5.yaml` sets `WAL_ALLS_INDRUN_STATE: "1"`.

### The one run that decides it

One S5 Time Attack race (`./dist/arcade-launcher run "<dump>/initial-d-the-arcade-season-5.windowsloader"`),
then in `wal-alls.log`:

1. `[F]/[B] …PathName : …` — are the names empty, relative, or full paths?
2. `Load Path …` — which file does IndRun open, and does it exist in the run directory Wine uses
   as the app root (`wine-prefix/full/drive_c/...`, mirrored from the dump)?
3. `indstate: car state …` — does the sim's car state change at all?

Then: names empty → the game's `stCourseSetting` is not being filled (course data or a patch); a
wrong/missing `Load Path` → fix the app root string(`[0x21c700]+0x20c8`) or the name the game
passes; sim state changing but still no `Checkpoint passed` → the fault is after the sim, in the
checkpoint/section code (`0x6ed90`, `0x7ece0`).

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

> Superseded by UPDATE 2 at the top: the path-spline step below is ruled out. Use UPDATE 2's run
> (path names + `Load Path` + sim car state) instead.

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

## Season 5 story races ending seconds after the start

A Story race used to end with `RaceFinish … eNORMAL` a few seconds after `RaceStart`, reported as
`state=6`, end type `0x1`, finished flag `+0x53c` untouched, with over 100 s still on the clock.

- `cUE4Interface::RetireRace(eRetireRaceType)` (`IndRun.dll+0x5ede0`) is what ends it. Its type-1
  path (`sub edx,1; je 0x5ee16`) runs `movl $0xffffffff,0x538(%rax)`: it stores `-1` into the
  remaining race time. The clock updater (`0x7b820`) subtracts the elapsed time and finishes the
  race on its next tick (`+0x668 = 6`, `+0xacc = 1`, `+0x53c` left alone). Types 2 and 3 tail-jump
  to `0x7c1e0` with their own end code instead.
- The call that matters arrives **before** the lights (`state=3`), leaving `-1` for the race that
  is about to start, so the first tick of the race already has nothing left. The elapsed stamps
  are correct at the green light (`+0x518 - +0x520` = 11 ms; the game logs the same value as
  `StartRace:<ms>`), which is why rewriting them is wrong: `GetRaceTime` (`0x5ee60`) subtracts
  `+0x584 + +0x57c + +0x520 + +0x51c` from `+0x518`, so stamping them negative-times the clock and
  the game ends the race itself.
- The timer finishes only through two branches into `0x7b9f0`: `0x7b95e` (`ecx < 1`, not mode 5)
  and `0x7b946` (mode 5, `ecx < -2000`, stored back as `0xfffff830`). Both need the remaining
  time at or below zero, which a race with 111 s left cannot reach.

Fixes, all in the payload's `indstate` module:

- `WAL_ALLS_INDRUN_RESET=1`: on entry to the racing state refund `+0x538` to the largest value
  seen for that race (the seeded budget, `0x7b2e0`). Nothing else is written.
- `WAL_ALLS_INDRUN_RETIRE=<ms>` (default 45 s): a detour on `0x5ede0` that drops the type-1 early
  cancel while the race is in state 4 and younger than the window; types 2/3, calls outside the
  racing state and later calls go to the original, so quitting still works. The prologue is
  `mov rax,[IndRun.dll+0x21c720]` (RIP-relative), so the trampoline re-encodes it as
  `mov rax, moffs64` and jumps back to `+0x5ede0+7`.
- The profile's `IndRun.dll!0x793a6:EB1B` patch is gone: it turned the goal range check into an
  unconditional jump, which made the `state = 5` store at `0x793a8` unreachable, so Story races
  could only ever end as a time-up. `0x7938c:7435` stays and still skips the null goal table.

Verified with a driven Story race: `Checkpoint 0 passed (time:79124)`, `Checkpoint 2 passed
(time:133469)`, then `state=5 end=0x0` and `RaceFinish ゲームモード: StoryMode 終了タイプ : eNORMAL`.
An unattended race now runs 125 s and ends at the clock (`state=6 end=0x1` at `rem=240`), and the
only `RetireRace` it issued was the pre-start `type=1` one, which passes through.

Season 3 comparison (`IndRun.dll` 2,174,464 B, PE 2024-01-19) shows the same code with a uniform
+8 field shift (`0x510` → `0x518`, `0x660` → `0x668`, `0x530` → `0x538`, `0x56c` → `0x574`,
`0x10d8`/`0x10e8` → `0x1100`/`0x1104`): `GetRaceTime`, the timer and the goal block are identical,
so none of the Season 5 workarounds are Season 5-specific bugs.

## Both race modes ending early: the same `RetireRace(1)` (fixed)

`cUE4Interface::RetireRace(eRetireRaceType)` (`IndRun.dll+0x5ede0`) type 1 runs
`movl $0xffffffff,0x538(%rax)`: it stores `-1` into the race's remaining time, and the clock updater
(`0x7b820`) then finishes the race on its next tick (`+0x668 = 6`, `+0xacc = 1`, `+0x53c` untouched),
which the game reports as `eNORMAL`. The game issues that call even when it does not mean to end the
race: seconds after the lights in Story, and about a minute in, mid-course, in Time Attack. The
payload filters it (`crates/payload-alls/src/indstate.rs`), so the race runs to its own end.

Story was fixed first (commit `c17e5e9`, section "Season 5 story races ending seconds after the
start" below): the filter's window was 45 s, which covered the cancel arriving 1-2 s after the lights,
and the budget refund (`WAL_ALLS_INDRUN_RESET=1`) put `+0x538` back to the value the game seeded.

Time Attack needed the same filter across the whole race (commit `dfe8983`). Its cancel arrives after
that 45 s window, so it passed straight through:

```
# before, WAL_ALLS_INDRUN_RETIRE: "45000"
indstate: RetireRace(type=1) state=4 race_age=58899ms rem=63504 -> passed through
indstate fast: t=502945ms state=6 finished=0 end=0x1 count=0
```

The race ended with 63 s still on the clock (`+0x538`). That is what "Time Attack ends early" looked
like — not a short base time: the race's own budget was ~122 s and its clock was healthy.

Policy now (`WAL_ALLS_INDRUN_RETIRE=1`, which the profile carries): a type-1 cancel is ignored while
the race is in the racing state **and** still has time on the clock (`+0x538 > 0`), in both modes.
Once the clock is spent the call goes to the original, so the normal end still works; type 2/3
cancels and calls outside the racing state were always passed through. `WAL_ALLS_INDRUN_RETIRE=<ms>`
keeps the old window for debugging, and leaving it unset turns the filter off. Every decision logs
the remaining time, so a run states its own verdict:

```
indstate: RetireRace(type=1) state=4 race_age=50998ms rem=71056 -> ignored
```

### Verified in game

| Run | Race kind | Cancel decision | Outcome |
|---|---|---|---|
| Story, unattended | `kind=4` | `race_age=12375ms rem=108128 -> ignored` | full 125 s budget, `state=6 end=0x1` at `rem=208` (clock expiry) |
| Time Attack, driven | `kind=1` | `race_age=50998ms rem=71056 -> ignored` | ran on to the goal: `state=5 finished=0 end=0x0` at t=284.5 s |
| Time Attack, before the fix | `kind=1` | `race_age=58899ms rem=63504 -> passed through` | `state=6 end=0x1` with 63 s of clock left |

Two things follow from the last two rows:

- a legitimate Time Attack finish does **not** go through `RetireRace(1)`: it takes the goal path
  (`0x793a8` stores `state = 5`, with `end = 0x0`), so ignoring type 1 for the whole race cannot
  block a real finish;
- `+0x5c8` in the `indstate clock:` dumps is the race kind: `1` is Time Attack, `4` is Story. That is
  how the two modes are told apart in the log without the game's own `Mode :` line (which only
  records the race being created).

The rest of the earlier sections is unchanged by this: the only budget write is still the green-light
refund of `+0x538`, and nothing rewrites the elapsed stamps (`GetRaceTime` stays consistent).

### Time Attack's checkpoint bonuses do work (the "still open" note this replaces was wrong)

Re-checked on a driven Time Attack after the retire fix: IndRun's player car registers progress and
the game extends the clock. From that run's log:

```
indrun: Actor:000000000E2E67C0 Standing(time:-200000)                       # at the lights
indrun: Actor:000000000E2E67C0 Checkpoint 0 passed (time:93859) (sectiontime:93859)
indrun: Actor:000000000E2E67C0 Checkpoint 1 passed (time:191497) (sectiontime:97638)
indstate clock: rem 30176 -> 39296 -> 62816 -> 86336 -> 109856 -> 127872
```

Five jumps against `+0x538` of +9120 / +23520 / +23520 / +23520 / +18016 ms: about 89 s added to a
~122 s base. Those are the checkpoint bonuses, and that race went on to the goal (`state=5
end=0x0`).

Timing detail, because it changes how the add works: all five jumps land within 2.3 s of
`Checkpoint 0 passed` (t=1794375086.4), and `Checkpoint 1 passed` (t=1794375184.0, 98 s later) adds
nothing. So the time is credited in one burst at the first checkpoint (the sections the car covered
while the tracking caught up), not one bonus per checkpoint. The race object's `bonus` and `pool`
fields stay 0 throughout, so the add does not go through them.

So the "Standing / no checkpoint / section 0" symptom in UPDATE and UPDATE 2 is **not** the current
behaviour. Between those runs and this one the profile was rebased onto upstream's Season 5 support,
which makes the three race-setting check values the constants IndRun expects (`0x6e23af` /
`0x6e241d`) and removes IndRun's date limit. With those the car's race state advances.

The payload's own control feed is **not** what does it: `indctl` (`WAL_ALLS_INDRUN_INPUT`) is not
enabled in the profile and logs nothing in these runs, so the progress comes from the game's own
`SetAccel` / `SetSteer` reaching IndRun once its checks pass — the opposite of the UPDATE-era
conclusion that the pedals never arrived.

Which piece is load-bearing, measured: with `WAL_ALLS_INDRUN_RESET` and `WAL_ALLS_INDRUN_RETIRE`
both off, a driven Time Attack (`kind=1`) reaches the racing state at t=96.7 s and ends `state=6
end=0x1` at t=147.5 s — 51 s of racing — with `rem=71664` still on the clock, and neither the retire
filter nor the budget refund armed in the log. So the retire filter is load-bearing for the race
surviving at all: the cancel returns as soon as it is off, which is the original symptom.

That run cannot say whether the race-setting checks alone drive progress, because the race dies at
51 s and the first checkpoint of a Time Attack only lands ~94 s in. Isolating progress needs the
other half: `WAL_ALLS_INDRUN_RESET` off with the retire filter on, which survives long enough to
reach checkpoint 0. Until that is run, read the Standing analysis in UPDATE as historical (it
describes the pre-rebase profile), not as the current mechanism.
