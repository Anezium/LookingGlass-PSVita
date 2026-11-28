//! `autosprite_live`: autosprite and autosprite2 quads re-faced every frame from a stream.
//!
//! The retained world packs these quads static at load, so flames and braziers keep their
//! load facing: the hatter1 torch collapses to a sliver seen edge-on, and the jlair2
//! flames hang at their authored angle. The desktop renderer re-faces them every frame
//! after the deforms (`render.rs`'s billboard), and this key does the same for the batches
//! it covers. Their groups keep the exact key-off (parameter, rank, cell) split - the draw
//! count cannot change - but their geometry moves from the immutable world buffers into
//! one small stream (Stream vertices, Immutable indices, the `sky_live` room pattern),
//! and each frame the quads are rewritten from their authored corners with the same
//! `axial_positions`/`facing_positions` the desktop draw calls, then uploaded once.
//!
//! Covered: retained world batches whose deforms carry a Sprite and nothing that would
//! compose differently with the billboard - no wave/bulge/move deform (the shader applies
//! those to the billboarded positions, the desktop applies them before the billboard), no
//! scripted motion or model matrix, no vertex animation. A batch left out stays frozen and
//! is counted in the load log. The jlair2 sky-room sprites take the same change inside
//! the sky set's own stream: the corners are posed to world space, faced to the world
//! camera, and posed back, which is what the desktop's per-batch portal transform did
//! before its own billboard.
use super::*;

