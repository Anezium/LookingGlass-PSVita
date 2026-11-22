//! Live input epochs for lag one in ordinary play.
//!
//! Macroquad edges change only at `next_frame`. A retained head loops back without one, so the
//! first lag head and the next capture the same epoch and a press would reach two steps. A drain
//! presents the retained frame inside the head, so edges of that flush epoch would reach no
//! head before the tail's own frame. `Epochs` gives every physical epoch's edges to exactly one
//! step: the shared head gets a derived sample (held input, sticks and clock latched, edges
//! cleared) and the flush epoch's edges are carried into the next head's sample.
//! Replays install one tape sample per step and never use this.
use crate::input::StepInput;
use crate::replay::input::Frame;

#[derive(Default)]
pub(super) struct Epochs {
    enabled: bool,
    /// A present ran after this head's capture and its epoch is owed to the next head: the tail
    /// carries its edges. An owner yield hands that epoch to the owner and clears this.
    carry_due: bool,
    /// The framework epoch moved on after this head's capture (a present, with or without a
    /// later yield). A retaining head then must not share its sample with the next head.
    moved: bool,
    /// The previous head's sample when the next head repeats its epoch.
    shared: Option<StepInput>,
    /// Edges of an epoch that no head captured.
    carry: Option<Frame>,
}

impl Epochs {
    pub(super) fn new(enabled: bool) -> Self {
        Self { enabled, ..Self::default() }
    }

    /// The head sample. The bool is true when the sample repeats the previous epoch, whose
    /// clock stays latched for this step.
    pub(super) fn head(&mut self, step: u64, capture: impl FnOnce() -> StepInput) -> (StepInput, bool) {
        self.carry_due = false;
        self.moved = false;
        if !self.enabled {
            return (capture(), false);
        }
        if let Some(previous) = self.shared.take() {
            // A shared epoch has no present between the heads, so nothing can be carried.
            assert!(self.carry.is_none(), "carried edges into a shared input epoch");
            return (previous.derived_sample(step), true);
        }
        let mut sample = capture();
        if let Some(edges) = self.carry.take() {
            merge_edges(&mut sample.frame, &edges);
        }
        (sample, false)
    }

    /// Called after every present of a retained frame.
    pub(super) fn presented(&mut self) {
        self.carry_due = true;
        self.moved = true;
    }

    /// The head yielded to owner work (loading, a menu, a movie) after its present. Those
    /// framework frames belong to the owner, as with the key off: the tail must not carry the
    /// epoch the owner already showed to the live controls into the next step. The epoch still
    /// moved: a head that retains after the yield must not share its stale sample.
    pub(super) fn yielded(&mut self) {
        self.carry_due = false;
    }

    /// The head retained its frame and loops back without a framework frame. Without a present
    /// in this head, the next head sees the epoch this head already consumed.
    pub(super) fn retained(&mut self, sample: &StepInput) {
        if self.enabled && !self.moved {
            self.shared = Some(sample.clone());
        }
    }

    /// Just before the serial tail's framework frame. A present in this head began an epoch that
    /// this head's consumers never read; keep its edges for the next head.
    pub(super) fn before_tail_frame(&mut self, edges: impl FnOnce() -> Frame) {
        if self.enabled && self.carry_due {
            self.carry = Some(edges());
        }
    }
}

