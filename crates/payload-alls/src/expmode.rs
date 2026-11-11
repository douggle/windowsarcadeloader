//! `WAL_ALLS_EXPMODE=1`: log the ExpansionMode content lookups the story race performs.
//!
//! At boot the ExpansionModeSubsystem fails to load all five expansion DataTables
//! (`拡張モード: ContentId: Nのロードに失敗！`), so its content container is empty. At `StartRace`
//! the race flow then resolves the selected content through `Exe+0x407fa740`, gets the
//! `0xFFFFFFFF` "absent" sentinel back, logs `指定されたコンテンツの… ContentId:-1, MapId:-1,
//! MapPositionId:-1`, never finds the race-start-sequence widget, and finishes the race
//! `eNORMAL` a few seconds in.
//!
//! This hooks the *call site* of that resolver — `Exe+0x7f9d4f`, the `call` at the end of the
//! block that builds the query struct with `ContentId = -1` — so every story-race lookup shows
//! the key it searched for and the container's own counters. The stub only writes the argument
//! registers into a page of our own with RIP-relative stores, then tail-jumps to the resolver;
//! the payload reads the page from its own thread, so no indirect branch ever leaves the image
//! (CFG-safe) and the game never calls into the payload.

use std::time::Duration;

use wal_payload_common::{iat, log};
use windows_sys::Win32::System::Memory::{VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE};

/// `call Exe+0x407fa740` (`e8 rel32`), right after the `ContentId = -1` struct is built.
const HOOK_RVA: usize = 0x7f9d4f;
/// The container the lookup reads (`mov 0x2f0(%rdi)`, `sub 0x31c(%rdi)`, `movslq 0x330(%rdi)`).
const FIELD_COUNT: usize = 0x2f0;
const FIELD_FREE: usize = 0x31c;
const FIELD_HASH_SIZE: usize = 0x330;
/// The query struct the caller passes in `%r9`: `ContentId` sits at `+0x30`.
const QUERY_CONTENT_ID: usize = 0x30;

// Page layout: what the stub writes, then what the poller reads.
const SLOT_CALLS: usize = 0x00;
const SLOT_KEY: usize = 0x04;
const SLOT_ID2: usize = 0x08;
const SLOT_CONTAINER: usize = 0x10;
const SLOT_QUERY: usize = 0x18;
const CODE_OFF: usize = 0x40;

/// `disp32` for a RIP-relative instruction whose displacement field starts at `at`.
fn disp32(at: usize, target: usize) -> [u8; 4] {
    ((target as isize - (at + 4) as isize) as i32).to_le_bytes()
}

/// Store the arguments the resolver is called with, count the call, then tail-jump to it.
fn stub(page: usize, resolver: usize) -> Vec<u8> {
    let code_at = page + CODE_OFF;
    let mut c: Vec<u8> = Vec::new();
    // (opcode bytes, slot) — each displacement is filled in once its position is known
    let mut store = |c: &mut Vec<u8>, op: &[u8], slot: usize| {
        c.extend_from_slice(op);
        let at = code_at + c.len();
        c.extend_from_slice(&disp32(at, page + slot));
    };
    store(&mut c, &[0x48, 0x89, 0x0D], SLOT_CONTAINER); // mov %rcx,[rip+slot]
    store(&mut c, &[0x89, 0x15], SLOT_KEY); // mov %edx,[rip+slot]
    store(&mut c, &[0x44, 0x89, 0x05], SLOT_ID2); // mov %r8d,[rip+slot]
    store(&mut c, &[0x4C, 0x89, 0x0D], SLOT_QUERY); // mov %r9,[rip+slot]
    store(&mut c, &[0xFF, 0x05], SLOT_CALLS); // incl [rip+slot]
    c.push(0xE9); // jmp rel32 resolver
    let at = code_at + c.len();
    c.extend_from_slice(&disp32(at, resolver));
    c
}

unsafe fn readable(p: usize, size: usize) -> bool {
    use windows_sys::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS};
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

