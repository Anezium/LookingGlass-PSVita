//! Lean fragment programs for actor packets, behind `actor_lean_shader`, default off.
//!
//! The actor program (`render::FRAGMENT` with eight dynamic lights and the fog block) is the
//! longest one the console runs. A model packet with no dynamic light and no appearance effect
//! executes most of it for nothing:
//!
//! - both `gl_FragCoord` discards test `FxAppearance.x > 0` and `FxAppearance.y > 0.5`: false;
//! - `FxModel = 1` fails `FxModel < 0.5`, so the light atlas is never read;
//! - the sixteen packed light vectors are zero (`lighting::packed`), so `dynamicLight` returns
//!   `vec3(0.0)` and `rgb += diffuse.rgb*vec3(0.0)*Gain.rgb` adds zero for a finite gain;
//! - `FxAppearance.z <= 0.5` and `FxAppearance.w == 0`: no tint, no filter fade;
//! - `FogMode <= 3.5` skips the source-colour fog branch, which holds a second copy of `fogged`;
//! - `SkyFade` is zero in every retained payload: no backdrop fetch.
//!
//! The lean source is `render::FRAGMENT` with exactly those lines removed. The texture read,
//! the alpha product, both cutout lines, the vertex lighting, the gain, the unlit colour
//! product and the `fogged` call stay byte for byte, so a packet that can reject fragments
//! still rejects the same ones. The one edited line is the fog call's guard, which loses the
//! `else` of the branch that went. `admitted` reads every condition from the packet's own
//! uniform payload, the words the program would have tested.
//!
//! The second program also drops the two cutout lines. It serves only packets that
//! `actor_discard_free_shader` or `actor_cutout_discard_free` already send to the discard-free
//! program, so it never widens the set of packets drawn without `discard`.
//!
//! `actor_fogfree_route` adds a twin of each unlit program whose `fogged` returns its argument
//! (see `fog_free_source` and `fog_free`).
use super::*;

pub(super) const NAMES: [&str; 6] = [
    "retained-actor-lean", "retained-actor-lean-opaque",
    "retained-actor-lean-lit", "retained-actor-lean-lit-opaque",
    "retained-actor-lean-fog-free", "retained-actor-lean-opaque-fog-free",
];

// Offsets into the retained uniform block (`UNIFORM_FIELDS`).
const GAIN: usize = 39;
const FOG_MODE: usize = 43;
const SKY_FADE: usize = 46;
const FX_MODEL: usize = 47;
const APPEARANCE: usize = 48;
const LIGHTS: usize = 52;

/// A `render::FRAGMENT` line that cannot run, or adds zero, for an admitted packet.
fn unreachable_line(line: &str) -> bool {
    ["FxAppearance.x", "FxAppearance.y", "LightAtlas,sampleUV", "FxAppearance.z",
     "FxAppearance.w", "// Multiplicative actor layers",
     "uniform sampler2D SkyBackdrop;"].iter().any(|marker| line.contains(marker))
}
/// The lean source: `discard_free` also removes the two cutout lines.
fn fragment(discard_free: bool, lit: bool) -> String {
    let full = crate::render::FRAGMENT;
    // The backdrop block is the only multi-line statement that goes: two comment lines, the
    // test and its four body lines, up to the output write.
    let (head, rest) = full.split_once(" // The remastered Fortress exterior").expect("backdrop block");
    let (_, tail) = rest.split_once(" gl_FragColor=").expect("output write");
    let source = format!("{head} gl_FragColor={tail}");
    // The source-colour fog branch (`FogMode > 3.5`) goes with its comments; the plain fog
    // call keeps its own guard.
    let (head, rest) = source.split_once(" if(FogMode>3.5){").expect("source-colour fog branch");
    let (_, tail) = rest.split_once(" }else if(FogMode>=0.0)").expect("fog call");
    let source = format!("{head} if(FogMode>=0.0){tail}");
    source.lines().filter(|line| !unreachable_line(line)
        && (lit || !(line.contains("dynamicLight(") || line.contains("// LIGHTS")))
        && !(discard_free && line.contains("discard;")))
        .map(|line| line.replace(" uniform sampler2D LightAtlas; uniform vec2 AtlasGrid;", "")
            .replace(" uniform vec4 FxAppearance;", ""))
        .collect::<Vec<_>>().join("\n")
}
/// The two program sources, `[lean, lean without discard]`. A host probe build can read
/// replacements from an absolute folder (`lean.frag`, `lean-opaque.frag`) while a pixel
/// difference is being attributed; a missing file stops the run. Never compiled for the console.
pub(super) fn sources() -> [String; 2] {
    #[cfg(all(feature = "sim_maps_probe", not(target_os = "vita")))]
    if let Some(folder) = std::env::var_os("LOOKING_GLASS_ACTOR_LEAN_SOURCES") {
        let read = |name: &str| std::fs::read_to_string(std::path::Path::new(&folder).join(name))
            .unwrap_or_else(|e| panic!("LOOKING_GLASS_ACTOR_LEAN_SOURCES {name}: {e}"));
        return [read("lean.frag"), read("lean-opaque.frag")];
    }
    [fragment(false, false), fragment(true, false)]
}

