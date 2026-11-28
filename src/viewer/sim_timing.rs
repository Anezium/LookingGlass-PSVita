//! Monotonic W0 timing records. Only completed measurements leave a lane.
use crate::sim_pipeline::Stamp;
use std::{marker::PhantomData, rc::Rc, thread::ThreadId, time::Instant};

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Cut {
    A,
    B,
    Lag,
}

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Scope {
    Head,
    AudioApply,
    Submit,
    Join,
    Worker,
    StepA,
    StepB,
    StepLag,
    Render,
    InputToPresent,
    Present,
    PresentWithoutYield,
}

#[derive(Clone, Copy)]
pub(super) struct Clock(Instant);

pub(super) struct Mark {
    start: Instant,
    _thread: PhantomData<Rc<()>>,
    #[cfg(target_os = "vita")]
    display_start: Option<u32>,
}

pub(super) fn mark(enabled: bool) -> Option<Mark> {
    enabled.then(|| Mark {
        start: Instant::now(),
        _thread: PhantomData,
        #[cfg(target_os = "vita")]
        display_start: None,
    })
}

pub(super) struct Timing {
    stamp: Stamp,
    thread: ThreadId,
    scope: Scope,
    cut: Option<Cut>,
    start_ns: u64,
    end_ns: u64,
}

impl Clock {
    pub(super) fn finish(
        self,
        stamp: Stamp,
        scope: Scope,
        cut: Option<Cut>,
        mark: Option<Mark>,
    ) -> Option<Timing> {
        mark.map(|mark| {
            let end = Instant::now();
            self.interval(stamp, scope, cut, mark.start, end)
        })
    }

