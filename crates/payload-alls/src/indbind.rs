//! `WAL_ALLS_INDRUN_BIND=1`: log the callbacks the game registers on IndRun's race listener.
//!
//! Every `Bind*` export (`BindActorCarState`, `BindCheckPointPassed`, `BindRaceGoalEvent`,
//! `BindRaceFinishEvent`, `BindRaceTimerUpdater`, `BindUDPSender`, …) resolves the live race
//! through `IndRun.dll+0x756e0`, takes its listener at `[wrapper+0x118]` and stores the game's
//! `std::function` into one of that object's slots. The listener's constructor (`0x6f9a0`)
//! zeroes them, so a slot going from 0 to a pointer *is* a registration, and
//! `UnBindActorEvent` (`0x60750`) blanks `0x918`, `0x978` and `0x9a0` again.
//!
//! The binders assign the callback through a helper (no literal slot store in their bodies), so
//! this samples the slot array instead of hooking 25 entry points: while a race is racing, every
//! slot in `0x880..0xa80` is compared with the previous sample and a registration, rebind or
//! unbind is logged with its offset. That answers which callbacks the game actually hands IndRun
//! — the same question `Recieve` answered for packets, where the answer was "none".

use std::time::Duration;

use wal_payload_common::log;
use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery};

/// The listener's slot array: the constructor zeroes `0x910`, `0x950`, `0x978`, `0x980`, `0x988`
/// and `0x998`; `UnBindActorEvent` blanks `0x918`, `0x978` and `0x9a0`.
const SLOTS_FROM: usize = 0x880;
const SLOTS_TO: usize = 0xa80;
const RACING: u32 = 4;
/// `cUE4Interface`'s exported session lookup the binders call with the race wrapper.
const RESOLVE: usize = 0x756e0;

unsafe fn readable(p: usize, size: usize) -> bool {
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

/// A slot's name where the earlier analysis established it.
fn name(off: usize) -> &'static str {
    match off {
        0x950 => " (checkpoint passed)",
        0x978 => " (receive/UDP receiver)",
        0x918 | 0x9a0 => " (unbound by UnBindActorEvent)",
        _ => "",
    }
}

/// `base` is the loaded `IndRun.dll`. Starts the poller if `WAL_ALLS_INDRUN_BIND` is set.
pub(crate) fn install(base: usize) {
    if !std::env::var("WAL_ALLS_INDRUN_BIND").is_ok_and(|v| v.trim() == "1") {
        return;
    }
    let lookup: unsafe extern "system" fn(usize, i32) -> usize = unsafe { std::mem::transmute(base + RESOLVE) };
    let race = base + 0x21c720;
    std::thread::spawn(move || {
        log!("indbind: polling the race listener's callback slots while racing");
        let mut prev: Vec<(usize, usize)> = Vec::new();
        let mut live = false;
        loop {
            std::thread::sleep(Duration::from_millis(200));
            // the race state +0x668 via [[[singleton]+8]]+8, and its wrapper for the lookup
            let Some(p0) = (unsafe { rd_ptr(race) }) else { continue };
            let Some(p1) = (unsafe { rd_ptr(p0 + 8) }) else { continue };
            let Some(p2) = (unsafe { rd_ptr(p1) }) else { continue };
            let Some(p3) = (unsafe { rd_ptr(p2 + 8) }) else { continue };
            if !unsafe { readable(p3 + 0x668, 4) } {
                continue;
            }
            if unsafe { std::ptr::read_volatile((p3 + 0x668) as *const u32) } != RACING {
                if live {
                    live = false;
                    prev.clear();
                    log!("indbind: race left the racing state");
                }
                continue;
            }
            if !live {
                live = true;
                log!("indbind: racing, sampling the listener's callback slots");
            }
            // the binders resolve the actor wrapper through the export, then read its +0x118
            let wrapper = unsafe { lookup(p3, 0) };
            let Some(listener) = (unsafe { rd_ptr(wrapper + 0x118) }) else { continue };
            let mut now: Vec<(usize, usize)> = Vec::new();
            for off in (SLOTS_FROM..=SLOTS_TO).step_by(8) {
                let Some(v) = (unsafe { rd_ptr(listener + off) }) else { continue };
                now.push((off, v));
            }
            for &(off, v) in &now {
                match prev.iter().find(|(o, _)| *o == off) {
                    None => log!("indbind: registered +{off:#05x}{} = {v:#x}", name(off)),
                    Some((_, old)) if *old != v => log!("indbind: rebound +{off:#05x}{} {old:#x} -> {v:#x}", name(off)),
                    _ => {}
                }
            }
            for &(off, old) in &prev {
                if !now.iter().any(|(o, _)| *o == off) {
                    log!("indbind: unbound +{off:#05x}{} (was {old:#x})", name(off));
                }
            }
            prev = now;
        }
    });
}
