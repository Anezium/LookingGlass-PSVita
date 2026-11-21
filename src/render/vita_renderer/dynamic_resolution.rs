//! Presentation-only feedback. A completed frame selects one scale for the next frame.
pub(crate) const CEILING: u8 = 75;
const STEP: u8 = 5;
const DOWN_FRAMES: u8 = 2;
const UP_FRAMES: u8 = 45;

#[derive(Default)]
pub(crate) struct DynamicResolution {
    controller: Option<Controller>,
    applied: Option<u8>,
    map: Option<usize>,
}

impl DynamicResolution {
    pub(crate) fn frame(&mut self, floor: Option<u8>, map_index: usize, frame: u64, map: &str) -> Option<f32> {
        if self.map != Some(map_index) {
            self.controller = None;
            self.map = Some(map_index);
        }
        if let Some(floor) = floor {
            self.controller.get_or_insert_with(|| Controller::new(floor));
        } else {
            self.controller = None;
        }
        let scale = self.controller.as_ref().map(|c| c.scale);
        let applied = scale.unwrap_or(CEILING);
        let previous = self.applied.unwrap_or(CEILING);
        if applied != previous {
            crate::spike_log::scale_change(frame, map, previous, applied);
        }
        self.applied = scale;
        scale.map(|scale| f32::from(scale) / 100.)
    }

    pub(crate) fn observe(&mut self, cost: Option<(u32, u32)>) {
        if let Some(controller) = &mut self.controller {
            controller.observe(cost);
        }
    }
}

struct Controller {
    floor: u8,
    scale: u8,
    heavy: u8,
    light: u8,
}

impl Controller {
    fn new(floor: u8) -> Self {
        Self { floor, scale: CEILING, heavy: 0, light: 0 }
    }

    fn observe(&mut self, cost: Option<(u32, u32)>) {
        // Require both a missed frame budget and meaningful GPU backpressure before stepping down.
        // A wider recovery band and longer dwell reject isolated quiet frames inside a burst.
        let heavy = cost.is_some_and(|(frame, end)| frame >= 38_000 && end >= 9_000);
        let light = cost.is_some_and(|(frame, end)| frame <= 28_000 && end <= 4_000);
        self.heavy = if heavy { self.heavy + 1 } else { 0 };
        self.light = if light { self.light + 1 } else { 0 };
        if self.heavy == DOWN_FRAMES {
            self.scale = (self.scale - STEP).max(self.floor);
            self.heavy = 0;
        }
        if self.light == UP_FRAMES {
            self.scale = (self.scale + STEP).min(CEILING);
            self.light = 0;
        }
    }
}

/// The host oracle injects measurements only in its probe build. Host GL has no GXM wait.
#[cfg(all(feature = "sim_maps_probe", not(target_os = "vita")))]
pub(crate) fn host_cost(frame: u64) -> Option<(u32, u32)> {
    static INJECT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if crate::sim_maps::probe::enabled()
        && *INJECT.get_or_init(|| std::env::var_os("LOOKING_GLASS_DYNRES_TEST_LOAD").is_some())
    {
        Some(if (220..280).contains(&frame) { (60_000, 30_000) } else { (22_000, 1_000) })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAVY: Option<(u32, u32)> = Some((60_000, 30_000));
    const LIGHT: Option<(u32, u32)> = Some((22_000, 1_000));

    #[test]
    fn requires_consecutive_gpu_bound_frames_and_gradually_recovers() {
        let mut control = Controller::new(50);
        control.observe(HEAVY);
        control.observe(Some((60_000, 1000)));
        control.observe(HEAVY);
        assert_eq!(control.scale, 75);
        control.observe(HEAVY);
        assert_eq!(control.scale, 70);
        for _ in 1..UP_FRAMES {
            control.observe(LIGHT);
            assert_eq!(control.scale, 70);
        }
        control.observe(LIGHT);
        assert_eq!(control.scale, 75);
    }

    #[test]
    fn both_floors_hold_and_missing_samples_break_dwell() {
        for floor in [50, 60] {
            let mut control = Controller::new(floor);
            for frame in 1..=50 {
                control.observe(HEAVY);
                assert_eq!(control.scale, (75u8.saturating_sub(5 * (frame / 2))).max(floor));
            }
            for _ in 1..UP_FRAMES {
                control.observe(LIGHT);
            }
            control.observe(None);
            control.observe(LIGHT);
            assert_eq!(control.scale, floor);
            for _ in 0..400 {
                control.observe(LIGHT);
            }
            assert_eq!(control.scale, CEILING);
        }
    }

    #[test]
    fn fight_trace_has_no_scale_oscillation() {
        let mut control = Controller::new(60);
        for _ in 0..6 {
            control.observe(HEAVY);
        }
        assert_eq!(control.scale, 60);
        for frame in 0..450 {
            control.observe(if frame % 12 == 0 { Some((32_000, 6_000)) } else { LIGHT });
            assert_eq!(control.scale, 60);
        }
        for _ in 0..135 {
            control.observe(LIGHT);
        }
        assert_eq!(control.scale, 75);
    }
}