unsafe fn rd_u32(p: usize) -> Option<u32> {
    unsafe { readable(p, 4) }.then(|| unsafe { std::ptr::read_volatile(p as *const u32) })
}

/// A page within ±2 GiB of `base`, so the `call rel32` to the stub reaches it.
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

/// The game's module is loaded: hook the story race's content resolver if `WAL_ALLS_EXPMODE` is set.
pub(crate) fn install() {
    if !std::env::var("WAL_ALLS_EXPMODE").is_ok_and(|v| v.trim() == "1") {
        return;
    }
    let base = iat::game_module();
    if base == 0 {
        log!("expmode: no game module yet");
        return;
    }
    unsafe {
        let at = base + HOOK_RVA;
        if !readable(at, 5) || std::ptr::read_volatile(at as *const u8) != 0xE8 {
            log!("expmode: exe+{HOOK_RVA:#x} is not the expected call, no lookup logging");
            return;
        }
        let rel = std::ptr::read_unaligned((at + 1) as *const i32);
        let resolver = ((at + 5) as isize).wrapping_add(rel as isize) as usize;
        log!("expmode: story-race content resolver at exe+{:#x}", resolver - base);
        let Some(page) = (unsafe { alloc_near(base) }) else {
            log!("expmode: no stub page within 2 GiB of the game, no lookup logging");
            return;
        };
        let code = stub(page, resolver);
        std::ptr::copy_nonoverlapping(code.as_ptr(), (page + CODE_OFF) as *mut u8, code.len());
        let dist = (page + CODE_OFF) as isize - (at + 5) as isize;
        if dist < i32::MIN as isize || dist > i32::MAX as isize {
            log!("expmode: stub {page:#x} out of rel32 range, no lookup logging");
            return;
        }
        let mut old = 0;
        if VirtualProtect(at as *const _, 5, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            log!("expmode: VirtualProtect failed, no lookup logging");
            return;
        }
        // `call` (not `jmp`): the resolver must still see an aligned callee stack and return
        // to the instruction after the call site.
        std::ptr::write_volatile(at as *mut u8, 0xE8);
        std::ptr::write_unaligned((at + 1) as *mut i32, dist as i32);
        VirtualProtect(at as *const _, 5, old, &mut old);
        log!("expmode: logging the story-race content lookups (stub {page:#x}, {} bytes)", code.len());

        std::thread::spawn(move || {
            let slot32 = move |off: usize| (page + off) as *const u32;
            let slot64 = move |off: usize| (page + off) as *const usize;
            let mut prev = 0u32;
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let calls = unsafe { std::ptr::read_volatile(slot32(SLOT_CALLS)) };
                if calls == prev || calls == 0 {
                    continue;
                }
                prev = calls;
                let key = unsafe { std::ptr::read_volatile(slot32(SLOT_KEY)) };
                let id2 = unsafe { std::ptr::read_volatile(slot32(SLOT_ID2)) };
                let container = unsafe { std::ptr::read_volatile(slot64(SLOT_CONTAINER)) };
                let query = unsafe { std::ptr::read_volatile(slot64(SLOT_QUERY)) };
                let count = unsafe { rd_u32(container + FIELD_COUNT) };
                let free = unsafe { rd_u32(container + FIELD_FREE) };
                let hash = unsafe { rd_u32(container + FIELD_HASH_SIZE) };
                let content = unsafe { rd_u32(query + QUERY_CONTENT_ID) };
                let f = |v: Option<u32>| v.map_or("?".to_string(), |x| x.to_string());
                let live = match (count, free) {
                    (Some(c), Some(fr)) => c.wrapping_sub(fr).to_string(),
                    _ => "?".to_string(),
                };
                log!(
                    "expmode: lookup #{calls} key={key} id2={id2} contentId={} count={} free={} live={live} hash_size={} container={container:#x} query={query:#x}",
                    f(content),
                    f(count),
                    f(free),
                    f(hash)
                );
            }
        });
    }
}
