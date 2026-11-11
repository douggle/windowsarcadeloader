//! `WAL_ALLS_INDRUN_STATE=1`: poll IndRun's simulated car state while a race is live.
//!
//! The Season 5 Time Attack investigation had no way to tell whether IndRun's own car simulation
//! moves: `BindActorCarState` hands the sim's car state to the game every tick, but nothing in
//! the payload can see it. The plugin exports exactly that state as
//! `cUE4Interface::FindCarStateInfo(int)` (`IndRun.dll+0x60c90`, a struct returned through the
//! hidden pointer in `rcx`), so this samples it and logs the leading fields whenever they change.
//! A stationary sim (every sample identical) with the game's car moving on screen means the race
//! progress feed is broken before the checkpoint code; samples that change mean the sim runs and
//! the missing piece is checkpoint/course progress instead.
//!
//! Only called while a race object exists (`[IndRun.dll+0x21c358]` non-null): the export walks
//! the plugin's race state and has nothing to read before that.

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use wal_payload_common::log;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc, VirtualProtect,
};

/// `stCarStateInfo __cdecl cUE4Interface::FindCarStateInfo(int)`: the hidden return pointer in
/// `rcx`, the actor id in `edx`; the struct is ~0x174 bytes (seen in `0x60cb0`'s stores).
type FindCarStateInfo = unsafe extern "system" fn(*mut u8, i32) -> *mut u8;

unsafe fn readable(p: usize, size: usize) -> bool {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery};
    if p < 0x10000 || p >= 0x8000_0000_0000 {
        return false;
    }
    let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let n = unsafe { VirtualQuery(p as *const _, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
    if n == 0 || mbi.State != MEM_COMMIT || (mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD)) != 0 {
        return false;
    }
    p + size <= mbi.BaseAddress as usize + mbi.RegionSize
}

unsafe fn rd_ptr(p: usize) -> Option<usize> {
    if !unsafe { readable(p, 8) } {
        return None;
    }
    let v = unsafe { std::ptr::read_volatile(p as *const usize) };
    (v >= 0x10000 && v < 0x8000_0000_0000).then_some(v)
}

/// The race state `+0x668` (`[[[*(base+0x21c720)]+8]][+8]+0x668`), the chain the payload's state
/// poller uses: 4 is racing. Anything else (no race, start fill, verdict) is not sampled.
unsafe fn race_object(race: usize) -> Option<usize> {
    let p0 = unsafe { rd_ptr(race) }?;
    let p1 = unsafe { rd_ptr(p0 + 8) }?;
    let p2 = unsafe { rd_ptr(p1) }?;
    unsafe { rd_ptr(p2 + 8) }
}

unsafe fn read_state(race: usize) -> Option<u32> {
    let p3 = unsafe { race_object(race) }?;
    unsafe { readable(p3 + 0x668, 4) }.then(|| unsafe { std::ptr::read_volatile((p3 + 0x668) as *const u32) })
}

/// `WAL_ALLS_INDRUN_CLOCK=1`: dump the words around the race state while racing, so the
/// remaining-time field can be found by watching which one counts down.
fn clock_dump() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("WAL_ALLS_INDRUN_CLOCK").is_ok_and(|v| v.trim() == "1"))
}

/// `WAL_ALLS_INDRUN_TIME=<ms>`: keep the race clock from expiring by topping the race's remaining
/// time (`+0x538`) up to at least that many milliseconds while racing. The clock updater at
/// `0x7b820` subtracts the elapsed race time from `+0x538` every tick and, at zero, finishes the
/// race itself (`+0x668 = 6`, `+0xacc = 1`, `+0x53c` untouched), which the game reports as a
/// normal finish with no goal data. The budget is set once by `0x7b2e0` (`+0x578` latch) as the
/// race setting's seconds plus a per-kind grace; a Story race gets no base time, only that grace.
/// Writing `+0x518` instead is wrong: it is the base the elapsed time is subtracted from, so
/// raising it makes the clock expire sooner.
fn time_base() -> Option<u32> {
    static VALUE: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("WAL_ALLS_INDRUN_TIME")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|v| *v > 1000)
    })
}

