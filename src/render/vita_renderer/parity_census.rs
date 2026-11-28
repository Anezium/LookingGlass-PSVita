//! Host-only diagnostic for the PC-parity investigation: what the retained world leaves out.
//!
//! `LOOKING_GLASS_PARITY_CENSUS=1` prints one `[lg/parity-census]` JSON line per retained
//! construction, before any geometry is released. It changes nothing that is drawn. For every
//! world batch the retained renderer reads, it counts the layers it drops (detail stages while
//! `detail_layers` is off, stages past `max_layers`), the batches whose vertex work it freezes
//! (sprite deforms, extra deforms, ambient vertex animation), and wave-normal stages it cannot
//! animate. Each count carries triangles and world-space area, so a dropped layer can be weighed
//! by how much surface it covers, and the entries name the shader and first texture of the stage.
use super::*;

#[derive(Default, serde::Serialize)]
struct Tally { layers: usize, batches: usize, triangles: usize, area: f64, #[serde(skip)] largest: (f64, [f32; 3], [f32; 3]) }
impl Tally {
    fn add(&mut self, layers: usize, triangles: usize, area: f64) {
        self.layers += layers; self.batches += 1; self.triangles += triangles; self.area += area;
    }
    /// Keeps the bounds of the largest batch, to place a camera on it.
    fn place(&mut self, area: f64, bounds: (Vec3, Vec3)) {
        if area > self.largest.0 { self.largest = (area, bounds.0.to_array(), bounds.1.to_array()); }
    }
}

fn area_of(b: &super::super::Batch, indices: &[u16]) -> f64 {
    let points: &[Vertex] = if b.base.is_empty() { &b.mesh.vertices } else { &b.base };
    indices.chunks_exact(3).map(|t| {
        let [a, b2, c] = [t[0], t[1], t[2]].map(|i| points.get(i as usize).map_or(Vec3::ZERO, |v| v.position));
        f64::from((b2 - a).cross(c - a).length() * 0.5)
    }).sum()
}
fn area(b: &super::super::Batch) -> f64 { area_of(b, &b.full_indices) }
/// Bounds of the batch's largest face (by area), or of the whole batch when it has no parts.
fn largest_face(b: &super::super::Batch) -> (Vec3, Vec3) {
    b.parts.iter().filter(|p| p.indices.end <= b.full_indices.len())
        .map(|p| (area_of(b, &b.full_indices[p.indices.clone()]), (p.min, p.max)))
        .max_by(|x, y| x.0.total_cmp(&y.0)).map_or(b.bounds, |x| x.1)
}

pub(super) fn print(scene: &Scene, c: &Config, corpus: &str) {
    if std::env::var_os("LOOKING_GLASS_PARITY_CENSUS").is_none() { return; }
    let mut totals: BTreeMap<&'static str, Tally> = BTreeMap::new();
    // (category, shader, first image) -> tally
    let mut entries: BTreeMap<(&'static str, String, String), Tally> = BTreeMap::new();
    let mut world = Tally::default();
    for b in &scene.batches {
        if b.sky { continue; }
        let triangles = b.full_indices.len() / 3;
        let a = area(b);
        let face = largest_face(b);
        world.add(b.layers.len(), triangles, a);
        let shader = b.parts.first().map_or_else(
            || format!("model{}", b.model.map_or(String::new(), |m| m.to_string())),
            |p| scene.map.shaders[scene.map.surfaces[p.face].shader].name.to_ascii_lowercase());
        let mut note = |category: &'static str, layers: usize, image: &str| {
            totals.entry(category).or_default().add(layers, triangles, a);
            let e = entries.entry((category, shader.clone(), image.to_string())).or_default();
            e.add(layers, triangles, a);
            e.place(a, face);
        };
        // Ranks as the construction loop assigns them, and as they would be with detail kept.
        let (mut rank, mut rank_detail) = (0, 0);
        for layer in &b.layers {
            if layer.spec.blend == Blend::Invisible { continue; }
            let image = layer.spec.images.first().map_or("", String::as_str);
            if layer.spec.detail {
                if !c.detail_layers { note("detail_dropped", 1, image); }
                if rank_detail >= c.max_layers { note("third_with_detail_on", 1, image); }
                rank_detail += 1;
                if !c.detail_layers { continue; }
            } else {
                if rank_detail >= c.max_layers { note("third_with_detail_on", 1, image); }
                rank_detail += 1;
            }
            if rank >= c.max_layers { note("third_dropped", 1, image); continue; }
            rank += 1;
        }
        let first = b.layers.first().and_then(|l| l.spec.images.first()).map_or("", String::as_str);
        if b.deforms.iter().any(|d| matches!(d, Deform::Sprite(_))) { note("sprite_frozen", 0, first); }
        if b.deforms.iter().filter(|d| d.moves_vertices()).count() > 1 { note("extra_deform_frozen", 0, first); }
        if b.deforms.iter().any(|d| matches!(d, Deform::WaveNormal(..))) {
            let reads_normal = b.layers.iter().any(|l| l.spec.environment || l.spec.dot_alpha.is_some());
            let moves = b.deforms.iter().any(|d| matches!(d, Deform::Wave(..) | Deform::Move(..) | Deform::Bulge(..)));
            note(if reads_normal && !moves && c.water_normals { "wave_normal_live" } else { "wave_normal_still" }, 0, first);
        }
        if b.animation.is_some() { note("animation_frozen", 0, first); }
        if b.motion.is_some() { note("scripted_motion", 0, first); }
    }
    let top: Vec<_> = entries.into_iter().map(|((category, shader, image), t)| serde_json::json!({
        "category": category, "shader": shader, "image": image, "layers": t.layers, "batches": t.batches,
        "triangles": t.triangles, "area": t.area.round(), "largest_min": t.largest.1, "largest_max": t.largest.2})).collect();
    println!("[lg/parity-census] {}", serde_json::json!({"corpus": corpus, "world": world, "totals": totals, "entries": top}));
}