    pub(super) fn interval(
        self,
        stamp: Stamp,
        scope: Scope,
        cut: Option<Cut>,
        start: Instant,
        end: Instant,
    ) -> Timing {
        let ns = |at: Instant| at.duration_since(self.0).as_nanos() as u64;
        Timing {
            stamp,
            scope,
            cut,
            start_ns: ns(start),
            end_ns: ns(end),
            thread: std::thread::current().id(),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct DetailSpec {
    pub phase: bool,
    pub spike: bool,
    pub profile: bool,
}

pub(super) struct Details {
    pub stamp: Stamp,
    pub cut: Cut,
    pub phase: Option<crate::phase_census::WorkerRow>,
    pub spike: Option<crate::spike_log::WorkerRow>,
    pub profile: Option<crate::frame_profile::WorkerRow>,
}

pub(super) struct WorkerRows {
    pub work: Option<Timing>,
    pub step: Option<Timing>,
    pub details: Option<Box<Details>>,
}

#[cfg(target_os = "vita")]
#[derive(Clone, Copy)]
pub(super) struct DisplaySample {
    stamp: Stamp,
    input_us: u32,
    ticket: u32,
}

pub(super) struct Trace {
    clock: Option<Clock>,
    first: u64,
    last: u64,
    rows: Vec<Timing>,
    details: Vec<Box<Details>>,
    dropped: u64,
    #[cfg(target_os = "vita")]
    display_pending: Vec<DisplaySample>,
    #[cfg(target_os = "vita")]
    display_rows: Vec<(DisplaySample, crate::display_timing::Displayed)>,
}

impl Trace {
    pub(super) fn new() -> Self {
        #[cfg(feature = "vita_renderer")]
        let (mut enabled, first, last) = {
            let config = crate::render::vita_renderer::config();
            (
                config.diag_sim_timing,
                u64::from(config.spike_log_first_frame),
                u64::from(config.spike_log_last_frame),
            )
        };
        #[cfg(not(feature = "vita_renderer"))]
        let (mut enabled, first, last) = (false, 0, 0);
        #[cfg(all(feature = "sim_maps_probe", not(target_os = "vita")))]
        {
            enabled |= std::env::var_os("LOOKING_GLASS_SIM_TIMING").is_some();
        }
        Self {
            clock: enabled.then(|| Clock(Instant::now())),
            first,
            last,
            rows: Vec::new(),
            details: Vec::new(),
            dropped: 0,
            #[cfg(target_os = "vita")]
            display_pending: Vec::with_capacity(4),
            #[cfg(target_os = "vita")]
            display_rows: Vec::new(),
        }
    }

    pub(super) fn clock(&self, frame: u64) -> Option<Clock> {
        self.clock
            .filter(|_| frame >= self.first && (self.last == 0 || frame <= self.last))
    }

    pub(super) fn mark(&self, frame: u64) -> Option<Mark> {
        mark(self.clock(frame).is_some())
    }

    pub(super) fn input(&self, frame: u64) -> Option<Mark> {
        #[cfg(target_os = "vita")]
        return self.mark(frame).map(|mut mark| {
            mark.display_start = crate::display_timing::input();
            mark
        });
        #[cfg(not(target_os = "vita"))]
        self.mark(frame)
    }

    #[cfg(target_os = "vita")]
    pub(super) fn display_before_swap(&mut self, stamp: Stamp, mark: Option<&Mark>) -> Option<DisplaySample> {
        let input_us = mark?.display_start?;
        self.collect_display();
        let Some(ticket) = crate::display_timing::arm() else {
            self.dropped += 1;
            return None;
        };
        Some(DisplaySample { stamp, input_us, ticket })
    }

    #[cfg(target_os = "vita")]
    pub(super) fn display_after_swap(&mut self, sample: Option<DisplaySample>) {
        if let Some(sample) = sample {
            if crate::display_timing::submitted(sample.ticket) {
                self.display_pending.push(sample);
                self.collect_display();
            } else {
                crate::display_timing::cancel(sample.ticket);
                self.dropped += 1;
            }
        }
    }

    #[cfg(target_os = "vita")]
    fn collect_display(&mut self) {
        self.display_pending.retain(|sample| {
            let Some(time) = crate::display_timing::take(sample.ticket) else { return true; };
            if self.display_rows.len() < 100_000 {
                self.display_rows.push((*sample, time));
            } else {
                self.dropped += 1;
            }
            false
        });
    }

    pub(super) fn owner(
        &mut self,
        stamp: Stamp,
        scope: Scope,
        cut: Option<Cut>,
        mark: Option<Mark>,
    ) {
        let row = self
            .clock(stamp.frame.frame)
            .and_then(|clock| clock.finish(stamp, scope, cut, mark));
        self.push(row);
    }

    pub(super) fn push(&mut self, row: Option<Timing>) {
        if let Some(row) = row {
            if self.rows.len() < 100_000 {
                self.rows.push(row);
            } else {
                self.dropped += 1;
            }
        }
    }

    pub(super) fn worker(&mut self, expected: Stamp, rows: WorkerRows) {
        for row in [&rows.work, &rows.step].into_iter().flatten() {
            assert_eq!(row.stamp, expected, "stale sim timing stamp");
        }
        self.push(rows.work);
        self.push(rows.step);
        if let Some(details) = rows.details {
            assert_eq!(details.stamp, expected, "stale sim detail stamp");
            if self.details.len() < 1024 {
                self.details.push(details);
            } else {
                self.dropped += 1;
            }
        }
    }

    pub(super) fn flush(self) {
        #[cfg(target_os = "vita")]
        let mut this = self;
        #[cfg(not(target_os = "vita"))]
        let this = self;
        #[cfg(target_os = "vita")]
        {
            if !this.display_pending.is_empty() && !crate::display_timing::finish() {
                this.dropped += 1;
            }
            this.collect_display();
            this.dropped += this.display_pending.len() as u64;
            for (sample, shown) in this.display_rows {
                let stamp = sample.stamp;
                let time = shown.scheduled_us;
                println!("SIM_W0_DISPLAY {}", serde_json::json!({
                    "frame": stamp.frame.frame, "tape_step": stamp.frame.tape_step,
                    "visit": stamp.frame.visit_id, "generation": stamp.slot_generation,
                    "submission": sample.ticket, "scheduled_us": time,
                    "input_to_display_us": time.wrapping_sub(sample.input_us),
                    "add_entry_us": shown.add_entry_us, "gate_wait_us": shown.gate_wait_us,
                    "gate_pending": shown.gate_pending, "gate_timeout": shown.gate_timeout,
                }));
            }
            if let Some(gate) = crate::display_timing::gate_stats() {
                println!("SIM_W0_CAP_QUEUE {}", serde_json::json!({
                    "depth": 1, "entries": gate.entries, "callbacks": gate.callbacks, "waits": gate.waits,
                    "timeouts": gate.timeouts, "max_wait_us": gate.max_wait_us, "off": gate.off,
                }));
            }
        }
        let owner = std::thread::current().id();
        for row in this.rows {
            let stamp = row.stamp;
            println!(
                "SIM_W0_TIMING {}",
                serde_json::json!({
                    "frame": stamp.frame.frame, "tape_step": stamp.frame.tape_step,
                    "visit": stamp.frame.visit_id, "generation": stamp.slot_generation,
                    "lane": if row.thread == owner {"owner"} else {"worker"},
                    "thread": format!("{:?}", row.thread),
                    "scope": row.scope, "cut": row.cut, "start_ns": row.start_ns, "end_ns": row.end_ns,
                })
            );
        }
        for detail in this.details {
            let stamp = detail.stamp;
            println!(
                "SIM_W0_DETAILS {}",
                serde_json::json!({
                    "frame": stamp.frame.frame, "tape_step": stamp.frame.tape_step,
                    "visit": stamp.frame.visit_id, "generation": stamp.slot_generation,
                    "lane": "worker", "cut": detail.cut, "census": detail.phase,
                    "spike": detail.spike, "profile": detail.profile,
                })
            );
        }
        if this.dropped != 0 {
            println!("SIM_W0_TIMING_DROPPED {}", this.dropped);
        }
    }
}