fn merge_edges(frame: &mut Frame, edges: &Frame) {
    frame.keys_pressed |= edges.keys_pressed;
    frame.keys_released |= edges.keys_released;
    frame.mouse_pressed |= edges.mouse_pressed;
    frame.mouse_released |= edges.mouse_released;
    frame.wheel.0 += edges.wheel.0;
    frame.wheel.1 += edges.wheel.1;
    frame.quit |= edges.quit;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A framework whose edges change only at `next_frame`, with one tap per epoch.
    struct Framework {
        epoch: u64,
    }

    impl Framework {
        fn edges(&self) -> Frame {
            Frame { keys_pressed: 1 << self.epoch, keys_down: 1 << 100, dt: 0.02, ..Frame::default() }
        }
        fn capture(&self, step: u64) -> StepInput {
            StepInput::test_sample(step, self.edges(), self.epoch)
        }
        fn next_frame(&mut self) {
            self.epoch += 1;
        }
    }

    enum Head {
        /// Serial step, drawn by the tail.
        Serial,
        /// Lag dispatch: present the previous retained frame, if any, then retain this one.
        Lag,
        /// Drain: present the previous retained frame, then run serially.
        Drain,
        /// Drain whose head then yields to owner work for two framework frames (loading),
        /// as a natural exit, chapter or New Game does after its present.
        DrainYield,
        /// Present the previous retained frame, yield one framework frame to the owner, then
        /// retain this step (reachable on desktop when `save_preview` is not throttled).
        LagYield,
    }

    /// Runs heads in order and returns the tap bits each step received, plus latched clocks.
    fn run(enabled: bool, heads: &[Head]) -> (Vec<u128>, Vec<(u64, bool)>) {
        run_with(enabled, true, heads)
    }

    fn run_with(enabled: bool, yield_clears: bool, heads: &[Head]) -> (Vec<u128>, Vec<(u64, bool)>) {
        run_full(enabled, yield_clears, heads).0
    }

    /// Also returns each head's sample, so a test can see what a derived head repeated.
    fn run_full(enabled: bool, yield_clears: bool, heads: &[Head])
        -> ((Vec<u128>, Vec<(u64, bool)>), Vec<StepInput>) {
        let mut framework = Framework { epoch: 0 };
        let mut epochs = Epochs::new(enabled);
        let mut retained = false;
        let mut taps = Vec::new();
        let mut clocks = Vec::new();
        let mut samples = Vec::new();
        for (step, head) in heads.iter().enumerate() {
            let (sample, latched) = epochs.head(step as u64, || framework.capture(step as u64));
            assert_eq!(sample.step, step as u64);
            assert_eq!(sample.frame.keys_down, 1 << 100, "held input survives every sample");
            taps.push(sample.frame.keys_pressed);
            clocks.push((sample.clock_bits, latched));
            samples.push(sample.clone());
            let present = |framework: &mut Framework, epochs: &mut Epochs, retained: &mut bool| {
                if std::mem::take(retained) {
                    framework.next_frame();
                    epochs.presented();
                }
            };
            match head {
                Head::Lag => {
                    present(&mut framework, &mut epochs, &mut retained);
                    retained = true;
                    epochs.retained(&sample);
                }
                Head::Serial | Head::Drain => {
                    present(&mut framework, &mut epochs, &mut retained);
                    epochs.before_tail_frame(|| framework.edges());
                    framework.next_frame();
                }
                Head::DrainYield => {
                    present(&mut framework, &mut epochs, &mut retained);
                    framework.next_frame();
                    framework.next_frame();
                    if yield_clears {
                        epochs.yielded();
                    }
                    epochs.before_tail_frame(|| framework.edges());
                    framework.next_frame();
                }
                Head::LagYield => {
                    present(&mut framework, &mut epochs, &mut retained);
                    framework.next_frame();
                    if yield_clears {
                        epochs.yielded();
                    }
                    retained = true;
                    epochs.retained(&sample);
                }
            }
        }
        ((taps, clocks), samples)
    }

    fn delivered(taps: &[u128]) -> Vec<u32> {
        let mut epochs = Vec::new();
        for bits in taps {
            epochs.extend((0..100).filter(|bit| bits >> bit & 1 != 0));
        }
        epochs
    }

    use Head::{Drain, DrainYield, Lag, LagYield, Serial};

    #[test]
    fn a_head_that_presents_yields_and_retains_does_not_share_its_stale_sample() {
        // Review REVIEW-FIXES-S10 R5. Step 2 presents step 1's frame (epoch 1 -> 2), yields one
        // owner frame (2 -> 3), then retains. The epoch moved, so step 3 must capture epoch 3
        // fresh, not repeat step 2's pre-yield sample (stale held keys, sticks and clock).
        let heads = [Serial, Lag, LagYield, Drain, Serial];
        let ((taps, clocks), samples) = run_full(true, true, &heads);
        assert!(!clocks[3].1, "step 3 captured, not derived");
        assert_eq!(samples[3].clock_bits, 3, "step 3 sees the post-yield epoch");
        assert_ne!(samples[3].clock_bits, samples[2].clock_bits);
        // Epoch 2 began at the present in step 2's head and the owner's yield frame consumed it,
        // as after any owner yield (review S34 F3). No epoch reaches two steps.
        let got = delivered(&taps);
        let mut sorted = got.clone();
        sorted.dedup();
        assert_eq!(sorted, got, "no epoch reaches two steps: {got:?}");
        assert!(!got.contains(&2), "the yielded epoch belongs to the owner: {got:?}");
        assert!(got.contains(&3), "the post-yield epoch reaches the next step: {got:?}");
    }

    #[test]
    fn ship_profile_sequences_are_unchanged_by_the_split_flag() {
        // Every head kind reachable with the ship profile: steady lag, drains, serial play and a
        // present followed by an owner yield and the serial tail. Same deliveries and latches as
        // the single flag gave (pinned values from the tests above).
        let (lag, _) = run(true, &[Serial, Lag, DrainYield, Serial, Serial]);
        assert_eq!(delivered(&lag[3..]), vec![5, 6]);
        let (taps, clocks) = run(true, ENTRY_AND_DRAIN);
        let got = delivered(&taps);
        assert_eq!(got, (0..=*got.last().unwrap()).collect::<Vec<_>>());
        assert_eq!(clocks.iter().filter(|(_, latched)| *latched).count(), 3);
    }

    #[test]
    fn a_press_on_the_last_loading_frame_does_not_reach_the_new_map() {
        // Lag, then a drain whose head presents and yields two loading frames (epochs 2..=4 go
        // to the owner), then serial play on the new map from epoch 5.
        let heads = [Serial, Lag, DrainYield, Serial, Serial];
        let (lag, _) = run(true, &heads);
        let (off, _) = run(false, &[Serial, Serial, DrainYield, Serial, Serial]);
        let new_map = |taps: &[u128]| delivered(&taps[3..]);
        assert_eq!(new_map(&lag), vec![5, 6], "the new map's first step sees only its own epoch");
        assert_eq!(new_map(&lag), new_map(&off), "same as with the key off");
        // Without the yield clear, the last loading epoch (4) leaks into the new map's first step.
        let (leak, _) = run_with(true, false, &heads);
        assert_eq!(new_map(&leak), vec![4, 5, 6]);
    }
    const ENTRY_AND_DRAIN: &[Head] = &[Serial, Serial, Lag, Lag, Lag, Drain, Serial, Lag, Lag, Drain, Lag, Drain, Serial];

    #[test]
    fn every_epoch_reaches_exactly_one_step_across_entry_and_drain() {
        let (taps, clocks) = run(true, ENTRY_AND_DRAIN);
        let got = delivered(&taps);
        let last = got.last().copied().unwrap();
        assert_eq!(got, (0..=last).collect::<Vec<_>>(), "each tap once, in order");
        // The second entry head repeats the first one's clock; captured heads never latch.
        for (step, (bits, latched)) in clocks.iter().enumerate() {
            let after_entry = step > 0 && matches!(ENTRY_AND_DRAIN[step - 1], Lag)
                && (step < 2 || !matches!(ENTRY_AND_DRAIN[step - 2], Lag));
            assert_eq!(*latched, after_entry, "step {step}");
            if *latched {
                assert_eq!(*bits, clocks[step - 1].0, "latched clock at step {step}");
                assert_eq!(taps[step], 0, "a shared epoch delivers no edge at step {step}");
            }
        }
    }

    #[test]
    fn without_epochs_entry_repeats_a_press_and_drain_loses_one() {
        let (taps, _) = run(false, ENTRY_AND_DRAIN);
        let got = delivered(&taps);
        let mut counts = std::collections::BTreeMap::new();
        for epoch in &got {
            *counts.entry(*epoch).or_insert(0) += 1;
        }
        assert!(counts.values().any(|&n| n == 2), "entry delivers a press twice: {got:?}");
        let last = *got.last().unwrap();
        assert!((0..=last).any(|e| !counts.contains_key(&e)), "drain loses an epoch: {got:?}");
    }

    #[test]
    fn steady_lag_and_serial_play_capture_every_epoch_unchanged() {
        for heads in [&[Serial, Serial, Serial][..], &[Lag, Lag, Lag, Lag, Drain][..]] {
            let (taps, _) = run(true, heads);
            let got = delivered(&taps);
            let last = *got.last().unwrap();
            assert_eq!(got, (0..=last).collect::<Vec<_>>());
        }
        // Serial play never derives or carries.
        let (serial, clocks) = run(true, &[Serial, Serial, Serial]);
        assert_eq!(serial, vec![1, 2, 4]);
        assert!(clocks.iter().all(|(_, latched)| !latched));
    }

    #[test]
    fn carried_edges_merge_into_the_next_capture() {
        let mut frame = Frame { keys_pressed: 1, keys_released: 2, mouse_pressed: 1, wheel: (0., 1.), ..Frame::default() };
        merge_edges(&mut frame, &Frame {
            keys_pressed: 4, keys_released: 8, mouse_pressed: 2, mouse_released: 4,
            wheel: (1., 2.), quit: true, keys_down: 16, ..Frame::default()
        });
        assert_eq!((frame.keys_pressed, frame.keys_released), (5, 10));
        assert_eq!((frame.mouse_pressed, frame.mouse_released), (3, 4));
        assert_eq!(frame.wheel, (1., 3.));
        assert!(frame.quit);
        assert_eq!(frame.keys_down, 0, "held state comes from the capture, not the carry");
    }
}