/// `WAL_ALLS_INDRUN_FAST=<ms>`: sample the race object's finish decision every few milliseconds
/// (default 20) and log each change, to name the path that ends a race. `+0x668` is the race
/// state (6 is finished), `+0x53c` the finished flag both finish helpers set, `+0xacc` the end
/// type they hand the game, and `+0x1028` the "player actor missing" grace counter whose 180th
/// period finishes the race with end type 10. A counter climbing to 180 names that timeout;
/// `+0x1028` staying low while `+0x53c` turns on names one of the other state-6 sites.
fn fast_period() -> Option<u64> {
    static VALUE: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        let v = std::env::var("WAL_ALLS_INDRUN_FAST").ok()?;
        let v = v.trim().to_string();
        if v == "1" {
            return Some(20);
        }
        v.parse::<u64>().ok().map(|ms| ms.clamp(5, 500))
    })
}

/// The fast sampler: state, finished flag, end type and grace counter, on every change. The race
/// object is read outside the racing state on purpose (the finish transition is the point), and a
/// missing object is reported so the race's lifetime is in the log too.
fn spawn_fast(race: usize, period: u64) {
    std::thread::spawn(move || {
        log!("indstate: sampling +0x668/+0x53c/+0xacc/+0x1028 every {period} ms");
        let t0 = std::time::Instant::now();
        let mut prev: Option<[u32; 4]> = None;
        let mut prev_cut: Option<[i32; 4]> = None;
        let mut clock_at = std::time::Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(period));
            let obj = unsafe { race_object(race) };
            let now = obj.and_then(|p3| unsafe {
                readable(p3 + 0x1028, 4).then(|| {
                    let word = |off: usize| std::ptr::read_volatile((p3 + off) as *const u32);
                    let byte = |off: usize| std::ptr::read_volatile((p3 + off) as *const u8) as u32;
                    [word(0x668), byte(0x53c), word(0xacc), word(0x1028)]
                })
            });
            // the clock the timer updater counts down, twice a second while racing: `+0x538` is set
            // once from the race setting (`+0x578` latch), `+0x550` is the same value in seconds
            if now.is_some_and(|n| n[0] == RACING)
                && clock_at.elapsed() >= Duration::from_millis(500)
                && let Some(p3) = obj
                && unsafe { readable(p3 + 0x580, 4) }
            {
                clock_at = std::time::Instant::now();
                unsafe {
                    let word = |off: usize| std::ptr::read_volatile((p3 + off) as *const u32);
                    let byte = |off: usize| std::ptr::read_volatile((p3 + off) as *const u8) as u32;
                    log!(
                        "indstate clock: rem={} disp={} init={} kind={} frame={} mark={} bonus={} pool={}",
                        word(0x538) as i32,
                        word(0x550) as i32,
                        byte(0x578),
                        word(0x5c8) as i32,
                        word(0x580) as i32,
                        word(0x530) as i32,
                        byte(0x664),
                        word(0x548) as i32
                    );
                }
            }
            // The two one-shot cut inputs: `+0x1100` forces the remaining time to 15 ms (which
            // finishes the race on the next tick, long before the budget runs out) and `+0x1104`
            // forces the elapsed time to 100 s. `+0x520` is the stored race start time and
            // `+0x57c` the elapsed-time stamp, so a mid-race re-stamp shows up here too.
            let cut = obj.and_then(|p3| unsafe {
                readable(p3 + 0x1104, 4).then(|| {
                    let word = |off: usize| std::ptr::read_volatile((p3 + off) as *const u32) as i32;
                    [word(0x1100), word(0x1104), word(0x520), word(0x57c)]
                })
            });
            if cut != prev_cut {
                let ms = t0.elapsed().as_millis();
                match cut {
                    Some([a, b, c, d]) => log!(
                        "indstate cut: t={ms}ms +0x1100={a} +0x1104={b} +0x520={c} +0x57c={d}"
                    ),
                    None => log!("indstate cut: t={ms}ms unreadable"),
                }
                prev_cut = cut;
            }
            if now == prev {
                continue;
            }
            let ms = t0.elapsed().as_millis();
            match now {
                Some([state, finished, end, count]) => log!(
                    "indstate fast: t={ms}ms state={state} finished={finished} end={end:#x} count={count}"
                ),
                None => log!("indstate fast: t={ms}ms no race object"),
            }
            prev = now;
        }
    });
}

const RACING: u32 = 4;