/// One quad to re-face: four template vertex ids in authored face order, the authored
/// corners the desktop math reads, and whether autosprite2 locks the long axis.
pub(super) struct Quad {
    pub ids: [u16; 4],
    pub corners: [Vec3; 4],
    pub locked: bool,
    pub uvs: [Vec2; 4],
}
/// Per-pending quad collection while the retained world gathers sprite geometry. The ids
/// are local to that pending's geometry until the stream packs them with an offset.
#[derive(Default)]
pub(super) struct PendingSprites {
    pub quads: Vec<Quad>,
}
/// The whole map's world autosprites: one stream buffer pair plus the quads that drive it.
pub(super) struct SpriteStream {
    vertex: BufferId,
    index: BufferId,
    /// Kept for the host world-packet proof, which resolves each group's draw range.
    pub(super) template: Vec<WorldVertex>,
    pub(super) indices: Vec<u16>,
    quads: Vec<Quad>,
    bytes: usize,
    /// Bits of the last rewrite's camera and time. The opaque and alpha passes of one
    /// frame share both, so the second rewrites and uploads nothing.
    camera: [u32; 3],
    time: u32,
}
impl SpriteStream {
    /// Vertices are uploaded each frame, indices once - the same pair the sky-live room
    /// stream uses, so vitaGL orphans the vertex buffer the same way.
    pub(super) fn new(ctx: &mut dyn RenderingBackend, resources: &mut ConstructionResources,
                      template: Vec<WorldVertex>, indices: Vec<u16>, quads: Vec<Quad>) -> Self {
        let vertex = ctx.new_buffer(BufferType::VertexBuffer, BufferUsage::Stream,
            BufferSource::slice(&template));
        let index = ctx.new_buffer(BufferType::IndexBuffer, BufferUsage::Immutable,
            BufferSource::slice(&indices));
        resources.buffers.extend([vertex, index]);
        let bytes = template.len() * std::mem::size_of::<WorldVertex>() + indices.len() * 2;
        Self { vertex, index, template, indices, quads, bytes, camera: [u32::MAX; 3], time: u32::MAX }
    }
    pub(super) fn buffers(&self) -> (BufferId, BufferId) { (self.vertex, self.index) }
    pub(super) fn bytes(&self) -> usize { self.bytes }
    pub(super) fn quads(&self) -> usize { self.quads.len() }
    pub(super) fn vertices(&self) -> usize { self.template.len() }
    /// Rewrite the quads for `camera` and upload the template. Returns the bytes pushed;
    /// zero when this camera and time were already uploaded.
    pub(super) fn rewrite(&mut self, ctx: &mut dyn RenderingBackend, camera: Vec3, time: f32) -> usize {
        let bits = camera.to_array().map(f32::to_bits);
        if self.camera == bits && self.time == time.to_bits() { return 0; }
        self.camera = bits;
        self.time = time.to_bits();
        for quad in &self.quads {
            for (id, position) in quad.ids.iter().zip(reface(quad, camera)) {
                self.template[*id as usize].position = position.to_array();
            }
        }
        ctx.buffer_update(self.vertex, BufferSource::slice(&self.template));
        self.template.len() * std::mem::size_of::<WorldVertex>()
    }
    pub(super) fn delete(&mut self, ctx: &mut dyn RenderingBackend) {
        ctx.delete_buffer(self.vertex);
        ctx.delete_buffer(self.index);
    }
}
/// The quad's new world-space positions: the desktop math, locked or free.
pub(super) fn reface(quad: &Quad, camera: Vec3) -> [Vec3; 4] {
    if quad.locked {
        crate::render::axial_positions(&quad.corners, camera)
    } else {
        crate::render::facing_positions(&quad.corners, &quad.uvs, camera)
    }
}
/// The same through the sky portal: posed to world space, faced to the world camera,
/// posed back to the room the stream stores. `inverse` is `pose` inverted, taken once.
pub(super) fn reface_through(quad: &Quad, camera: Vec3, pose: Mat4, inverse: Mat4) -> [Vec3; 4] {
    let corners = quad.corners.map(|corner| pose.transform_point3(corner));
    let faced = if quad.locked {
        crate::render::axial_positions(&corners, camera)
    } else {
        crate::render::facing_positions(&corners, &quad.uvs, camera)
    };
    faced.map(|position| inverse.transform_point3(position))
}
/// Bounds covering every billboard facing of `quads`: a sphere per quad around its own
/// center, sized by the farthest authored corner, with the axial midpoints' excursions
/// included and a margin for skewed quads. The authored box when nothing re-faces.
pub(super) fn covering_bounds(quads: &[Quad], fallback: (Vec3, Vec3)) -> (Vec3, Vec3) {
    let (mut min, mut max) = fallback;
    for quad in quads {
        let center = quad.corners.iter().map(|&corner| corner).sum::<Vec3>() / 4.;
        let mut radius = quad.corners.iter().map(|corner| corner.distance(center)).fold(0., f32::max);
        if quad.locked {
            let first = usize::from(quad.corners[0].distance_squared(quad.corners[1])
                > quad.corners[1].distance_squared(quad.corners[2]));
            let ids = [first, (first + 1) % 4, (first + 2) % 4, (first + 3) % 4];
            let [a, b, c, d] = ids.map(|i| quad.corners[i]);
            let lower = (a + b) * 0.5;
            let upper = (c + d) * 0.5;
            let low_radius = a.distance(b) * 0.5;
            let high_radius = c.distance(d) * 0.5;
            radius = radius.max(((lower - center).length_squared() + low_radius * low_radius).sqrt());
            radius = radius.max(((upper - center).length_squared() + high_radius * high_radius).sqrt());
        } else {
            let scale = quad.corners[0].distance(center) * 2. * 0.70710677;
            let uv_radius = quad.uvs.iter().map(|uv| (*uv - Vec2::splat(0.5)).length()).fold(0., f32::max);
            radius = radius.max(scale * uv_radius);
        }
        radius += 1.;
        min = min.min(center - Vec3::splat(radius));
        max = max.max(center + Vec3::splat(radius));
    }
    (min, max)
}
/// Match the desktop's four-vertex chunks in the entire authored vertex array.
fn authored_faces(distinct: &[u16]) -> impl Iterator<Item = [u16; 4]> + '_ {
    distinct.windows(4).filter_map(|chunk| {
        let first = chunk[0];
        (first % 4 == 0 && chunk[3] == first + 3).then_some([first, chunk[1], chunk[2], chunk[3]])
    })
}
/// Quads of one part just appended to a sprite pending: the distinct source vertices in
/// authored order (a face's vertices sit contiguous in `base`, so ascending is authored
/// order), chunked in fours exactly where the desktop whole-array chunks line up with
/// faces. A trailing partial chunk keeps its load facing, as the desktop remainder does.
pub(super) fn record_part(quads: &mut Vec<Quad>, geometry: &Geometry, batch: usize,
                          b: &crate::render::Batch, source: &[u16], locked: bool) {
    let mut distinct = source.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    for face in authored_faces(&distinct) {
        let ids = face.map(|src| geometry.packed(batch, src).expect("an appended source vertex"));
        quads.push(Quad {
            ids,
            corners: face.map(|src| b.base[src as usize].position),
            locked,
            uvs: face.map(|src| b.base[src as usize].uv),
        });
    }
}
/// Quads of a whole sprite batch packed into a sky set's stream: `source[k]` names the
/// batch vertex that stream vertex `first + k` holds, in first-seen order.
pub(super) fn quads_of_batch(b: &crate::render::Batch, locked: bool, first: usize, source: &[u16]) -> Vec<Quad> {
    let mut distinct = source.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    authored_faces(&distinct).map(|face| {
        let ids = face.map(|src| {
            (first + source.iter().position(|&s| s == src).expect("a packed source vertex")) as u16
        });
        Quad { ids, corners: face.map(|src| b.base[src as usize].position), locked,
            uvs: face.map(|src| b.base[src as usize].uv) }
    }).collect()
}
/// Pack every sprite pending's geometry into one stream, in pending order. Returns the
/// stream (None when there is no sprite geometry) and, aligned with `pending`, each
/// sprite pending's (first index, index count, bounds covering every facing); other
/// entries are None. Each pending's indices are contiguous, so a group's draw range
/// resolves to exactly the bytes its geometry held, as the immutable packing guarantees.
pub(super) fn build(ctx: &mut dyn RenderingBackend, resources: &mut ConstructionResources,
                    pending: &mut [Pending]) -> Result<(Option<SpriteStream>, Vec<Option<(i32, i32, Vec3, Vec3)>>)> {
    let mut template: Vec<WorldVertex> = Vec::new();
    let mut indices: Vec<u16> = Vec::new();
    let mut quads: Vec<Quad> = Vec::new();
    let mut ranges = vec![None; pending.len()];
    for (id, p) in pending.iter_mut().enumerate() {
        let Some(sprite) = p.sprite.as_mut() else { continue };
        let offset = template.len();
        let count = p.geometry.indices.len();
        ensure!(offset + p.geometry.vertices.len() <= u16::MAX as usize,
            "autosprite stream exceeds u16 vertices");
        let batch_quads = std::mem::take(&mut sprite.quads);
        let bounds = covering_bounds(&batch_quads, (p.geometry.min, p.geometry.max));
        template.append(&mut p.geometry.vertices);
        let base = indices.len() as i32;
        indices.extend(p.geometry.indices.drain(..).map(|id| id + offset as u16));
        for quad in batch_quads {
            quads.push(Quad { ids: quad.ids.map(|id| (id as usize + offset) as u16), ..quad });
        }
        ranges[id] = Some((base, count as i32, bounds.0, bounds.1));
    }
    if template.is_empty() || indices.is_empty() {
        return Ok((None, ranges));
    }
    let stream = SpriteStream::new(ctx, resources, template, indices, quads);
    Ok((Some(stream), ranges))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn quad_batch(positions: [Vec3; 4], indices: &[u16]) -> crate::render::Batch {
        crate::render::Batch {
            #[cfg(target_os = "vita")]
            static_offset: None,
            face_side: crate::materials::FaceSide::Both,
            motion: None,
            full_indices: indices.to_vec(),
            parts: Vec::new(),
            visibility_work: crate::render::VisibilityWork::default(),
            bounds: (Vec3::ZERO, Vec3::ZERO),
            detail: Vec::new(),
            animation: None,
            mesh: crate::mesh::Mesh { upload_identity: None, surface: None, vertices: Vec::new(),
                indices: Vec::new(), texture: None },
            base: positions.into_iter().zip([vec2(0., 1.), vec2(1., 1.), vec2(1., 0.), vec2(0., 0.)]).map(|(position, uv)| Vertex {
                position,
                uv,
                color: [255; 4],
                normal: Vec4::ZERO,
            }).collect(),
            normals: vec![Vec3::Z; positions.len()],
            deforms: vec![crate::materials::Deform::Sprite(false)],
            sky: false,
            portal: false,
            layers: Vec::new(),
            center: Vec3::ZERO,
            model: None,
        }
    }
    /// One four-vertex face packs to one quad in authored order, and the shared desktop
    /// math places each new corner at the free billboard's exact radius from the center.
    #[test]
    fn a_packed_quad_face_refaces_like_the_desktop() {
        let b = quad_batch([vec3(10., 20., 30.), vec3(10., 24., 30.), vec3(10., 24., 34.),
            vec3(10., 20., 34.)], &[0, 1, 2, 0, 2, 3]);
        let mut geometry = Geometry::default();
        geometry.append(0, &b, &b.full_indices, None);
        let mut sprites = PendingSprites::default();
        record_part(&mut sprites.quads, &geometry, 0, &b, &b.full_indices, false);
        assert_eq!(sprites.quads.len(), 1, "one quad from one four-vertex face");
        let quad = &sprites.quads[0];
        assert_eq!(quad.ids, [0, 1, 2, 3], "authored face order");
        assert_eq!(quad.corners, [b.base[0].position, b.base[1].position,
            b.base[2].position, b.base[3].position]);
        let center = quad.corners.iter().map(|&c| c).sum::<Vec3>() / 4.;
        let radius = quad.corners[0].distance(center) * 0.70710677;
        for position in reface(quad, vec3(500., -70., 42.)) {
            assert!((position.distance(center) - radius * std::f32::consts::SQRT_2).abs() < 1e-4,
                "the free billboard keeps the authored corner radius");
        }
        // Facing the camera: the quad plane's normal points at it.
        let faced = reface(quad, vec3(500., -70., 42.));
        let normal = (faced[1] - faced[0]).cross(faced[2] - faced[1]);
        assert!(normal.dot(vec3(500., -70., 42.) - center) > 0.);
    }
    /// `quads_of_batch` maps first-seen packing back to authored order, chunking a whole
    /// batch exactly as the desktop whole-array chunks line up with its faces.
    #[test]
    fn sky_quads_of_batch_keep_face_chunks_and_authored_order() {
        let positions = (0..8).map(|i| vec3(i as f32, i as f32 * 3., i as f32 * 7.)).collect::<Vec<_>>();
        let mut b = quad_batch([Vec3::ZERO; 4], &[]);
        b.base = positions.iter().map(|&position| Vertex {
            position, uv: vec2(0.5, 0.5), color: [255; 4], normal: Vec4::ZERO,
        }).collect();
        // Two quad faces; the second's triangulation starts at its own vertex 5.
        b.full_indices = vec![0, 1, 2, 0, 2, 3, 5, 6, 7, 5, 7, 4];
        let source = [3_u16, 2, 1, 0, 5, 6, 7, 4]; // first-seen order, two rotations
        let quads = quads_of_batch(&b, true, 100, &source);
        assert_eq!(quads.len(), 2, "one quad per face");
        assert_eq!(quads[0].ids, [103, 102, 101, 100], "face one in authored order");
        assert_eq!(quads[1].ids, [107, 104, 105, 106], "face two in authored order");
        assert_eq!(quads[0].corners, [positions[0], positions[1], positions[2], positions[3]]);
        assert_eq!(quads[1].corners, [positions[4], positions[5], positions[6], positions[7]]);
        assert!(quads.iter().all(|q| q.locked));
        // A trailing partial chunk keeps its load facing, as the desktop remainder does.
        assert!(quads_of_batch(&b, false, 0, &[1, 2, 3, 4]).is_empty());
    }
    /// The covering bounds hold every facing, locked and free, from many camera angles.
    #[test]
    fn covering_bounds_hold_every_facing() {
        let quad = Quad {
            ids: [0, 1, 2, 3],
            corners: [vec3(-2372., 2586., -470.), vec3(-2372., 2586., -400.),
                vec3(-2372., 2658., -400.), vec3(-2372., 2658., -470.)],
            locked: true,
            uvs: [vec2(1.99, 1.99), vec2(1.99, 0.99), vec2(0.99, 0.99), vec2(0.99, 1.99)],
        };
        for locked in [true, false] {
            let mut quad = quad;
            quad.locked = locked;
            let (min, max) = covering_bounds(std::slice::from_ref(&quad), (Vec3::ZERO, Vec3::ZERO));
            for step in 0..24 {
                let angle = step as f32 * std::f32::consts::TAU / 24.;
                let camera = vec3(angle.cos(), angle.sin(), 0.3) * 250. + vec3(-2372., 2622., -435.);
                for position in reface(&quad, camera) {
                    assert!(min.cmple(position).all() && position.cmple(max).all(),
                        "locked={locked} faced corner {position:?} outside {min:?}..{max:?}");
                }
                // Through a portal pose, the room-space result stays inside the same
                // bounds: the pose is a rotation about the origin plus a translation.
                let pose = Mat4::from_translation(vec3(80., -20., 4.))
                    * Mat4::from_quat(Quat::from_rotation_z(0.7)) * Mat4::from_translation(-Vec3::ONE);
                let room = reface_through(&quad, camera, pose, pose.inverse());
                for position in room {
                    assert!(min.cmple(position).all() && position.cmple(max).all(),
                        "locked={locked} posed corner {position:?} outside {min:?}..{max:?}");
                }
            }
        }
        // Nothing to re-face: the authored box.
        assert_eq!(covering_bounds(&[], (Vec3::ONE, Vec3::splat(2.))), (Vec3::ONE, Vec3::splat(2.)));
    }
}
