//! Match measured owner swaps to vitaGL's FIFO display queue.
//!
//! `cap_queue_depth: 1` under the 30 fps cap also gates the queue: a swap waits in the AddEntry
//! wrapper until every entry already queued has returned from its display callback, so at most
//! one finished frame waits for the display. vitaGL's callback ends with the capped vblank wait
//! (`gxm.c:266-270`), whose wrapper calls `callback_returned`. A wait is bounded by
//! `GATE_TIMEOUT_US`; three timeouts in a row turn the gate off for the rest of the run.
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering::{Acquire, Relaxed, Release}};

const SLOT_COUNT: usize = 16;
const NEXT_FRAME: i32 = 1;

const GATE_TIMEOUT_US: u32 = 100_000;
const GATE_TIMEOUTS_TO_DISABLE: u32 = 3;
const GATE_STATE_ON: u32 = 0x100;
const GATE_STATE_TIMEOUT: u32 = 0x200;
const SCE_KERNEL_ERROR_WAIT_TIMEOUT: i32 = 0x8002_8005_u32 as i32;

struct Slot {
    ticket: AtomicU32,
    scheduled: AtomicU32,
    // Written and read by the swapping thread only.
    gate_wait_us: AtomicU32,
    add_entry_us: AtomicU32,
    gate_state: AtomicU32,
}

/// What the display side recorded for one measured swap.
#[derive(Clone, Copy)]
pub(crate) struct Displayed {
    pub scheduled_us: u32,
    pub add_entry_us: u32,
    pub gate_wait_us: u32,
    /// Entries still queued or in their callback when the swap reached AddEntry; None with the gate off.
    pub gate_pending: Option<u32>,
    pub gate_timeout: bool,
}