/// `cUE4Interface::RetireRace(eRetireRaceType)` (`IndRun.dll+0x5ede0`). Type 1 takes the
/// `sub edx,1; je 0x5ee16` path, which stores `-1` into the remaining race time (`+0x538`); the
/// clock updater then finishes the race on its next tick (`+0x668 = 6`, `+0xacc = 1`) and the
/// game reports `eNORMAL`. The game calls it seconds after the lights go out when the car has not
/// driven off, which is what ended every unattended race. Types 2 and 3 tail-jump to `0x7c1e0`
/// with their own end code and are not touched.
const RETIRE_RVA: usize = 0x5ede0;
/// The prologue is `mov rax,[IndRun.dll+0x21c720]` (7 bytes, RIP-relative), so the trampoline
/// re-encodes it as `mov rax, moffs64` with the absolute address instead of copying the bytes.
const RETIRE_PROLOGUE: usize = 7;

static RETIRE_ORIG: AtomicUsize = AtomicUsize::new(0);
static RACE_STATE: AtomicU32 = AtomicU32::new(0);
/// When the race entered the racing state, in milliseconds since the payload started.
static LIGHTS_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// `WAL_ALLS_INDRUN_RETIRE=<ms>`: pass `RetireRace(1)` through only once the race has been racing
/// for that long, so the early cancel is ignored but quitting afterwards works. `1` means the
/// 45 s default.
fn retire_window() -> Option<u64> {
    static VALUE: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        let v = std::env::var("WAL_ALLS_INDRUN_RETIRE").ok()?;
        let v = v.trim().to_string();
        if v == "1" {
            return Some(45_000);
        }
        v.parse::<u64>().ok().map(|ms| ms.clamp(1_000, 600_000))
    })
}

unsafe extern "system" fn retire_hook(this: usize, kind: u32) {
    let state = RACE_STATE.load(Ordering::Relaxed);
    let lights = LIGHTS_MS.load(Ordering::Relaxed);
    let age = if lights == 0 { 0 } else { now_ms().saturating_sub(lights) };
    let window = retire_window().unwrap_or(0);
    let ignore = kind == 1 && state == RACING && lights != 0 && age < window;
    log!(
        "indstate: RetireRace(type={kind}) state={state} race_age={age}ms -> {}",
        if ignore { "ignored" } else { "passed through" }
    );
    if ignore {
        return;
    }
    let orig: unsafe extern "system" fn(usize, u32) =
        unsafe { std::mem::transmute(RETIRE_ORIG.load(Ordering::Relaxed)) };
    unsafe { orig(this, kind) }
}

/// Executable page within ±2 GiB of `base`, for the entry `jmp rel32` to reach.
unsafe fn alloc_near(base: usize) -> Option<usize> {
    let mut delta = 0x0100_0000usize;
    while delta < 0x6000_0000 {
        for cand in [base.checked_sub(delta), base.checked_add(delta)].into_iter().flatten() {
            let p = unsafe {
                VirtualAlloc(cand as *const _, 0x1000, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE)
            };
            if !p.is_null() {
                return Some(p as usize);
            }
        }
        delta += 0x0100_0000;
    }
    None
}

/// `mov rax, <f>; jmp rax`: enters `f` with the caller's registers untouched except `rax`.
unsafe fn write_jump_stub(at: usize, f: usize) {
    let mut code = [0u8; 12];
    code[0] = 0x48;
    code[1] = 0xB8;
    code[2..10].copy_from_slice(&(f as u64).to_le_bytes());
    code[10] = 0xFF;
    code[11] = 0xE0;
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), at as *mut u8, code.len()) };
}

unsafe fn patch_entry(at: usize, to: usize) -> bool {
    let dist = to as isize - (at + 5) as isize;
    if dist < i32::MIN as isize || dist > i32::MAX as isize {
        return false;
    }
    let mut old = 0;
    if unsafe { VirtualProtect(at as *const _, 5, PAGE_EXECUTE_READWRITE, &mut old) } == 0 {
        return false;
    }
    unsafe {
        std::ptr::write_volatile(at as *mut u8, 0xE9);
        std::ptr::write_unaligned((at + 1) as *mut i32, dist as i32);
        VirtualProtect(at as *const _, 5, old, &mut old);
    }
    true
}

