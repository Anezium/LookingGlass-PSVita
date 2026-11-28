//! Match measured owner swaps to vitaGL's FIFO display queue.
//!
//! `cap_queue_depth: 1` under the 30 fps cap also gates the queue: before the real AddEntry, a
//! swap waits until every earlier entry has returned from its display callback, so at most one
//! finished frame waits for the display. vitaGL's callback ends with the capped vblank wait
//! (`gxm.c:266-270`), whose wrapper calls `callback_returned`. A wait is bounded by
//! `GATE_TIMEOUT_US`. The gate fails open, off for the rest of the run, on a vsync interval
//! other than 1 (no wait, so no signal), any kernel or GXM error, counts out of step, or three
//! timeouts in a row.
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering::{Acquire, Relaxed, Release}};

const SLOT_COUNT: usize = 16;
const NEXT_FRAME: i32 = 1;

const GATE_TIMEOUT_US: u32 = 100_000;
const GATE_TIMEOUTS_TO_DISABLE: u32 = 3;
const GATE_STATE_ON: u32 = 0x100;
const GATE_STATE_TIMEOUT: u32 = 0x200;
const SCE_KERNEL_ERROR_WAIT_TIMEOUT: i32 = 0x8002_8005_u32 as i32;
const SCE_KERNEL_ERROR_SEMA_OVF: i32 = 0x8002_8103_u32 as i32;

/// Why the gate turned itself off; 0 while it is on or was never on.
#[derive(Clone, Copy)]
#[repr(u32)]
enum Off {
    Vsync = 1,
    Sema,
    AddEntry,
    Wait,
    Signal,
    Counts,
    Timeouts,
}

fn off_name(code: u32) -> Option<&'static str> {
    Some(match code {
        1 => "vsync_interval", 2 => "sema", 3 => "add_entry", 4 => "wait_error", 5 => "signal_error",
        6 => "counts", 7 => "timeouts", _ => return None,
    })
}

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
    /// Earlier entries still queued or in their callback when the swap reached the gate; None with the gate off.
    pub gate_pending: Option<u32>,
    pub gate_timeout: bool,
}

