#[cfg(not(target_os = "vita"))]
use super::UPSCALE_VERTEX;
use super::{Material, MaterialParams, PipelineParams, ShaderSource, UniformDesc, UniformType};
use std::{cell::OnceCell, mem::ManuallyDrop, panic};

thread_local! {
    // The renderer and copy orientation are fixed for the process. Retain a
    // successful material across target resizes and remember a refusal too.
    // As with the scaled target, GL may be gone by the time TLS is destroyed.
    static MATERIAL: OnceCell<Option<ManuallyDrop<Material>>> = const { OnceCell::new() };
}

pub(super) fn prepare(invert: bool) {
    prepare_with(|| {
        #[cfg(target_os = "vita")]
        let _binding = crate::shader_cache::EagerGlsl::new();
        crate::frame_profile::load_material(
            ShaderSource::Glsl {
                vertex: VERTEX,
                fragment: if invert { FRAGMENT_INVERT_V } else { FRAGMENT },
            },
            MaterialParams {
                pipeline_params: PipelineParams {
                    color_blend: None,
                    alpha_blend: None,
                    ..Default::default()
                },
                uniforms: vec![UniformDesc::new("InvGamma", UniformType::Float1)],
                ..Default::default()
            },
        )
    });
}

fn prepare_with(load: impl FnOnce() -> Result<Material, macroquad::Error> + panic::UnwindSafe) {
    MATERIAL.with(|slot| {
        slot.get_or_init(|| {
            // Miniquad can assert on a failed compile/link with an empty
            // driver log. Keep that failure local to this optional shader.
            match panic::catch_unwind(load) {
                Ok(Ok(material)) => return Some(ManuallyDrop::new(material)),
                Ok(Err(error)) => {
                    eprintln!("[lg/gamma] warning: shader refused ({error}); using neutral upscale")
                }
                Err(_) => {
                    eprintln!("[lg/gamma] warning: shader loader panicked; using neutral upscale")
                }
            }
            None
        });
    });
}

pub(super) fn with_material<R>(value: f32, select: impl FnOnce(Option<&Material>) -> R) -> R {
    if value == 1. {
        return select(None);
    }
    MATERIAL.with(|slot| {
        let material = slot.get().and_then(|loaded| loaded.as_deref());
        if let Some(material) = material {
            material.set_uniform("InvGamma", value.recip());
        }
        select(material)
    })
}

macro_rules! fragment {
    ($coord:literal) => {
        concat!(
            "#version 100\n",
            r#"precision mediump float;
varying mediump vec2 uv;
uniform sampler2D Texture;
uniform mediump float InvGamma;
float correct(float channel) {
    return channel > 0.0 ? pow(channel, InvGamma) : 0.0;
}
void main() {
    vec3 rgb = texture2D(Texture, "#,
            $coord,
            r#").rgb;
    gl_FragColor = vec4(correct(rgb.r), correct(rgb.g), correct(rgb.b), 1.0);
}"#,
        )
    };
}

// Keep the Vita vertex-stage cache key distinct from the neutral shader,
// whose varyings are bound by the postponed pair translator.
#[cfg(target_os = "vita")]
const VERTEX: &str = super::upscale_vertex!("\n// Gamma upscale\n");
#[cfg(not(target_os = "vita"))]
const VERTEX: &str = UPSCALE_VERTEX;

const FRAGMENT: &str = fragment!("uv");
const FRAGMENT_INVERT_V: &str = fragment!("vec2(uv.x,1.-uv.y)");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_shader_refusal_latches_neutral_fallback() {
        prepare_with(|| {
            Err(macroquad::miniquad::ShaderError::LinkError("forced refusal".into()).into())
        });
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        prepare_with(|| {
            attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Err(macroquad::Error::UnknownError("unexpected retry"))
        });
        assert_eq!(attempts.into_inner(), 0);
        MATERIAL.with(|slot| assert!(matches!(slot.get(), Some(None))));
        with_material(1.2, |material| assert!(material.is_none()));
        with_material(1.0, |material| assert!(material.is_none()));
    }

    #[test]
    fn gamma_empty_driver_log_panic_latches_neutral_fallback() {
        prepare_with(|| panic!("simulated empty driver log"));
        MATERIAL.with(|slot| assert!(matches!(slot.get(), Some(None))));
        with_material(0.8, |material| assert!(material.is_none()));
    }
}
