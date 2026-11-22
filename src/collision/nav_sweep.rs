//! A bounded, plan-local broad phase for the ground detour's short sweeps.
use super::*;

/// Separate windows keep the small corner probes from scanning the body's wider corridor.
#[derive(Default)]
pub(crate) struct NavSweepCache {
    point: Window,
    body: Window,
}

#[derive(Default)]
pub(super) struct Window {
    region: Option<Bounds>,
    ranks: Vec<u32>,
    dense: bool,
}

impl NavSweepCache {
    #[cfg(test)]
    pub(crate) fn has_candidates(&self) -> bool {
        !self.body.ranks.is_empty() && !self.point.ranks.is_empty()
    }

    pub(super) fn trace(
        &mut self,
        geometry: &StaticGeometry,
        start: Vec3,
        end: Vec3,
        half: Vec3,
        query: &SweepBounds,
    ) -> Option<(Trace, u32, u32)> {
        let hierarchy = geometry.sah.as_ref()?;
        let window = if half == Vec3::ZERO {
            &mut self.point
        } else {
            &mut self.body
        };
        let mut counts = sah::Counts::default();
        let timer = crate::phase_census::static_traversal();
        let result =
            hierarchy.trace_in_window(geometry, start, end, half, query, window, &mut counts);
        timer.finish(
            counts.nodes,
            counts.tested,
            counts.clipped,
            counts.planes,
            counts.certificates,
            counts.fallbacks,
        );
        #[cfg(all(feature = "sim_maps_probe", not(target_os = "vita")))]
        if let Some(trace) = result {
            use std::sync::OnceLock;
            static VERIFY: OnceLock<bool> = OnceLock::new();
            if *VERIFY.get_or_init(|| std::env::var_os("LOOKING_GLASS_NAV_SWEEP_VERIFY").is_some())
            {
                let expected =
                    geometry.trace_inner(start, end, half, query, &mut sah::Counts::default());
                let bits = |t: Trace| {
                    (
                        t.fraction.to_bits(),
                        t.normal.to_array().map(f32::to_bits),
                        t.start_solid,
                        t.all_solid,
                    )
                };
                assert_eq!(
                    bits(trace),
                    bits(expected),
                    "nav sweep differs: {start} {end} {half}"
                );
            }
        }
        result.map(|trace| (trace, counts.tested as u32, counts.clipped as u32))
    }
}

impl Window {
    pub(super) fn ranks(
        &mut self,
        query: &SweepBounds,
        gather: impl FnOnce(&SweepBounds, &mut Vec<u32>) -> bool,
    ) -> Option<&[u32]> {
        let bounds = query.bounds;
        if !self.region.is_some_and(|region| {
            bounds.min.cmpge(region.min).all() && bounds.max.cmple(region.max).all()
        }) {
            let pad = Vec3::new(64., 64., 24.);
            let region = Bounds {
                min: bounds.min - pad,
                max: bounds.max + pad,
            };
            self.region = Some(region);
            self.ranks.clear();
            // With all axes parallel the existing overlap predicate is just the AABB test.
            let enclosing = SweepBounds {
                bounds: region,
                start: Vec3::ZERO,
                inverse: Vec3::ZERO,
                padding: Vec3::ZERO,
            };
            self.dense = !gather(&enclosing, &mut self.ranks);
            if !self.dense {
                self.ranks.sort_unstable();
            }
        }
        // Dense windows use the normal hierarchy until the queries leave the window.
        (!self.dense).then_some(&self.ranks)
    }
}
