//! `WAL_ALLS_INDLOG=<rva>`: un-stub Initial D THE ARCADE's IndRun debug logger. In the release
//! `IndRun.dll` the logger `IndLog(const char *format, ...)` is a no-op (it stores its varargs
//! and returns), so none of its messages (`RaceSetting`, `Checkpoint %d passed (time/sectiontime)`,
//! `Standing`, course progress, ...) reach any log. This replaces it with a stub that formats the
//! message and writes it to the payload log, for debugging the Season 5 Time Attack timer.
//!
//! `<rva>` is the logger's offset in `IndRun.dll` (e.g. `0x1df0`). Installed by `indrun.rs` once
//! `IndRun.dll` is loaded. Modelled on `payload-seganu`'s `amlog` (`gamelog`).

use std::ffi::{c_char, c_void};

use wal_payload_common::log;
use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, VirtualProtect};

unsafe extern "C" {
    fn _vsnprintf(buf: *mut c_char, size: usize, fmt: *const c_char, args: *const c_void) -> i32;
}

// IndLog(format, ...): format in rcx, the variable arguments in rdx, r8, r9 (then the stack).
// The release build stubbed this logger to a no-op that preserves EVERY register; IndRun's
// callers rely on that (e.g. fcn 0x704b0 keeps `r10` across the call and dereferences it right
// after). So this stub saves/restores all volatile registers (rax, rcx, rdx, r8-r11, xmm0-5)
// around the call to the Rust formatter, and stores the varargs into the caller's home slots
// first so vsnprintf can walk them.
core::arch::global_asm!(
    ".globl {name}",
    "{name}:",
    "mov [rsp + 0x10], rdx",
    "mov [rsp + 0x18], r8",
    "mov [rsp + 0x20], r9",
    "push rax",
    "push rcx",
    "push rdx",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "sub rsp, 0x80",
    "movups [rsp + 0x20], xmm0",
    "movups [rsp + 0x30], xmm1",
    "movups [rsp + 0x40], xmm2",
    "movups [rsp + 0x50], xmm3",
    "movups [rsp + 0x60], xmm4",
    "movups [rsp + 0x70], xmm5",
    "mov rcx, [rsp + 0xA8]",
    "lea rdx, [rsp + 0xC8]",
    "call {log}",
    "movups xmm0, [rsp + 0x20]",
    "movups xmm1, [rsp + 0x30]",
    "movups xmm2, [rsp + 0x40]",
    "movups xmm3, [rsp + 0x50]",
    "movups xmm4, [rsp + 0x60]",
    "movups xmm5, [rsp + 0x70]",
    "add rsp, 0x80",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rdx",
    "pop rcx",
    "pop rax",
    "ret",
    name = sym indlog_stub,
    log = sym indlog_va,
);

unsafe extern "C" {
    fn indlog_stub();
}

// IndRun's logger fires on MANY messages, including high-frequency per-tick float/debug spam.
// Running vsnprintf on every call (a bad `%s` pointer, or mis-walked float varargs, faults) is
// what crashed the game. Keep only the checkpoint / section-progress messages, and only ever
// vsnprintf a format with plain integer/pointer conversions — anything with a float or string
// conversion is logged as its raw template instead (still tells us it fired, and the values we
// care about — checkpoint index, time, sectiontime — are all %d/%p).
const KEEP: [&str; 10] = [
    "passed",
    "heckPoint",
    "heckpoint",
    "Standing",
    "setCurrentSection",
    "borted",
    "StartRace",
    "oalIn",
    // the course setting IndRun receives (AddCourseSetting): the .pa4 path names it then loads
    "PathName",
    // the loader's "Load Path %s.": the file it opens (app root + the course's path name)
    "Load Path",
];

// A `%s`/`%ls` conversion reads its argument as a pointer-to-string, which can fault; those
// formats are logged as their raw template instead. Floats (`%f`) are now safe to format (the
// stub preserves registers), so the per-tick `PlayerPointer` progress line can be read.
fn string_fmt(t: &str) -> bool {
    t.contains("%s") || t.contains("%ls") || t.contains("%S")
}

/// Formats whose `%s` arguments are C strings IndRun itself dereferences (the course setting's
/// path names and the path loader's file name): those are formatted instead of logged as their
/// template, which is what tells us whether the game sends usable course paths at all.
fn known_string_arg(t: &str) -> bool {
    (t.contains("PathName") || t.contains("Load Path")) && !t.contains("%ls") && !t.contains("%S")
}

static TICK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

extern "C" fn indlog_va(fmt: *const c_char, args: *const c_void) {
    if fmt.is_null() {
        return;
    }
    let template = unsafe { std::ffi::CStr::from_ptr(fmt) }.to_string_lossy();
    // The progress/debug lines ("nbfPointer:..", "dV %.3f ..", "racetime %d ..") fire every
    // tick; throttle them to ~3/second so the log stays readable but shows whether progress and
    // the race-update advance.
    if template.contains("Pointer") || template.contains("racetime") || template.contains("dV ") {
        if TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 20 != 0 {
            return;
        }
    } else if !KEEP.iter().any(|k| template.contains(k)) {
        return;
    }
    if string_fmt(&template) && !known_string_arg(&template) {
        log!("indrun: {}", template.trim_end());
        return;
    }
    let mut buf = [0 as c_char; 1024];
    unsafe { _vsnprintf(buf.as_mut_ptr(), buf.len() - 1, fmt, args) };
    let msg = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
    log!("indrun: {}", msg.trim_end());
}

