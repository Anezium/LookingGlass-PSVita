//! Host-only audio exactness trace.
//!
//! Enabled by `LOOKING_GLASS_AUDIO_PROOF=1` in a `sim_maps_probe` host build and
//! never compiled on Vita. `AUDIO_DECODE` records the decoded mono samples of every
//! clip that enters the cache; `AUDIO_MIX` records the actual mixed sample stream the
//! silent mixer produced for one frame. Two runs that agree on every frame's mixed
//! samples and on each path's decoded samples have identical audio output.
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::sync::OnceLock;

struct Mix {
    samples: u64,
    hash: Sha256,
}
thread_local! {
    // Whole-session count of stereo interleaved samples actually pulled by the host mixer.
    static MIX: RefCell<(u64, Option<Mix>)> = const { RefCell::new((0, None)) };
}

pub(super) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LOOKING_GLASS_AUDIO_PROOF").is_some())
}

/// One mixed sample from the deterministic host mixer pump.
pub(super) fn mix_sample(value: f32) {
    if !enabled() {
        return;
    }
    MIX.with(|mix| {
        let mut mix = mix.borrow_mut();
        mix.0 += 1;
        let entry = mix.1.get_or_insert_with(|| Mix {
            samples: 0,
            hash: Sha256::new(),
        });
        entry.hash.update(value.to_bits().to_le_bytes());
        entry.samples += 1;
    });
}

/// Emit the samples accumulated for the current preparation pass.
pub(super) fn mix_end() {
    if !enabled() {
        return;
    }
    let frame = crate::sim_maps::probe::frame();
    MIX.with(|mix| {
        if let Some(entry) = mix.borrow_mut().1.take() {
            println!(
                "AUDIO_MIX frame={frame} samples={} sha256={:x}",
                entry.samples,
                entry.hash.finalize()
            );
        }
    });
}

/// One decoded clip entering the level cache. `origin` is preload, prefetch,
/// runtime or loop; the path and samples are what playback consumes.
pub(super) fn decode(origin: &str, path: &str, samples: &[f32]) {
    if !enabled() {
        return;
    }
    println!(
        "AUDIO_DECODE frame={} origin={origin} path={path} samples={} sha256={:x}",
        crate::sim_maps::probe::frame(),
        samples.len(),
        pcm_hash(samples)
    );
}

fn pcm_hash(samples: &[f32]) -> impl std::fmt::LowerHex {
    let mut hash = Sha256::new();
    for sample in samples {
        hash.update(sample.to_bits().to_le_bytes());
    }
    hash.finalize()
}

/// Playback proof for measured first-use effects. Use the source passed to
/// append_effect, before resampling, and the deterministic mixer's output clock.
pub(super) fn named_enqueue(
    path: &str,
    pcm: &super::Pcm,
    origin: Option<macroquad::prelude::Vec3>,
    gain: f32,
    loop_id: Option<usize>,
) {
    if !enabled() || !super::ALL_NAMED_CUES.iter().any(|paths| paths.contains(&path))
    {
        return;
    }
    let output_sample = MIX.with(|mix| mix.borrow().0);
    println!("AUDIO_NAMED_ENQUEUE {}", serde_json::json!({
        "frame": crate::sim_maps::probe::frame(),
        "output_sample": output_sample,
        "path": path,
        "origin_bits": origin.map(|p| p.to_array().map(f32::to_bits)),
        "gain_bits": gain.to_bits(),
        "loop_id": loop_id,
        "index": pcm.index,
        "rate": pcm.clip.rate,
        "samples": pcm.clip.samples.len(),
        "sha256": format!("{:x}", pcm_hash(&pcm.clip.samples)),
    }));
}

/// Playback proof for resident Alice player cues across one-shot and loop paths.
pub(super) fn player_enqueue(
    path: &str,
    pcm: &super::Pcm,
    origin: Option<macroquad::prelude::Vec3>,
    gain: f32,
    loop_id: Option<usize>,
) {
    if !enabled() || !super::PLAYER_CUES.contains(&path) {
        return;
    }
    let output_sample = MIX.with(|mix| mix.borrow().0);
    println!("AUDIO_PLAYER_ENQUEUE {}", serde_json::json!({
        "frame": crate::sim_maps::probe::frame(),
        "output_sample": output_sample,
        "path": path,
        "origin_bits": origin.map(|p| p.to_array().map(f32::to_bits)),
        "gain_bits": gain.to_bits(),
        "loop_id": loop_id,
        "index": pcm.index,
        "rate": pcm.clip.rate,
        "samples": pcm.clip.samples.len(),
        "sha256": format!("{:x}", pcm_hash(&pcm.clip.samples)),
    }));
}