/// `base` is the loaded `IndRun.dll`, `race` the address of `DAT_18021c720`.
pub(crate) fn install_retire_filter(base: usize, race: usize) {
    let Some(window) = retire_window() else { return };
    let entry = base + RETIRE_RVA;
    if !unsafe { readable(entry, RETIRE_PROLOGUE) } {
        log!("indstate: RetireRace filter not installed, +{RETIRE_RVA:#x} unreadable");
        return;
    }
    let head = unsafe { std::slice::from_raw_parts(entry as *const u8, RETIRE_PROLOGUE) };
    if head[0] != 0x48 || head[1] != 0x8B || head[2] != 0x05 {
        log!("indstate: RetireRace filter not installed, unexpected prologue {head:02x?}");
        return;
    }
    let Some(page) = (unsafe { alloc_near(base) }) else {
        log!("indstate: RetireRace filter not installed, no stub page near IndRun.dll");
        return;
    };
    unsafe {
        // trampoline: mov rax, moffs64(race); jmp entry+7 — runs the replaced prologue, then the
        // rest of the real function
        let tramp = page;
        let mut t = [0u8; 15];
        t[0] = 0x48;
        t[1] = 0xA1;
        t[2..10].copy_from_slice(&(race as u64).to_le_bytes());
        let back = (entry + RETIRE_PROLOGUE) as isize - (tramp + 15) as isize;
        t[10] = 0xE9;
        t[11..15].copy_from_slice(&(back as i32).to_le_bytes());
        std::ptr::copy_nonoverlapping(t.as_ptr(), tramp as *mut u8, t.len());

        let stub = page + 0x40;
        write_jump_stub(stub, retire_hook as *const () as usize);
        RETIRE_ORIG.store(tramp, Ordering::Relaxed);
        if !patch_entry(entry, stub) {
            log!("indstate: RetireRace filter not installed, entry patch out of range");
            return;
        }
    }
    std::thread::spawn(move || {
        log!("indstate: RetireRace filter armed (type 1 ignored for the first {window} ms of a race)");
        let mut last = u32::MAX;
        loop {
            std::thread::sleep(Duration::from_millis(10));
            let state = unsafe { read_state(race) }.unwrap_or(0);
            if state == RACING && last != RACING {
                LIGHTS_MS.store(now_ms().max(1), Ordering::Relaxed);
            }
            if state != last {
                last = state;
            }
            RACE_STATE.store(state, Ordering::Relaxed);
        }
    });
}

/// `WAL_ALLS_INDRUN_RESET=1`: refund the race budget on the green light so the clock starts full.
///
/// A Story race reaches the lights with `+0x538` (the remaining race time) already spent, seen at
/// `-1`, so the timer at `0x7b820` finishes the race seconds after the start (`+0x668 = 6`,
/// `+0xacc = 1`). The elapsed-time stamps themselves are correct at that point (`+0x518 - +0x520`
/// is only a few milliseconds), so this leaves them alone and only puts `+0x538` back to the
/// largest value seen for the race, which is the budget the game seeded for it.
///
/// Rewriting the stamps instead is wrong: `GetRaceTime` (`0x5ee60`) subtracts
/// `+0x584 + +0x57c + +0x520 + +0x51c` from `+0x518`, so zeroing `+0x520` while stamping
/// `+0x57c`/`+0x584` with the current clock makes the time the game reads negative, and it ends
/// the race itself.
fn reset_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("WAL_ALLS_INDRUN_RESET").is_ok_and(|v| v.trim() == "1"))
}

fn spawn_reset(race: usize) {
    std::thread::spawn(move || {
        log!("indstate: race budget refund armed (top-up on entry to state 4)");
        let mut last: Option<u32> = None;
        let mut seed = 0i32;
        loop {
            std::thread::sleep(Duration::from_millis(10));
            let Some(p3) = (unsafe { race_object(race) }) else {
                last = None;
                seed = 0;
                continue;
            };
            if !unsafe { readable(p3 + 0x590, 4) } {
                last = None;
                continue;
            }
            unsafe {
                let word = |off: usize| std::ptr::read_volatile((p3 + off) as *const i32);
                let state = word(0x668) as u32;
                if state <= RACING && word(0x538) > seed {
                    seed = word(0x538);
                }
                if state == RACING && last != Some(RACING) {
                    let (base, stamp, synced) = (word(0x518), word(0x57c), word(0x584));
                    let (start, extra, mode5) = (word(0x520), word(0x51c), word(0x52c));
                    let rem = word(0x538);
                    if seed > rem {
                        std::ptr::write_volatile((p3 + 0x538) as *mut i32, seed);
                    }
                    log!(
                        "indstate: green light base={base} start={start} stamp={stamp} synced={synced} extra={extra} mode5={mode5} elapsed={} rem {rem}->{}",
                        base - stamp - mode5 - start - extra,
                        word(0x538)
                    );
                }
                last = Some(state);
            }
        }
    });
}