pub(super) fn lit_source(discard_free: bool) -> String {
    crate::lighting::fragment_slots(&fragment(discard_free, true), 2)
}
/// `actor_fogfree_route`: an unlit lean source whose fog block is only the `worldPosition`
/// declaration (the interpolants match the full program's) and a `fogged` that returns its
/// argument, as `simple_fog` does. `main` stays byte for byte, guarded call included: removing
/// the call changed the host GPU's rounding on a few pixels (wchess1, 1/255) even with every
/// fog term off, while this form drew the same pixels.
pub(super) fn fog_free_source(discard_free: bool) -> String {
    let lean = fragment(discard_free, false);
    assert_eq!(lean.matches("// FOG").count(), 1, "lean fog block");
    lean.replacen("// FOG", FOG_FREE_BLOCK, 1)
}
const FOG_FREE_BLOCK: &str = "varying highp vec3 worldPosition;\nvec3 fogged(vec3 rgb,float additive){return rgb;}";
/// Every line the lean source leaves out is a no-op under payload `u`. Each test is the
/// negation of the shader's own guard, so NaN fails the guard here as it does there.
pub(super) fn admitted(p: &Pass, u: &[f32; UNIFORM_FLOATS]) -> bool {
    !has_lights(u, 0) && lean_material(p, u)
}

pub(super) fn admitted_lit(p: &Pass, u: &[f32; UNIFORM_FLOATS], culled_to_zero: bool) -> bool {
    p.lit && (culled_to_zero || has_lights(u, 0)) && !has_lights(u, 2) && lean_material(p, u)
}

pub(super) fn has_lights(u: &[f32; UNIFORM_FLOATS], first: usize) -> bool {
    u[LIGHTS + first * 8..LIGHTS + 64].chunks_exact(8).any(|slot| slot[7] > 0.5)
}

pub(super) fn lean_material(p: &Pass, u: &[f32; UNIFORM_FLOATS]) -> bool {
    let a = &u[APPEARANCE..APPEARANCE + 4];
    !p.effect
        && !(a[0] > 0.) && !(a[1] > 0.5) && !(a[2] > 0.5) && !(a[3] > 0.) && !(a[3] < 0.)
        && !(u[FX_MODEL] < 0.5)
        && !(u[FOG_MODE] > 3.5)
        && !(u[SKY_FADE] > 0.5)
        && u[GAIN..GAIN + 3].iter().all(|gain| gain.is_finite())
}
// CPU positions are the identity-model vertex inputs. Perspective-correct
// interpolation over the clipped triangle stays in the vertex box, up to
// rounding. The Vita translator erases GLSL precision qualifiers; the same
// deliberately wide guard also covers mediump rounding on a GLSL backend.
// Keep finite, bounded inputs and require one axis alone to clear twice
// max(radius, 0.001) plus a coordinate allowance. Never sit on the cutoff.
fn outside_light(lo: Vec3, hi: Vec3, slot: &[f32]) -> bool {
    if !slot[..7].iter().all(|x| x.abs() <= 8192.) || slot[3] <= 0. {
        return false;
    }
    let light = Vec3::from_slice(slot);
    let gap = (lo - light).max(light - hi).max_element();
    let rounding = 1. + (lo.abs().max(hi.abs()).max(light.abs()).max_element() / 64.);
    gap > 2. * slot[3].max(0.001) + rounding
}

