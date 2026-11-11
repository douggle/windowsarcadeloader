//! `WAL_ALLS_INDRUN_RECV=1`: log the packets the game publishes into IndRun.
//!
//! IndRun is Initial D THE ARCADE's authoritative race simulation and the game publishes the
//! player's car state to it as *packets*: `cUE4Interface::Recieve(int, void *, int)`
//! (`IndRun.dll+0x60860`) resolves the live session, takes the receiver object out of
//! `[session+0x118]+0x978` and tail-jumps to the real handler (`IndRun.dll+0x800bc0`), which
//! decodes a `u16 type` at `packet+0` and looks the message up in the `std::map` at
//! `receiver+0x78` by the `u32 id` at `packet+8`.
//!
//! Whether that channel carries anything under the loader is the open question: the sim car
//! reads as standing, no checkpoint is ever passed and the time-attack clock is never extended.
//! Two hooks answer it:
//!
//! - the 5-byte tail jump (`IndRun.dll+0x608bd`) becomes a jump to a stub that logs the packet
//!   (type, id, leading bytes, the receiver's message-map root) and then runs the real handler;
//! - the function's epilogue (`IndRun.dll+0x608c2`, `add rsp,0x28; ret`) is the target of its
//!   four `je` early-outs, so it is hooked too: a call that is dropped for want of a live session
//!   or receiver shows up as `dropped` instead of vanishing. Its stub restores the frame and
//!   then tail-jumps to the logger, which returns straight to `Recieve`'s caller.
//!
//! Both stubs live within ±2 GiB of IndRun (a `jmp rel32` reaches them) and end in an absolute
//! `jmp rax`, so they reach the payload wherever it is loaded. A stub is entered exactly as a
//! callee would be (`rsp ≡ 8 mod 16`) and never returns to it: it jumps to a Rust function that
//! itself returns to `Recieve`'s caller.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use wal_payload_common::log;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, PAGE_GUARD,
    PAGE_NOACCESS, VirtualAlloc, VirtualProtect, VirtualQuery,
};

/// `cUE4Interface::Recieve`'s tail jump `e9 <rel32>`, and the epilogue its early-outs jump to.
const TAIL_JUMP: usize = 0x608bd;
const EPILOGUE: usize = 0x608c2;
/// The real packet handler (`IndRun.dll+0x800bc0`), re-read from the `rel32` at install time.
static ORIG: AtomicUsize = AtomicUsize::new(0);
/// `receiver+0x78`: the `std::map<int32, handler>` the u32 at `packet+8` is looked up in.
const MESSAGE_MAP: usize = 0x78;
static CALLS: AtomicU32 = AtomicU32::new(0);
static DROPS: AtomicU32 = AtomicU32::new(0);
static LAST_KEY: AtomicU32 = AtomicU32::new(u32::MAX);

/// Is `[p, p+size)` committed and readable? (tracing foreign pointers can't fault this way)
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

unsafe fn bytes_are(p: usize, want: &[u8]) -> bool {
    (unsafe { readable(p, want.len()) })
        && want.iter().enumerate().all(|(i, b)| unsafe { std::ptr::read_volatile((p + i) as *const u8) } == *b)
}

/// Executable page within ±2 GiB of `base`, for the `jmp rel32` to reach.
unsafe fn alloc_near(base: usize) -> Option<usize> {
    let mut delta = 0x0100_0000usize;
    while delta < 0x6000_0000 {
        for cand in [base.checked_sub(delta), base.checked_add(delta)].into_iter().flatten() {
            let p = unsafe { VirtualAlloc(cand as *const _, 0x1000, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) };
            if !p.is_null() {
                return Some(p as usize);
            }
        }
        delta += 0x0100_0000;
    }
    None
}

/// `mov rax, <fn>; jmp rax` — reaches the payload from anywhere; `fn` returns to the caller.
unsafe fn write_stub(at: usize, f: *const ()) {
    let mut code = [0u8; 12];
    code[0] = 0x48;
    code[1] = 0xB8;
    code[2..10].copy_from_slice(&(f as u64).to_le_bytes());
    code[10] = 0xFF;
    code[11] = 0xE0;
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), at as *mut u8, code.len()) };
}

