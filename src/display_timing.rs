//! Match measured owner swaps to vitaGL's FIFO display queue.
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::{Acquire, Relaxed, Release}};

const SLOT_COUNT: usize = 16;
const NEXT_FRAME: i32 = 1;

struct Slot {
    ticket: AtomicU32,
    scheduled: AtomicU32,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static FAILED: AtomicBool = AtomicBool::new(false);
static SUBMITTED: AtomicU32 = AtomicU32::new(0);
static DISPLAYED: AtomicU32 = AtomicU32::new(0);
static STAMPS: [Slot; SLOT_COUNT] = [const {
    Slot { ticket: AtomicU32::new(0), scheduled: AtomicU32::new(0) }
}; SLOT_COUNT];

type Void = core::ffi::c_void;
extern "C" {
    fn sceKernelGetProcessTimeLow() -> u32;
    fn sceGxmDisplayQueueFinish() -> i32;
    fn __real_sceGxmDisplayQueueAddEntry(old: *mut Void, new: *mut Void, data: *const Void) -> i32;
    fn __real_sceDisplaySetFrameBuf(buf: *const Void, sync: i32) -> i32;
}

pub(crate) fn enable() {
    ENABLED.store(true, Release);
}

fn now() -> u32 {
    unsafe { sceKernelGetProcessTimeLow() }.max(1)
}

pub(crate) fn input() -> Option<u32> {
    ENABLED.load(Acquire).then(now)
}

fn slot(ticket: u32) -> &'static Slot {
    &STAMPS[ticket as usize % SLOT_COUNT]
}

pub(crate) fn arm() -> Option<u32> {
    let ticket = SUBMITTED.load(Relaxed).wrapping_add(1);
    let slot = slot(ticket);
    if ticket == 0 || slot.ticket.load(Acquire) != 0 || FAILED.load(Relaxed) {
        return None;
    }
    slot.scheduled.store(0, Relaxed);
    slot.ticket.store(ticket, Release);
    Some(ticket)
}

pub(crate) fn submitted(ticket: u32) -> bool {
    SUBMITTED.load(Relaxed) == ticket
}

pub(crate) fn take(ticket: u32) -> Option<u32> {
    let time = slot(ticket).scheduled.load(Acquire);
    if time == 0 {
        return None;
    }
    cancel(ticket);
    Some(time)
}

pub(crate) fn cancel(ticket: u32) {
    slot(ticket).ticket.store(0, Release);
}

pub(crate) fn finish() -> bool {
    (unsafe { sceGxmDisplayQueueFinish() } >= 0) && !FAILED.load(Relaxed)
}

#[no_mangle]
pub unsafe extern "C" fn __wrap_sceGxmDisplayQueueAddEntry(
    old: *mut Void, new: *mut Void, data: *const Void,
) -> i32 {
    let enabled = ENABLED.load(Acquire);
    if enabled {
        // Include swaps outside the viewer's measurement window, including loading.
        SUBMITTED.fetch_add(1, Relaxed);
    }
    let result = __real_sceGxmDisplayQueueAddEntry(old, new, data);
    if enabled && result < 0 {
        FAILED.store(true, Relaxed);
    }
    result
}

#[no_mangle]
pub unsafe extern "C" fn __wrap_sceDisplaySetFrameBuf(buf: *const Void, sync: i32) -> i32 {
    let result = __real_sceDisplaySetFrameBuf(buf, sync);
    // vitaGL calls NEXTFRAME once per display queue entry, before waiting.
    if sync == NEXT_FRAME && ENABLED.load(Acquire) && !FAILED.load(Relaxed) {
        let ticket = DISPLAYED.fetch_add(1, Relaxed).wrapping_add(1);
        let slot = slot(ticket);
        if result < 0 {
            FAILED.store(true, Relaxed);
        } else if slot.ticket.load(Acquire) == ticket {
            slot.scheduled.store(now(), Release);
        }
    }
    result
}
