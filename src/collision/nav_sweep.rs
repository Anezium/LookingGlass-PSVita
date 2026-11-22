//! A bounded, plan-local broad phase for the ground detour's short sweeps.
use super::*;

/// Separate windows keep the small corner probes from scanning the body's wider corridor.
#[derive(Default)]
pub(crate) struct NavSweepCache {
    point: Window,
    body: Window,
}

pub(super) struct Window {
    region: Option<Bounds>,
    ranks: [u32; sah::CANDIDATES],
    len: usize,
    dense: bool,
}

impl Default for Window {
    fn default() -> Self {
        Self { region: None, ranks: [0; sah::CANDIDATES], len: 0, dense: false }
    }
}

impl NavSweepCache {
    #[cfg(test)]
    pub(crate) fn has_candidates(&self) -> bool {
        self.body.len > 0 && self.point.len > 0
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
                crate::sim_maps::count("nav_window_verified", 1);
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
        gather: impl FnOnce(&SweepBounds, &mut [u32; sah::CANDIDATES], &mut usize) -> bool,
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
            self.len = 0;
            // With all axes parallel the existing overlap predicate is just the AABB test.
            let enclosing = SweepBounds {
                bounds: region,
                start: Vec3::ZERO,
                inverse: Vec3::ZERO,
                padding: Vec3::ZERO,
            };
            self.dense = !gather(&enclosing, &mut self.ranks, &mut self.len);
            if !self.dense {
                self.ranks[..self.len].sort_unstable();
            }
        }
        // Dense windows use the normal hierarchy until the queries leave the window.
        (!self.dense).then_some(&self.ranks[..self.len])
    }
}

#[cfg(any(test, all(feature = "sim_maps_probe", not(target_os = "vita"))))]
fn bits(t: Trace) -> (u32, [u32; 3], bool, bool) {
    (t.fraction.to_bits(), t.normal.to_array().map(f32::to_bits), t.start_solid, t.all_solid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(geometry: &StaticGeometry, window: &mut Window, a: Vec3, b: Vec3, half: Vec3) -> (bool, u64) {
        let query = SweepBounds::new(a, b, half);
        let hierarchy = geometry.sah.as_ref().expect("SAH fixture");
        let mut counts = sah::Counts::default();
        let cached = hierarchy.trace_in_window(geometry, a, b, half, &query, window, &mut counts);
        let reference = geometry.trace_inner(a, b, half, &query, &mut sah::Counts::default());
        if let Some(hit) = cached {
            assert_eq!(bits(hit), bits(reference), "{a} {b} {half}");
        }
        (cached.is_some(), counts.nodes)
    }

    #[test]
    fn randomized_windows_refill_and_fall_back_when_dense() {
        let mut boxes: Vec<_> = (0..320).map(|i| {
            let min = vec3((i % 20) as f32 * 2., (i / 20) as f32 * 2., -2.);
            (min, min + vec3(1., 1., 4.))
        }).collect();
        boxes.extend((0..256).map(|i| {
            let min = vec3(400. + (i % 16) as f32 * 20., (i / 16) as f32 * 20., -3.);
            (min, min + vec3(9., 8., 7.))
        }));
        let world = World::fixture(&boxes);
        let g = &world.static_geometry;
        let mut window = Window::default();
        let a = vec3(10., 10., 0.);
        let b = a + Vec3::X * 8.;
        assert!(matches!(check(g, &mut window, a, b, Vec3::ONE), (false, n) if n > 1));
        assert_eq!(check(g, &mut window, a, b, Vec3::ONE), (false, 0));
        let mut seed = 0x5429_efa3u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            ((seed >> 16) % 20) as f32 - 10.
        };
        for corner in [vec3(400., 40., 3.), vec3(550., 150., 3.), vec3(690., 290., 3.)] {
            assert!(matches!(check(g, &mut window, corner, corner + Vec3::X, Vec3::ONE),
                             (true, n) if n > 1));
            for i in 0..128 {
                let a = corner + vec3(noise(), noise(), noise());
                let b = a + vec3(noise(), noise(), noise());
                let half = if i % 2 == 0 { Vec3::ZERO } else { vec3(4., 3., 5.) };
                assert_eq!(check(g, &mut window, a, b, half), (true, 0));
            }
        }
    }

    #[test]
    #[ignore = "requires local licensed map data"]
    fn windows_match_real_map_hulls() {
        let data = std::env::var_os("LOOKING_GLASS_TEST_DATA").expect("local data path");
        let mut assets = crate::assets::Assets::open(std::path::Path::new(&data)).unwrap();
        let mut dense = 0;
        let mut sparse = 0;
        let mut reused = 0;
        let mut refilled = 0;
        for name in ["potears2", "wchess1", "grounds1"] {
            let map = Bsp::parse(&assets.read(&format!("maps/{name}.bsp")).unwrap()).unwrap();
            let world = World::from_bsp(&map).unwrap();
            let g = &world.static_geometry;
            let mut window = Window::default();
            for hull in g.hulls.iter().step_by(7) {
                let center = (hull.bounds.min + hull.bounds.max) * 0.5;
                let half = vec3(8., 8., 12.);
                let a = center + vec3(-8., -2., 3.);
                let b = center + vec3(5., 3., -2.);
                let (cached, nodes) = check(g, &mut window, a, b, half);
                dense += usize::from(!cached);
                sparse += usize::from(cached);
                refilled += usize::from(nodes > 1);
                let (repeat, nodes) = check(g, &mut window, b, a, half);
                reused += usize::from(cached && repeat && nodes == 0);
            }
        }
        println!("real SAH windows sparse={sparse} dense={dense} refill={refilled} reuse={reused}");
        assert!(sparse > 256 && dense > 0 && refilled > 128 && reused > 128);
    }
}