/// Replaces the function at `base + rva` by a jump to `stub`.
unsafe fn jump_to(base: usize, rva: usize, stub: usize) {
    let at = base + rva;
    let mut old = 0;
    unsafe {
        VirtualProtect(at as *const c_void, 5, PAGE_EXECUTE_READWRITE, &mut old);
        *(at as *mut u8) = 0xE9;
        std::ptr::write_unaligned((at + 1) as *mut i32, stub.wrapping_sub(at + 5) as i32);
        VirtualProtect(at as *const c_void, 5, old, &mut old);
    }
}

fn parse_hex(s: &str) -> Option<usize> {
    usize::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

/// `base` is the loaded `IndRun.dll`. Installs the logger un-stub if `WAL_ALLS_INDLOG` is set.
///
/// The patch waits for IndRun's first live race: its boot (`ABootSequence::SetupRecords`) logs
/// course paths that do not exist yet, and formatting one of those `%s` arguments faults the
/// game into its error mode ("Error 4104 / Unexpected Error Occurred").
pub(crate) fn install(base: usize) {
    let rva = std::env::var("WAL_ALLS_INDLOG").ok().and_then(|v| parse_hex(&v)).filter(|rva| *rva != 0);
    if std::env::var("WAL_ALLS_INDLOG_STATE").is_ok_and(|v| v.trim() == "1") {
        poll_state(base, rva);
    } else if let Some(rva) = rva {
        // no race state poller to wait on: patch once the boot sequence is over
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(60));
            unsafe { jump_to(base, rva, indlog_stub as *const () as usize) };
            log!("indlog: IndRun log function un-stubbed at IndRun.dll+{rva:#x}");
        });
    }
}

/// Is `[p, p+size)` committed and readable? (VirtualQuery-guarded, so polling foreign pointers
/// can't fault.)
unsafe fn readable(p: usize, size: usize) -> bool {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery};
    if p < 0x10000 || p >= 0x8000_0000_0000 {
        return false;
    }
    let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let n = unsafe { VirtualQuery(p as *const c_void, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
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

/// The race object `+0x668` (race state) via the singleton chain
/// `[[[*(base+0x21c720)]+8]] [+8] + 0x668`, or None when no race is live.
unsafe fn read_state(singleton: usize) -> Option<u32> {
    let p0 = unsafe { rd_ptr(singleton) }?;
    let p1 = unsafe { rd_ptr(p0 + 8) }?;
    let p2 = unsafe { rd_ptr(p1) }?;
    let p3 = unsafe { rd_ptr(p2 + 8) }?;
    unsafe { readable(p3 + 0x668, 4) }.then(|| unsafe { std::ptr::read_volatile((p3 + 0x668) as *const u32) })
}

/// The loaded course path-point count (GetPathPointsNum): `[[[*(base+0x21c358)]+0x48]+0x10]`.
/// 0 (or None) means IndRun has no course path, so the car cannot be projected onto the course
/// and section progress can never advance.
unsafe fn read_path_points(path_singleton: usize) -> Option<i32> {
    let p0 = unsafe { rd_ptr(path_singleton) }?;
    let p1 = unsafe { rd_ptr(p0 + 0x48) }?;
    unsafe { readable(p1 + 0x10, 4) }.then(|| unsafe { std::ptr::read_volatile((p1 + 0x10) as *const i32) })
}

/// The accel (`+8`, u8) and steer (`+0xc`, i8) the game feeds IndRun via SetAccel/SetSteer,
/// stored in the control object at `[base+0x21c718]`. None when it isn't live.
unsafe fn read_controls(ctrl_singleton: usize) -> Option<(u8, i8)> {
    let ctrl = unsafe { rd_ptr(ctrl_singleton) }?;
    unsafe { readable(ctrl + 8, 8) }.then(|| {
        let accel = unsafe { std::ptr::read_volatile((ctrl + 8) as *const u8) };
        let steer = unsafe { std::ptr::read_volatile((ctrl + 0xc) as *const i8) };
        (accel, steer)
    })
}

/// Polls the IndRun race state `+0x668` (logged on change) and periodically samples the accel/
/// steer the game actually feeds IndRun — to see whether the race reaches state 4 (racing) and
/// whether the car controls reach IndRun's sim at all (if accel stays 0 while driving, the
/// controls never reach IndRun → its car never moves → "Standing" → no progress).
fn poll_state(base: usize, rva: Option<usize>) {
    let singleton = base + 0x21c720;
    let ctrl_singleton = base + 0x21c718;
    let path_singleton = base + 0x21c358;
    std::thread::spawn(move || {
        let mut last: i64 = -2;
        let mut patched = rva.is_none();
        let mut last_path: i64 = -2;
        let mut n: u32 = 0;
        let mut accel_max: u8 = 0;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let now = unsafe { read_state(singleton) }.map_or(-1, |s| s as i64);
            if now != last {
                last = now;
                if now < 0 {
                    log!("indrun: [state] no live race");
                } else {
                    log!("indrun: [state] race state +0x668 = {now}");
                }
            }
            if !patched && now >= 0 {
                if let Some(rva) = rva {
                    unsafe { jump_to(base, rva, indlog_stub as *const () as usize) };
                    log!("indlog: IndRun log function un-stubbed at IndRun.dll+{rva:#x}");
                }
                patched = true;
            }
            let path = unsafe { read_path_points(path_singleton) }.map_or(-1, |c| c as i64);
            if path != last_path {
                last_path = path;
                log!("indrun: [path] course path points = {path}");
            }
            if let Some((accel, steer)) = unsafe { read_controls(ctrl_singleton) } {
                accel_max = accel_max.max(accel);
                n += 1;
                // ~ once per second, report the current + peak accel and steer IndRun received
                if n % 5 == 0 {
                    log!("indrun: [ctrl] accel={accel} (peak {accel_max}) steer={steer}");
                }
            }
        }
    });
}