unsafe fn read_ptr(p: usize) -> usize {
    if unsafe { readable(p, 8) } {
        unsafe { std::ptr::read_volatile(p as *const usize) }
    } else {
        0
    }
}

/// `base` is the loaded `IndRun.dll`. Starts the poller if `WAL_ALLS_INDRUN_STATE` is set.
pub(crate) fn init(base: usize) {
    let state_log = std::env::var("WAL_ALLS_INDRUN_STATE").is_ok_and(|v| v.trim() == "1");
    let fast = fast_period();
    let reset = reset_enabled();
    if !state_log && time_base().is_none() && !clock_dump() && fast.is_none() && !reset {
        return;
    }
    let find: FindCarStateInfo = unsafe { std::mem::transmute(base + 0x60c90) };
    let singleton = base + 0x21c358;
    let race = base + 0x21c720;
    if let Some(period) = fast {
        spawn_fast(race, period);
    }
    if reset {
        spawn_reset(race);
    }
    install_retire_filter(base, race);
    std::thread::spawn(move || {
        log!("indstate: polling FindCarStateInfo(0) while a race is live");
        let mut buf = [0u8; 0x180];
        let mut prev = [0u8; 0x180];
        let mut wide = vec![0u8; 0x800];
        let mut wide_prev = vec![0u8; 0x800];
        let mut live = false;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            // only while the race is actually racing: FindCarStateInfo walks IndRun's live
            // actor table, which does not exist (and must not be walked) outside it
            let racing = unsafe { read_state(race) } == Some(RACING)
                && unsafe { read_ptr(singleton) } != 0;
            if !racing {
                if live {
                    live = false;
                    log!("indstate: race left the racing state");
                }
                continue;
            }
            if !live {
                live = true;
                log!("indstate: racing, sampling the sim car state");
            }
            buf.fill(0);
            unsafe { find(buf.as_mut_ptr(), 0) };
            let diff = buf.iter().zip(prev.iter()).filter(|(a, b)| a != b).count();
            // the struct is zeroed by the export when it has nothing: only report a live state
            if buf.iter().any(|&b| b != 0) && diff > 0 {
                let words: Vec<String> = buf[..0x40]
                    .chunks_exact(4)
                    .map(|c| format!("{:08x}", u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
                    .collect();
                log!("indstate: car state {diff} bytes changed: {}", words.join(" "));
            }
            prev = buf;
            // never let the race clock reach zero: the updater finishes the race when it does
            if let Some(ms) = time_base()
                && let Some(p3) = unsafe { race_object(race) }
                && unsafe { readable(p3 + 0x538, 4) }
            {
                let cur = unsafe { std::ptr::read_volatile((p3 + 0x538) as *const i32) };
                if cur < ms as i32 {
                    unsafe { std::ptr::write_volatile((p3 + 0x538) as *mut i32, ms as i32) };
                    log!("indstate: race time left +0x538 {cur} -> {ms}");
                }
            }
            // the race object's first 0x800 bytes: report only the words that changed since the
            // last sample, so the remaining-time countdown stands out from the static tables
            if clock_dump()
                && let Some(p3) = unsafe { race_object(race) }
            {
                for (i, b) in wide.iter_mut().enumerate() {
                    let a = p3 + i;
                    *b = if unsafe { readable(a, 1) } { unsafe { std::ptr::read_volatile(a as *const u8) } } else { 0 };
                }
                let word = |b: &[u8], off: usize| u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]);
                let mut changed = Vec::new();
                for off in (0..wide.len()).step_by(4) {
                    let (now, before) = (word(&wide, off), word(&wide_prev, off));
                    if now != before {
                        changed.push(format!("+0x{off:03x}={now:08x}"));
                    }
                }
                wide_prev.copy_from_slice(&wide);
                if !changed.is_empty() {
                    log!("indstate: race object changes: {}", changed.join(" "));
                }
            }
        }
    });
}