/// Turn off zero terms and keep the surviving terms in their original order.
/// GPU skinning has only frustum bounds, which are not a certificate for
/// floating-point interpolation of the actual skinned positions.
pub(super) fn cull_lights(p: &Pass, u: &mut [f32; UNIFORM_FLOATS]) -> bool {
    if p.gpu_skin.is_some() || !p.lit || !has_lights(u, 0) || !lean_material(p, u)
        || !u[GAIN..GAIN + 3].iter().all(|gain| gain.abs() <= 8192.) {
        return false;
    }
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for vertex in &p.geometry().vertices {
        let pos = vertex.position;
        if !(pos.abs().max_element() <= 8192. && pos.is_finite()) {
            return false;
        }
        lo = lo.min(pos);
        hi = hi.max(pos);
    }
    if !lo.cmple(hi).all() { return false; }

    let selected = &mut u[LIGHTS..LIGHTS + 64];
    let removed = selected.chunks_exact(8).enumerate().fold(0u8, |mask, (i, slot)| {
        mask | (u8::from(slot[7] > 0.5 && outside_light(lo, hi, slot)) << i)
    });
    if removed == 0 { return false; }
    let mut kept = 0;
    for i in 0..8 {
        let start = i * 8;
        if selected[start + 7] > 0.5 && removed & (1 << i) == 0 {
            selected.copy_within(start..start + 8, kept * 8);
            kept += 1;
        }
    }
    selected[kept * 8..].fill(0.);
    kept == 0
}

/// The certified glow samples are black before lighting. With the same
/// bounded world positions and finite light terms, black remains exactly zero.
pub(super) fn bounded_glow_lights(u: &[f32; UNIFORM_FLOATS]) -> bool {
    u[LIGHTS..LIGHTS + 16].chunks_exact(8).all(|slot| {
        slot[7] <= 0.5 || (slot[..7].iter().all(|x| x.abs() <= 8192.) && slot[3] > 0.)
    })
}

// More offsets into the retained uniform block: the fog terms after `Model` (0..16).
const EYE: usize = 116;
const DISTANCE: usize = 119;
const LIQUID_W: usize = 126;
const FOG_A_W: usize = 136;
const FOG_B_W: usize = 146;
/// Same coordinate limit as `outside_light`.
const FOG_LIMIT: f32 = 8192.;

/// `actor_fogfree_route`: `fogged(rgb,FogMode)` returns `rgb` for every fragment of this
/// packet under payload `u`, so the fog-free twin draws the same pixels. Decided per draw
/// from the payload words the program reads; nothing is kept across draws.
///
/// The call runs only for `FogMode >= 0`. Each fog term has its own `if(X.w>0.0)` guard,
/// and NaN fails the guard here as it does there. With the liquid and both volume terms off
/// and distance fog off, `fogged` changes nothing. With distance fog on, the factor is
/// `clamp((dist-0.35w)/(0.65w),0,1)`: it is +0 once `dist < 0.35w`, and then
/// `mix(rgb,c,0)` returns `rgb` for a finite `rgb` (finite gain, `lean_material`) and a
/// finite fog colour `c` (checked here).
///
/// `dist` is `length(worldPosition-FogEye)`. The vertex program writes `worldPosition` as
/// `Model*position`, and `Model` must be the identity, so it is the uploaded vertex position.
/// `Pass::geometry` holds the vertices the draw uploads: final world positions for static and
/// CPU-skinned packets, the same assumption `cull_lights` makes. On a single-sample target
/// (the key requires `msaa_samples=1`) a fragment's `worldPosition` is a convex combination of
/// its triangle's vertices, and distance is convex, so no fragment is farther from the eye
/// than the packet's farthest vertex. Every vertex is tested, until one fails. A GPU-skinned
/// packet has only placeholder geometry on the CPU and is refused.
///
/// The threshold is `0.35*w` in f32 from the live word. `dist` is mediump (`render.rs`): a
/// backend that keeps half precision rounds it by 0.25 at 630. The margin `2 + s/4096` covers
/// that and the f32 interpolation and `length` rounding, so a packet never sits on the cutoff.
/// `s = |eye| + 0.35w` (largest coordinate) bounds every coordinate of a vertex that passes.
/// The CPU test compares squared f32 distances: its own rounding is below 0.01 unit here.
pub(super) fn fog_free(p: &Pass, u: &[f32; UNIFORM_FLOATS]) -> bool {
    if p.gpu_skin.is_some() || u[..16] != Mat4::IDENTITY.to_cols_array() { return false; }
    if !(u[FOG_MODE] >= 0.) { return true; }
    if u[LIQUID_W] > 0. || u[FOG_A_W] > 0. || u[FOG_B_W] > 0. { return false; }
    let w = u[DISTANCE + 3];
    if !(w > 0.) { return true; }
    let bounded = |values: &[f32]| values.iter().all(|x| x.abs() <= FOG_LIMIT);
    if !(w >= 1. && w <= 65504. && u[FOG_MODE] <= 3.5 && bounded(&u[DISTANCE..DISTANCE + 3])
        && bounded(&u[GAIN..GAIN + 4]) && bounded(&u[EYE..EYE + 3])) {
        return false;
    }
    let eye = Vec3::from_slice(&u[EYE..EYE + 3]);
    let radius = 0.35f32 * w;
    let limit = radius - 2. - (eye.abs().max_element() + radius) / 4096.;
    if !(limit > 0.) { return false; }
    let limit2 = limit * limit;
    let vertices = &p.geometry().vertices;
    // A non-finite position fails the comparison.
    let far = vertices.iter().position(|v| { let d = v.position - eye; !(d.dot(d) < limit2) });
    #[cfg(all(feature = "sim_maps_probe", not(target_os = "vita")))]
    count("vita_actor_fog_free_vertices", far.map_or(vertices.len(), |i| i + 1) as u64);
    !vertices.is_empty() && far.is_none()
}