/// Whole-run gate counters, menus and loading included.
pub(crate) struct GateStats {
    pub submitted: u32,
    pub completed: u32,
    pub waits: u32,
    pub timeouts: u32,
    pub max_wait_us: u32,
    pub off: Option<&'static str>,
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
static GATE_OFF: AtomicU32 = AtomicU32::new(0);
static GATE_OFF_LOGGED: AtomicBool = AtomicBool::new(false);
static GATE_SEMA: AtomicI32 = AtomicI32::new(-1);
static DISPLAY_THREAD: AtomicI32 = AtomicI32::new(0);
/// Entries offered to the real AddEntry while the gate is on, the current one included, and
/// display callbacks that returned. An entry passes once `COMPLETED >= GATE_SUBMITTED - 1`.
static GATE_SUBMITTED: AtomicU32 = AtomicU32::new(0);
static COMPLETED: AtomicU32 = AtomicU32::new(0);
static WAITS: AtomicU32 = AtomicU32::new(0);
static TIMEOUTS: AtomicU32 = AtomicU32::new(0);
static TIMEOUT_RUN: AtomicU32 = AtomicU32::new(0);
static MAX_WAIT_US: AtomicU32 = AtomicU32::new(0);

type Void = core::ffi::c_void;
extern "C" {
    /// vitaGL `vgl.c:112`: the display callback waits for vblanks only while this is non-zero (`gxm.c:269`).
    static vsync_interval: u32;
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

/// Before vglInit, so the counts start with the first display entry. Fails open without a semaphore.
pub(crate) fn enable_gate() -> i32 {
    let id = unsafe { sceKernelCreateSema(b"lg-cap-queue\0".as_ptr(), 0, 0, 1, core::ptr::null()) };
    if id >= 0 {
        GATE_SEMA.store(id, Relaxed);
        GATE.store(true, Release);
    } else {
        disable(Off::Sema);
    }
    id
}

/// Fail open: the queue keeps vitaGL's two pending entries from here on. Any thread.
fn disable(reason: Off) {
    GATE.store(false, Release);
    let _ = GATE_OFF.compare_exchange(0, reason as u32, Relaxed, Relaxed);
}

/// The first time the swapping thread sees the gate off, it logs why (never the display thread).
fn log_off() {
    if let Some(reason) = off_name(GATE_OFF.load(Relaxed)) {
        if !GATE_OFF_LOGGED.swap(true, Relaxed) {
            println!("[lg/display] cap_queue_gate off reason={reason} submitted={} completed={}",
                GATE_SUBMITTED.load(Relaxed), COMPLETED.load(Relaxed));
        }
    }
}

pub(crate) fn gate_stats() -> Option<GateStats> {
    let off = off_name(GATE_OFF.load(Relaxed));
    (GATE.load(Acquire) || off.is_some()).then(|| GateStats {
        submitted: GATE_SUBMITTED.load(Relaxed),
        completed: COMPLETED.load(Relaxed),
        waits: WAITS.load(Relaxed),
        timeouts: TIMEOUTS.load(Relaxed),
        max_wait_us: MAX_WAIT_US.load(Relaxed),
        off,
    })
}

/// From the capped vblank wait wrapper, after the real wait: the display callback is returning.
#[inline]
pub(crate) fn callback_returned() {
    if !GATE.load(Relaxed) || unsafe { sceKernelGetThreadId() } != DISPLAY_THREAD.load(Relaxed) {
        return;
    }
    // Count first, then signal: a waiter woken by this signal, or by its own timeout, sees the count.
    COMPLETED.fetch_add(1, Release);
    // Max count 1: a signal nobody waits for stays as one spare wakeup, which the waiter re-checks.
    let result = unsafe { sceKernelSignalSema(GATE_SEMA.load(Relaxed), 1) };
    if result < 0 && result != SCE_KERNEL_ERROR_SEMA_OVF {
        disable(Off::Signal);
    }
}

/// Earlier entries not yet back from their callback, for the entry numbered `submitted`.
fn pending(submitted: u32) -> u32 {
    submitted.wrapping_sub(1).wrapping_sub(COMPLETED.load(Acquire))
}

/// Number this entry, then wait until `completed >= submitted - 1`. Returns (waited us, pending, timed out).
unsafe fn gate_wait() -> (u32, u32, bool) {
    let submitted = GATE_SUBMITTED.fetch_add(1, Relaxed).wrapping_add(1);
    if core::ptr::read_volatile(core::ptr::addr_of!(vsync_interval)) != 1 {
        disable(Off::Vsync);
        return (0, pending(submitted), false);
    }
    let before = pending(submitted);
    if before == 0 {
        return (0, 0, false);
    }
    if before > 2 {
        // vitaGL never has more than two entries pending, so the counts are out of step.
        disable(Off::Counts);
        return (0, before, false);
    }
    let start = now();
    let sema = GATE_SEMA.load(Relaxed);
    let timed_out = loop {
        // Re-check before every timeout decision and after every wakeup, real or spare.
        if pending(submitted) == 0 {
            break false;
        }
        let elapsed = now().wrapping_sub(start);
        if elapsed >= GATE_TIMEOUT_US {
            break true;
        }
        let mut left = GATE_TIMEOUT_US - elapsed;
        let result = sceKernelWaitSema(sema, 1, &mut left);
        if result < 0 && result != SCE_KERNEL_ERROR_WAIT_TIMEOUT {
            disable(Off::Wait);
            break pending(submitted) != 0;
        }
    };
    let waited = now().wrapping_sub(start);
    WAITS.fetch_add(1, Relaxed);
    MAX_WAIT_US.fetch_max(waited, Relaxed);
    if timed_out {
        TIMEOUTS.fetch_add(1, Relaxed);
        if TIMEOUT_RUN.fetch_add(1, Relaxed) + 1 >= GATE_TIMEOUTS_TO_DISABLE {
            disable(Off::Timeouts);
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
    // The gate keys on CAP_30 and cap_queue_depth alone (enable_gate), not on the timing trace.
    let gated = GATE.load(Acquire);
    if !enabled && !gated {
        return __real_sceGxmDisplayQueueAddEntry(old, new, data);
    }
    // Gate before the real AddEntry, which then finds at most one entry pending.
    let (wait_us, pending, timed_out) = if gated { gate_wait() } else { (0, 0, false) };
    // Include swaps outside the viewer's measurement window, including loading.
    let ticket = if enabled { SUBMITTED.fetch_add(1, Relaxed).wrapping_add(1) } else { 0 };
    let enter = if enabled { now() } else { 0 };
    let result = __real_sceGxmDisplayQueueAddEntry(old, new, data);
    if gated && result < 0 {
        disable(Off::AddEntry);
    }
    log_off();
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