/// Whole-run gate counters, menus and loading included.
pub(crate) struct GateStats {
    pub entries: u32,
    pub callbacks: u32,
    pub waits: u32,
    pub timeouts: u32,
    pub max_wait_us: u32,
    pub off: bool,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static FAILED: AtomicBool = AtomicBool::new(false);
static SUBMITTED: AtomicU32 = AtomicU32::new(0);
static DISPLAYED: AtomicU32 = AtomicU32::new(0);
static STAMPS: [Slot; SLOT_COUNT] = [const {
    Slot {
        ticket: AtomicU32::new(0), scheduled: AtomicU32::new(0), gate_wait_us: AtomicU32::new(0),
        add_entry_us: AtomicU32::new(0), gate_state: AtomicU32::new(0),
    }
}; SLOT_COUNT];

static GATE: AtomicBool = AtomicBool::new(false);
static GATE_OFF: AtomicBool = AtomicBool::new(false);
static GATE_SEMA: AtomicI32 = AtomicI32::new(-1);
static DISPLAY_THREAD: AtomicI32 = AtomicI32::new(0);
/// Entries the real AddEntry accepted while the gate was on, and callbacks that returned.
static QUEUED: AtomicU32 = AtomicU32::new(0);
static CALLBACKS: AtomicU32 = AtomicU32::new(0);
static WAITS: AtomicU32 = AtomicU32::new(0);
static TIMEOUTS: AtomicU32 = AtomicU32::new(0);
static TIMEOUT_RUN: AtomicU32 = AtomicU32::new(0);
static MAX_WAIT_US: AtomicU32 = AtomicU32::new(0);

type Void = core::ffi::c_void;
extern "C" {
    fn sceKernelGetProcessTimeLow() -> u32;
    fn sceGxmDisplayQueueFinish() -> i32;
    fn __real_sceGxmDisplayQueueAddEntry(old: *mut Void, new: *mut Void, data: *const Void) -> i32;
    fn __real_sceDisplaySetFrameBuf(buf: *const Void, sync: i32) -> i32;
    fn sceKernelGetThreadId() -> i32;
    fn sceKernelCreateSema(name: *const u8, attr: u32, init: i32, max: i32, option: *const Void) -> i32;
    fn sceKernelWaitSema(id: i32, signal: i32, timeout: *mut u32) -> i32;
    fn sceKernelSignalSema(id: i32, signal: i32) -> i32;
}

pub(crate) fn enable() {
    ENABLED.store(true, Release);
}

/// Before vglInit, so the counts start with the first display entry. Off when the semaphore fails.
pub(crate) fn enable_gate() -> i32 {
    let id = unsafe { sceKernelCreateSema(b"lg-cap-queue\0".as_ptr(), 0, 0, 1, core::ptr::null()) };
    if id >= 0 {
        GATE_SEMA.store(id, Relaxed);
        GATE.store(true, Release);
    }
    id
}

pub(crate) fn gate_stats() -> Option<GateStats> {
    (GATE.load(Acquire) || GATE_OFF.load(Relaxed)).then(|| GateStats {
        entries: QUEUED.load(Relaxed),
        callbacks: CALLBACKS.load(Relaxed),
        waits: WAITS.load(Relaxed),
        timeouts: TIMEOUTS.load(Relaxed),
        max_wait_us: MAX_WAIT_US.load(Relaxed),
        off: GATE_OFF.load(Relaxed),
    })
}

/// From the capped vblank wait wrapper, after the real wait: the display callback is returning.
#[inline]
pub(crate) fn callback_returned() {
    if !GATE.load(Relaxed) || unsafe { sceKernelGetThreadId() } != DISPLAY_THREAD.load(Relaxed) {
        return;
    }
    CALLBACKS.fetch_add(1, Release);
    // Max count 1: a signal nobody waits for stays as one spare wakeup, which the loop re-checks.
    unsafe { sceKernelSignalSema(GATE_SEMA.load(Relaxed), 1) };
}

fn pending() -> u32 {
    QUEUED.load(Relaxed).wrapping_sub(CALLBACKS.load(Acquire))
}

/// Wait until no queued entry is left before this one. Returns (waited us, entry pending, timed out).
unsafe fn gate_wait() -> (u32, u32, bool) {
    let before = pending();
    if before == 0 || before > 2 {
        // More than vitaGL's two pending entries means the counts are out of step: do not wait.
        return (0, before, false);
    }
    let start = now();
    let sema = GATE_SEMA.load(Relaxed);
    let timed_out = loop {
        let elapsed = now().wrapping_sub(start);
        if elapsed >= GATE_TIMEOUT_US {
            break true;
        }
        let mut left = GATE_TIMEOUT_US - elapsed;
        let result = sceKernelWaitSema(sema, 1, &mut left);
        if pending() == 0 {
            break false;
        }
        if result < 0 && result != SCE_KERNEL_ERROR_WAIT_TIMEOUT {
            break true;
        }
    };
    let waited = now().wrapping_sub(start);
    WAITS.fetch_add(1, Relaxed);
    MAX_WAIT_US.fetch_max(waited, Relaxed);
    if timed_out {
        TIMEOUTS.fetch_add(1, Relaxed);
        if TIMEOUT_RUN.fetch_add(1, Relaxed) + 1 >= GATE_TIMEOUTS_TO_DISABLE {
            GATE.store(false, Release);
            GATE_OFF.store(true, Relaxed);
            println!("[lg/display] cap_queue_gate off after {GATE_TIMEOUTS_TO_DISABLE} timeouts in a row, pending={}", pending());
        }
    } else {
        TIMEOUT_RUN.store(0, Relaxed);
    }
    (waited, before, timed_out)
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
    slot.gate_wait_us.store(0, Relaxed);
    slot.add_entry_us.store(0, Relaxed);
    slot.gate_state.store(0, Relaxed);
    slot.ticket.store(ticket, Release);
    Some(ticket)
}

pub(crate) fn submitted(ticket: u32) -> bool {
    SUBMITTED.load(Relaxed) == ticket
}

pub(crate) fn take(ticket: u32) -> Option<Displayed> {
    let slot = slot(ticket);
    let time = slot.scheduled.load(Acquire);
    if time == 0 {
        return None;
    }
    let state = slot.gate_state.load(Relaxed);
    let displayed = Displayed {
        scheduled_us: time,
        add_entry_us: slot.add_entry_us.load(Relaxed),
        gate_wait_us: slot.gate_wait_us.load(Relaxed),
        gate_pending: (state & GATE_STATE_ON != 0).then_some(state & 0xff),
        gate_timeout: state & GATE_STATE_TIMEOUT != 0,
    };
    cancel(ticket);
    Some(displayed)
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
    let gated = GATE.load(Acquire);
    if !enabled && !gated {
        return __real_sceGxmDisplayQueueAddEntry(old, new, data);
    }
    let (wait_us, pending, timed_out) = if gated { gate_wait() } else { (0, 0, false) };
    // Include swaps outside the viewer's measurement window, including loading.
    let ticket = if enabled { SUBMITTED.fetch_add(1, Relaxed).wrapping_add(1) } else { 0 };
    let enter = if enabled { now() } else { 0 };
    let result = __real_sceGxmDisplayQueueAddEntry(old, new, data);
    if gated && result >= 0 {
        QUEUED.fetch_add(1, Relaxed);
    }
    if enabled {
        let add_us = now().wrapping_sub(enter);
        let slot = slot(ticket);
        if slot.ticket.load(Acquire) == ticket {
            slot.gate_wait_us.store(wait_us, Relaxed);
            slot.add_entry_us.store(add_us, Relaxed);
            let state = if gated {
                GATE_STATE_ON | pending.min(0xff) | if timed_out { GATE_STATE_TIMEOUT } else { 0 }
            } else { 0 };
            slot.gate_state.store(state, Relaxed);
        }
        if result < 0 {
            FAILED.store(true, Relaxed);
        }
    }
    result
}

#[no_mangle]
pub unsafe extern "C" fn __wrap_sceDisplaySetFrameBuf(buf: *const Void, sync: i32) -> i32 {
    let result = __real_sceDisplaySetFrameBuf(buf, sync);
    if sync == NEXT_FRAME && GATE.load(Relaxed) {
        DISPLAY_THREAD.store(sceKernelGetThreadId(), Relaxed);
    }
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
