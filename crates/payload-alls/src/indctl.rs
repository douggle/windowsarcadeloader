//! Feed IndRun's physics sim the player's car controls each tick.
//!
//! IndRun is Initial D THE ARCADE's authoritative car simulation: the game feeds it the pedal and
//! wheel through its `SetAccel`/`SetBrake`/`SetSteer`/`SetShift` exports and reads the simulated
//! car back for the race (progress, checkpoints, the time-attack clock). Under the loader the game
//! never forwards them — IndRun receives accel/steer = 0 for the whole race (confirmed live), so
//! its car stays stationary, every actor reads as "Standing", no checkpoint registers and the
//! clock is never extended → early TIME UP, even though the game's own physics move the car on
//! screen.
//!
//! This writes the launcher's real controls straight into IndRun's control object at
//! `[IndRun.dll+0x21c718]` — `+8` accel (u8), `+0xa` brake (u8), `+0xc` steer (i8), `+0xe` shift
//! (i8), the same fields `SetAccel`/`SetBrake`/`SetSteer`/`SetShift` write — bypassing the broken
//! forward. Enable with `WAL_ALLS_INDRUN_INPUT`.

use std::time::Duration;

use wal_payload_common::{hshifter, input, log};
use wal_protocol::Axis;
use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery};

/// Is `[p, p+size)` committed and accessible? (so writing IndRun's control object can't fault
/// before a race — and its object — exists).
unsafe fn committed(p: usize, size: usize) -> bool {
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

/// `base` is the loaded IndRun.dll. Starts the control-feed thread if `WAL_ALLS_INDRUN_INPUT` is set.
pub(crate) fn init(base: usize) {
    if std::env::var("WAL_ALLS_INDRUN_INPUT").is_err() {
        return;
    }
    let singleton = base + 0x21c718;
    std::thread::spawn(move || {
        log!("indctl: feeding IndRun car controls from the launcher input");
        loop {
            // ~250 Hz, well above IndRun's 60 Hz sim tick, so the fresh value is read each tick
            std::thread::sleep(Duration::from_millis(4));
            // control object = *[IndRun.dll+0x21c718]; null until a race is live
            if !unsafe { committed(singleton, 8) } {
                continue;
            }
            let ctrl = unsafe { std::ptr::read_volatile(singleton as *const usize) };
            if !unsafe { committed(ctrl + 8, 8) } {
                continue;
            }
            let p1 = input(0);
            let accel = (p1.axis_u16(Axis::Accel) >> 8) as u8;
            let brake = (p1.axis_u16(Axis::Brake) >> 8) as u8;
            let steer = (((p1.axis_u16(Axis::LeftX) as i32) - 0x8000) >> 8).clamp(-128, 127) as i8;
            let shift = hshifter::current_gear() as i8;
            unsafe {
                std::ptr::write_volatile((ctrl + 0x8) as *mut u8, accel);
                std::ptr::write_volatile((ctrl + 0xa) as *mut u8, brake);
                std::ptr::write_volatile((ctrl + 0xc) as *mut i8, steer);
                std::ptr::write_volatile((ctrl + 0xe) as *mut i8, shift);
            }
        }
    });
}