/// Host census line for one actor draw: the payload facts the selector reads and the route.
#[cfg(all(feature = "sim_maps_probe", not(target_os = "vita")))]
pub(super) fn census(p: &Pass, u: &[f32; UNIFORM_FLOATS], depth_write: bool, route: Route, culled_to_zero: bool) {
    if p.effect || !crate::character::skinning::census::active() { return; }
    let surface = p.mesh.surface.as_ref();
    let mesh = p.geometry();
    println!("ACTOR_GPU2 {}", serde_json::json!({
        "frame": crate::sim_maps::probe::frame(), "label": surface.map(|s| s.label.clone()),
        "route": format!("{route:?}"), "admitted": admitted(p, u),
        "culled_to_zero": culled_to_zero, "lean_lit": route.is_lean_lit(), "full": route == Route::Full,
        "blend": p.blend_index, "equal": p.depth_equal, "depth_write": depth_write,
        "lit": p.lit, "clamp": p.clamp, "cutout": p.cutout, "cutout_reference": p.cutout_reference,
        "gain": p.gain.to_array(), "fog_mode": p.fog, "model": p.model, "fullbright": u[34],
        "dissolve": p.appearance.dissolve, "ghost": p.appearance.ghost, "power": p.appearance.power,
        "filter_fade": p.filter_fade,
        "lights_on": (0..8).filter(|light| u[LIGHTS + light * 8 + 7] > 0.5).count(),
        "fog_w": [u[122], u[126], u[136], u[146]], "fog_free": add_program::fog_free(p, u),
        "layers": surface.map(|s| s.layers.iter().map(|l| (l.stage.environment, l.stage.alpha_test)).collect::<Vec<_>>()),
        "vertex_alpha_opaque": mesh.vertices.iter().all(|v| v.color[3] == 255),
        "triangles": mesh.indices.len() / 3, "vertices": mesh.vertices.len(),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pass() -> Pass {
        Pass {
            gpu_skin: None,
            _game_thread: crate::game_thread::GameThread::default(), geometry: None, effect: false,
            blend_index: 0, depth_equal: false,
            #[cfg(target_os = "vita")] world_static: None,
            #[cfg(target_os = "vita")] world_geometry_counted: false,
            mesh: Mesh { upload_identity: None, surface: None,
                vertices: vec![Vertex::new(0., 0., 0., 0., 0., WHITE); 3], indices: vec![0, 1, 2], texture: None },
            material: None, lit: true, clamp: false, cutout: 4., cutout_reference: 0.5, gain: Vec4::ONE,
            fog: 0., model: true, appearance: Default::default(), filter_fade: 0.,
        }
    }
    /// The payload words `uniforms` writes for `p`, without a retained frame.
    fn payload(p: &Pass) -> [f32; UNIFORM_FLOATS] {
        let mut u = [0.; UNIFORM_FLOATS];
        u[35] = f32::from(p.lit); u[36] = f32::from(p.clamp); u[37] = p.cutout; u[38] = p.cutout_reference;
        u[GAIN..GAIN + 4].copy_from_slice(&p.gain.to_array()); u[43] = p.fog; u[FX_MODEL] = f32::from(p.model);
        u[APPEARANCE..APPEARANCE + 4].copy_from_slice(
            &[p.appearance.dissolve, f32::from(p.appearance.ghost), p.appearance.power, p.filter_fade]);
        u
    }
    #[test]
    fn offsets_name_the_fields_of_the_retained_block() {
        let end = |name: &str| UNIFORM_FIELDS.iter().find(|f| f.0 == name).unwrap().1;
        assert_eq!(end("Gain"), GAIN + 4);
        assert_eq!(end("FogMode"), FOG_MODE + 1);
        assert_eq!(end("SkyFade"), SKY_FADE + 1);
        assert_eq!(end("FxModel"), FX_MODEL + 1);
        assert_eq!(end("FxAppearance"), APPEARANCE + 4);
        assert_eq!(end("FxAppearance"), LIGHTS);
        assert_eq!(end("Lights"), LIGHTS + 64);
        // `lighting::packed` writes position+radius then colour+1 per light: the on flag is word 7.
        assert_eq!(crate::lighting::uniforms().len(), 16);
    }
    #[test]
    fn only_packets_whose_removed_lines_are_no_ops_are_admitted() {
        let p = pass();
        assert!(admitted(&p, &payload(&p)));
        for (name, adjust) in [
            ("effect", (|p: &mut Pass| p.effect = true) as fn(&mut Pass)),
            ("dissolve", |p: &mut Pass| p.appearance.dissolve = 0.001),
            ("ghost", |p: &mut Pass| p.appearance.ghost = true),
            ("power tint", |p: &mut Pass| p.appearance.power = 1.),
            ("rage tint", |p: &mut Pass| p.appearance.power = 2.),
            ("filter fade to white", |p: &mut Pass| p.filter_fade = 0.25),
            ("filter fade to grey", |p: &mut Pass| p.filter_fade = -0.25),
            ("world model reads the light atlas", |p: &mut Pass| p.model = false),
            ("source-colour fog", |p: &mut Pass| p.fog = 4.),
            ("non-finite gain", |p: &mut Pass| p.gain.x = f32::INFINITY),
        ] {
            let mut p = pass();
            adjust(&mut p);
            assert!(!admitted(&p, &payload(&p)), "{name} was admitted");
        }
        // Blend, depth state, lighting, clamping, cutout mode and fog mode are all still
        // evaluated by the kept lines, so none of them decides admission.
        for adjust in [(|p: &mut Pass| p.blend_index = 1) as fn(&mut Pass), |p: &mut Pass| p.depth_equal = true,
            |p: &mut Pass| p.lit = false, |p: &mut Pass| p.clamp = true, |p: &mut Pass| p.cutout = 1.,
            |p: &mut Pass| p.cutout = 0., |p: &mut Pass| p.fog = 1., |p: &mut Pass| p.fog = 3., |p: &mut Pass| p.fog = -1.,
            |p: &mut Pass| p.gain.w = 0.3] {
            let mut p = pass();
            adjust(&mut p);
            assert!(admitted(&p, &payload(&p)));
        }
        // Any dynamic light, in any of the eight slots, keeps the full program.
        for light in 0..8 {
            let mut u = payload(&p);
            u[LIGHTS + light * 8 + 7] = 1.;
            assert!(!admitted(&p, &u), "light {light}");
        }
        let mut u = payload(&p);
        u[SKY_FADE] = 1.;
        assert!(!admitted(&p, &u), "backdrop fade");
    }
    #[test]
    fn the_lean_sources_are_the_full_program_minus_the_unreachable_lines() {
        let full = crate::render::FRAGMENT;
        let [lean, opaque] = [fragment(false, false), fragment(true, false)];
        assert_eq!(sources(), [lean.clone(), opaque.clone()]);
        // Every lean line is a full-program line, in the full program's order, except the two
        // uniform declarations that lose unused names and the fog call that loses its `else`.
        let fog_call = " if(FogMode>=0.0)rgb=fogged(rgb,FogMode);";
        assert!(full.contains("\n }else if(FogMode>=0.0)rgb=fogged(rgb,FogMode);\n"));
        let mut cursor = full.lines();
        for line in lean.lines() {
            if line == "uniform sampler2D Texture;" || line == "uniform float FxModel;" || line == fog_call { continue; }
            assert!(cursor.any(|candidate| candidate == line), "not a full-program line in order: {line}");
        }
        assert!(full.contains("uniform sampler2D Texture; uniform sampler2D LightAtlas; uniform vec2 AtlasGrid;"));
        assert!(full.contains("uniform float FxModel; uniform vec4 FxAppearance;"));
        // Exactly these statements are gone.
        let gone: Vec<&str> = full.lines().filter(|line| !lean.lines().any(|kept| kept == *line)
            && !line.starts_with("uniform sampler2D Texture;") && !line.starts_with("uniform float FxModel;")).collect();
        let markers = [
            "uniform sampler2D SkyBackdrop;", "// LIGHTS", "FxAppearance.x>0.0", "FxAppearance.y>0.5",
            "if(lm.z>0.5&&FxModel<0.5)", "rgb+=diffuse.rgb*dynamicLight(worldPosition)*Gain.rgb;",
            "if(FxAppearance.z>1.5)", "// Multiplicative actor layers", "if(FxAppearance.w>0.0)",
            " if(FogMode>3.5){", "// Source-alpha/source-colour", "// fully fogged destination",
            "float visible=fogged(vec3(1.0),1.0).r;", "rgb=mix(vec3(1.0),rgb,visible);alpha*=visible;",
            " }else if(FogMode>=0.0)rgb=fogged(rgb,FogMode);",
            "// The remastered Fortress exterior", "// sky, preserving", "if(SkyFade>0.5",
            "float a=FogA.w>0.0", "float b=FogB.w>0.0", "rgb+=texture2D(SkyBackdrop", " }"];
        assert_eq!(gone.len(), markers.len(), "{gone:#?}");
        for (line, marker) in gone.iter().zip(markers) {
            assert!(line.contains(marker), "{line} / {marker}");
        }
        assert!(!lean.contains("SkyBackdrop") && full.contains("rgb+=texture2D(SkyBackdrop"));
        for kept in [
            " vec2 coords=mix(uv,clamp(uv,vec2(0.001),vec2(0.999)),ClampUV);vec4 diffuse=texture2D(Texture,coords);",
            " float alpha=diffuse.a*Gain.a*color.a;",
            " vec3 lighting=max(color.rgb,vec3(0.18));",
            " vec3 rgb=diffuse.rgb*mix(vec3(1.0),lighting,Lit*(1.0-Fullbright))*Gain.rgb;",
            " if(FxModel>0.5&&Lit<0.5)rgb*=color.rgb;",
            fog_call, " gl_FragColor=vec4(rgb,alpha);", "// FOG",
        ] {
            for source in [&lean, &opaque] { assert_eq!(source.lines().filter(|l| *l == kept).count(), 1, "{kept}"); }
        }
        for source in [&lean, &opaque] {
            for absent in ["LightAtlas", "AtlasGrid", "SkyBackdrop", "SkySize", "SkyFade", "dynamicLight", "FxAppearance", "gl_FragCoord", "visible", "else"] {
                assert!(!source.contains(absent), "{absent}");
            }
            assert!(source.ends_with(" gl_FragColor=vec4(rgb,alpha);\n}"));
        }
        // The lean program keeps both cutout lines byte for byte; the opaque one differs from
        // it by exactly those two lines.
        let cutouts: Vec<&str> = full.lines().filter(|l| l.contains("Cutout>") && l.contains("discard;")).collect();
        assert_eq!(cutouts.len(), 2);
        assert_eq!(lean.matches("discard").count(), 2);
        assert_eq!(opaque, lean.lines().filter(|l| !cutouts.contains(l)).collect::<Vec<_>>().join("\n"));
        assert!(!opaque.contains("discard"));
        // The interpolant declarations match the full program's, in its order.
        let varyings = |source: &str| source.split(';').map(str::trim).filter(|d| d.contains("varying "))
            .map(|d| d.rsplit(' ').next().unwrap().to_owned()).collect::<Vec<_>>();
        let expected = varyings(&crate::environment::fragment(full));
        assert_eq!(expected, ["uv", "lm", "color", "worldPosition"]);
        for source in [&lean, &opaque] { assert_eq!(varyings(&crate::environment::fragment(source)), expected); }
    }
    #[test]
    fn lit_source_uses_the_exact_first_two_full_terms_and_original_cutouts() {
        let source = fragment(false, true);
        let expanded = crate::lighting::fragment_slots(&source, 2);
        let full = crate::lighting::fragment_slots(crate::render::FRAGMENT, 8);
        let mut full_lines = full.lines();
        for line in expanded.lines().filter(|line| line.contains("FxLight") || line.contains("dynamicLight(")) {
            assert!(full_lines.any(|full| line == full));
        }
        assert!(!expanded.contains("FxLight2"));
        assert_eq!(expanded.matches("discard;").count(), 2);
        assert!(!lit_source(true).contains("discard;"));

        let p = pass();
        let mut u = payload(&p);
        u[LIGHTS + 15] = 1.;
        assert!(admitted_lit(&p, &u, false));
        u[LIGHTS + 23] = 1.;
        assert!(!admitted_lit(&p, &u, false));
    }

    #[test]
    fn culling_keeps_cutoff_contacts_and_compacts_only_zero_terms_stably() {
        let mut p = pass();
        let mut u = payload(&p);
        let put = |u: &mut [f32], index: usize, x: f32, red: f32| {
            u[LIGHTS + index * 8..][..8].copy_from_slice(&[x, 0., 0., 10., red, 1., 1., 1.]);
        };
        put(&mut u, 0, 100., 1.);
        put(&mut u, 1, 10., 2.);
        put(&mut u, 2, -100., 3.);
        put(&mut u, 3, -10., 4.);
        assert!(!cull_lights(&p, &mut u));
        assert_eq!((u[LIGHTS + 4], u[LIGHTS + 12]), (2., 4.));
        assert!(admitted_lit(&p, &u, false));
        assert!(!admitted(&p, &u));

        let mut u = payload(&p);
        put(&mut u, 0, 100., 1.);
        p.mesh.vertices[0].position.x = f32::NAN;
        assert!(!cull_lights(&p, &mut u));
        assert_eq!(u[LIGHTS + 7], 1.);
        p.mesh.vertices[0].position = Vec3::ZERO;
        assert!(cull_lights(&p, &mut u));
        assert!(admitted(&p, &u));
        assert!(!admitted_lit(&p, &u, false));
        assert!(admitted_lit(&p, &u, true));
    }

    fn fogged_payload(p: &Pass, eye: Vec3, w: f32) -> [f32; UNIFORM_FLOATS] {
        let mut u = payload(p);
        u[..16].copy_from_slice(&Mat4::IDENTITY.to_cols_array());
        u[EYE..EYE + 3].copy_from_slice(&eye.to_array());
        u[DISTANCE..DISTANCE + 4].copy_from_slice(&[0.3, 0.2, 0.1, w]);
        u
    }
    #[test]
    fn the_fog_free_sources_are_the_unlit_lean_sources_without_the_fog_call() {
        let full = crate::environment::fragment(crate::render::FRAGMENT);
        let varyings = |source: &str| source.split(';').map(str::trim).filter(|d| d.contains("varying "))
            .map(|d| d.rsplit(' ').next().unwrap().to_owned()).collect::<Vec<_>>();
        for discard_free in [false, true] {
            let lean = fragment(discard_free, false);
            let free = fog_free_source(discard_free);
            let gone: Vec<&str> = lean.lines().filter(|l| !free.lines().any(|kept| kept == *l)).collect();
            assert_eq!(gone, ["// FOG"]);
            let added: Vec<&str> = free.lines().filter(|l| !lean.lines().any(|kept| kept == *l)).collect();
            assert_eq!(added, ["varying highp vec3 worldPosition;", "vec3 fogged(vec3 rgb,float additive){return rgb;}"]);
            assert_eq!(free.lines().count(), lean.lines().count() + 1);
            assert_eq!(free.matches(" if(FogMode>=0.0)rgb=fogged(rgb,FogMode);").count(), 1, "the call stays");
            for absent in ["FogEye", "DistanceFog", "LiquidFog", "FogA", "FogB", "length("] {
                assert!(!free.contains(absent), "{absent}");
            }
            assert_eq!(crate::environment::fragment(&free), free, "no fog block left to expand");
            assert_eq!(varyings(&free), varyings(&full));
            assert_eq!(free.matches("discard;").count(), lean.matches("discard;").count());
        }
    }
    #[test]
    fn fog_free_needs_every_vertex_inside_the_zero_fog_radius_with_a_margin() {
        let mut p = pass();
        p.mesh.vertices[1].position = Vec3::new(10., 0., 0.);
        p.mesh.vertices[2].position = Vec3::new(0., 10., 0.);
        let at = |x: f32| fogged_payload(&p, Vec3::new(-x, 0., 0.), 1800.);
        // Zero-fog radius 630 (0.35f32*1800 rounds to exactly 630). Farthest vertex (10,0,0).
        assert_eq!(0.35f32 * 1800., 630.);
        assert!(fog_free(&p, &at(617.)), "627 against 630 - 2 - 1247/4096");
        // Inside the radius, but not by the margin: the packet keeps its fog.
        assert!(!fog_free(&p, &at(618.)));
        assert!(!fog_free(&p, &at(640.)));
        // The vertices, not their box: the corner (100,100,0) is no vertex and is 636 away.
        let mut r = pass();
        r.mesh.vertices[1].position = Vec3::new(100., 0., 0.);
        r.mesh.vertices[2].position = Vec3::new(0., 100., 0.);
        let eye = Vec3::new(-350., -350., 0.);
        assert!(Vec3::new(100., 100., 0.).distance(eye) > 630.);
        assert!(fog_free(&r, &fogged_payload(&r, eye, 1800.)), "farthest vertex 570.1");
        // Any vertex beyond the limit decides, wherever it is in the list.
        let mut q = pass();
        q.mesh.vertices[2].position = Vec3::new(700., 0., 0.);
        assert!(!fog_free(&q, &fogged_payload(&q, Vec3::ZERO, 1800.)));
        q.mesh.vertices[2].position = Vec3::new(600., 0., 0.);
        assert!(fog_free(&q, &fogged_payload(&q, Vec3::ZERO, 1800.)));
        for (name, adjust) in [
            ("liquid fog", (|u: &mut [f32; UNIFORM_FLOATS]| u[LIQUID_W] = 1.) as fn(&mut [f32; UNIFORM_FLOATS])),
            ("first fog volume", |u| u[FOG_A_W] = 1.),
            ("second fog volume", |u| u[FOG_B_W] = 1.),
            ("moved model", |u| u[12] = 1.),
            ("non-finite eye", |u| u[EYE] = f32::NAN),
            ("non-finite fog colour", |u| u[DISTANCE] = f32::INFINITY),
            ("non-finite gain", |u| u[GAIN] = f32::INFINITY),
            ("tiny fog distance", |u| u[DISTANCE + 3] = 0.5),
            ("source-colour fog", |u| u[FOG_MODE] = 4.),
        ] {
            let mut u = at(600.);
            assert!(fog_free(&p, &u), "control for {name}");
            adjust(&mut u);
            assert!(!fog_free(&p, &u), "{name}");
        }
        let mut q = pass();
        q.mesh.vertices[1].position.y = f32::NAN;
        assert!(!fog_free(&q, &fogged_payload(&q, Vec3::new(-100., 0., 0.), 1800.)), "non-finite vertex");
        q.mesh.vertices[1].position.y = 9000.;
        assert!(!fog_free(&q, &fogged_payload(&q, Vec3::new(-100., 0., 0.), 1800.)), "unbounded vertex");
        q.mesh.vertices.clear();
        assert!(!fog_free(&q, &fogged_payload(&q, Vec3::new(-100., 0., 0.), 1800.)), "no vertex");
        // `fogged` changes nothing when no term is on, or is not called: no bound is needed.
        let far = |w: f32| fogged_payload(&p, Vec3::new(-5000., 0., 0.), w);
        assert!(!fog_free(&p, &far(1800.)));
        assert!(fog_free(&p, &far(0.)));
        assert!(fog_free(&p, &far(f32::NAN)), "NaN fails the shader's guard too");
        let mut u = far(1800.);
        u[LIQUID_W] = 1.;
        u[FOG_MODE] = -1.;
        assert!(fog_free(&p, &u), "no fog call at all");
        u[FOG_MODE] = f32::NAN;
        assert!(fog_free(&p, &u), "NaN fails the call's guard");
    }
    #[test]
    fn the_key_is_off_by_default_and_strict() {
        let c = crate::render::vita_renderer::Config::default();
        assert!(!c.actor_lean_shader && !c.actor_light_cull && !c.actor_lean_lights);
        for key in ["actor_light_cull", "actor_lean_lights"] {
            assert!(serde_json::from_value::<crate::render::vita_renderer::Config>(
                serde_json::json!({(key): 1})).is_err());
        }
        assert!(serde_json::from_value::<crate::render::vita_renderer::Config>(serde_json::json!({"actor_lean_shader": 1})).is_err());
        assert!(serde_json::from_value::<crate::render::vita_renderer::Config>(serde_json::json!({"actor_lean_shader": true})).unwrap().actor_lean_shader);
    }
}
