//! `mover_hull_cache` against the original rebuild on the real map. Needs local assets.
use super::*;

#[test]
#[ignore = "requires local assets (LOOKING_GLASS_TEST_DATA)"]
fn fast_mover_colliders_match_the_original_rebuild() -> Result<()> {
    let mut a = Assets::open(std::path::Path::new(&std::env::var("LOOKING_GLASS_TEST_DATA")?))?;
    let map = Bsp::parse(&a.read("maps/wchess1.bsp")?)?;
    let mut run = |fast: bool| -> Result<(Vec<Vec<u32>>, usize)> {
        crate::sim_maps::with_mover_hull_cache(fast, || {
            let mut r = Realm::load(&mut a, &map)?;
            r.saved.pending = None;
            r.saved.intro = true;
            let mut w = World::from_bsp(&map)?;
            let mut p = Player::new(r.data.point("wchess1_start1").translation);
            let (mut rows, mut placed) = (Vec::new(), 0);
            // Uneven steps, as a device tape gives, across several spike periods.
            for step in 0..240 {
                r.advance(0.025 + (step % 7) as f32 * 0.004, &map, &mut w, &mut p, &[])?;
                let mut row = p.feet.to_array().map(f32::to_bits).to_vec();
                for o in &r.objects {
                    row.extend(o.collider.geometry_bits());
                    row.extend(o.volume.geometry_bits());
                    placed += o.shapes.iter().flatten().count();
                }
                rows.push(row);
            }
            Ok((rows, placed))
        })
    };
    let (slow, none) = run(false)?;
    let (fast, placed) = run(true)?;
    ensure!(none == 0 && placed > 0, "templates off {none}, on {placed}");
    ensure!(slow == fast, "fast colliders differ from the original rebuild");
    println!("PASS wchess fast colliders bit-identical over 240 steps ({placed} template-step slots)");
    Ok(())
}