/// Replaces the 5 bytes at `at` with `jmp stub`, when the stub is in `rel32` range.
unsafe fn patch_jmp(at: usize, stub: usize) -> bool {
    let dist = stub as isize - (at + 5) as isize;
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

/// The delivered packet: entered as the handler is (`rcx` receiver, `rdx` packet, `r9d` actor).
unsafe extern "system" fn recv_hook(receiver: usize, packet: usize, rb: usize, actor: u32) {
    let n = CALLS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    if unsafe { readable(packet, 0x10) } {
        let ty = unsafe { std::ptr::read_unaligned(packet as *const u16) } as u32;
        let id = unsafe { std::ptr::read_unaligned((packet + 8) as *const u32) };
        let key = (ty << 16) ^ id;
        let changed = LAST_KEY.swap(key, Ordering::Relaxed) != key;
        if n <= 32 || changed || n % 600 == 0 {
            let mut bytes = String::new();
            if unsafe { readable(packet, 24) } {
                for b in unsafe { std::slice::from_raw_parts(packet as *const u8, 24) } {
                    bytes.push_str(&format!("{b:02x}"));
                }
            }
            let root = if unsafe { readable(receiver + MESSAGE_MAP, 8) } {
                unsafe { std::ptr::read_volatile((receiver + MESSAGE_MAP) as *const usize) }
            } else {
                0
            };
            log!("indrecv: #{n} actor={actor} type={ty:#06x} id={id:#010x} map_root={root:#x} bytes={bytes}");
        }
    } else if n <= 8 || LAST_KEY.swap(u32::MAX, Ordering::Relaxed) != u32::MAX {
        log!("indrecv: #{n} actor={actor} packet={packet:#x} (unreadable)");
    }
    let orig = ORIG.load(Ordering::Relaxed);
    if orig != 0 {
        let f: unsafe extern "system" fn(usize, usize, usize, u32) = unsafe { std::mem::transmute(orig) };
        unsafe { f(receiver, packet, rb, actor) }
    }
}

/// A call `Recieve` dropped: the register arguments as they still stand at its return.
unsafe extern "system" fn drop_hook(a: usize, b: u32, c: usize, d: u32) {
    let n = DROPS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    if n <= 8 || n % 600 == 0 {
        log!("indrecv: dropped #{n} a={a:#x} b={b:#x} c={c:#x} d={d:#x}");
    }
}

/// `base` is the loaded `IndRun.dll`. Installs the packet logger if `WAL_ALLS_INDRUN_RECV` is set.
pub(crate) fn install(base: usize) {
    if !std::env::var("WAL_ALLS_INDRUN_RECV").is_ok_and(|v| v.trim() == "1") {
        return;
    }
    unsafe {
        let at = base + TAIL_JUMP;
        if !unsafe { bytes_are(at, &[0xE9]) } {
            log!("indrecv: IndRun.dll+{TAIL_JUMP:#x} is not the expected tail jump, no packet logging");
            return;
        }
        let rel = unsafe { std::ptr::read_unaligned((at + 1) as *const i32) };
        let orig = ((at + 5) as isize).wrapping_add(rel as isize) as usize;
        ORIG.store(orig, Ordering::Relaxed);
        log!("indrecv: Recieve tail jump -> IndRun.dll+{:#x}", orig - base);
        let Some(stub) = (unsafe { alloc_near(base) }) else {
            log!("indrecv: no stub page within 2 GiB of IndRun.dll, no packet logging");
            return;
        };
        unsafe { write_stub(stub, recv_hook as *const ()) };
        if !unsafe { patch_jmp(at, stub) } {
            log!("indrecv: stub at {stub:#x} is out of rel32 range, no packet logging");
            return;
        }
        log!("indrecv: logging the packets the game publishes to IndRun (stub {stub:#x})");

        // the four `je` early-outs return through the epilogue: a drop must be visible too
        let ep = base + EPILOGUE;
        if unsafe { bytes_are(ep, &[0x48, 0x83, 0xC4, 0x28, 0xC3]) } {
            let stub2 = stub + 0x20;
            unsafe {
                // add rsp,0x28 (undo Recieve's frame) then jump to the logger
                std::ptr::write_volatile(stub2 as *mut u8, 0x48);
                std::ptr::write_volatile((stub2 + 1) as *mut u8, 0x83);
                std::ptr::write_volatile((stub2 + 2) as *mut u8, 0xC4);
                std::ptr::write_volatile((stub2 + 3) as *mut u8, 0x28);
                write_stub(stub2 + 4, drop_hook as *const ());
            }
            if unsafe { patch_jmp(ep, stub2) } {
                log!("indrecv: logging dropped Recieve calls (stub2 {stub2:#x})");
            } else {
                log!("indrecv: epilogue stub {stub2:#x} out of rel32 range");
            }
        } else {
            log!("indrecv: IndRun.dll+{EPILOGUE:#x} is not the expected epilogue, drops stay silent");
        }
    }
}
