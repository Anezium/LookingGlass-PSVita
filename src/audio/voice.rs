//! Bounded PCM for registered story lines, prepared before gameplay.
use rodio::Source;
use std::{sync::Arc, time::Duration};

#[derive(Clone)]
pub(super) struct Pcm {
    samples: Arc<Vec<i16>>,
    index: usize,
    channels: u16,
    rate: u32,
    duration: Duration,
}

impl Iterator for Pcm {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        let sample = *self.samples.get(self.index)?;
        self.index += 1;
        Some(sample)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.samples.len() - self.index;
        (left, Some(left))
    }
}

impl Source for Pcm {
    // Match Rodio's WAV source so skip_duration uses the same interleaved-sample offset.
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        Some(self.duration)
    }
}

#[cfg(feature = "vita_renderer")]
use std::{collections::BTreeMap, io::Cursor};

#[cfg(feature = "vita_renderer")]
const PCM_BYTES: usize = 4 * 1024 * 1024;
#[cfg(feature = "vita_renderer")]
const WAV_BYTES: u64 = 1024 * 1024;

#[cfg(feature = "vita_renderer")]
fn decode(bytes: Vec<u8>, remaining: usize) -> Option<Pcm> {
    let header = hound::WavReader::new(Cursor::new(bytes.as_slice())).ok()?;
    let spec = header.spec();
    if spec.channels == 0
        || spec.sample_rate == 0
        || !matches!(
            (spec.sample_format, spec.bits_per_sample),
            (hound::SampleFormat::Int, 8 | 16 | 24 | 32) | (hound::SampleFormat::Float, 32)
        )
    {
        return None;
    }
    let count = header.len() as usize;
    if count == 0 || count > remaining / size_of::<i16>() {
        return None;
    }
    let decoder = rodio::Decoder::new_wav(Cursor::new(bytes)).ok()?;
    let duration = decoder.total_duration()?;
    let mut samples = Vec::with_capacity(count);
    samples.extend(decoder.take(count));
    Some(Pcm {
        samples: Arc::new(samples),
        index: 0,
        channels: spec.channels,
        rate: spec.sample_rate,
        duration,
    })
}

#[cfg(feature = "vita_renderer")]
pub(super) fn prefetch(
    assets: &mut crate::assets::Assets,
    story: &crate::story::Story,
    map: &str,
) -> BTreeMap<String, Pcm> {
    let mut cache = BTreeMap::new();
    let mut bytes = 0;
    let started = std::time::Instant::now();
    for path in story.voice_paths() {
        if bytes == PCM_BYTES {
            break;
        }
        if cache.contains_key(path) {
            continue;
        }
        if let Ok(wav) = assets.read_limited(path, WAV_BYTES) {
            if let Some(clip) = decode(wav, PCM_BYTES - bytes) {
                bytes += clip.samples.capacity() * size_of::<i16>();
                cache.insert(path.to_owned(), clip);
            }
        }
    }
    eprintln!(
        "[lg/story-voice-prefetch] map={map} clips={} bytes={bytes} cap_bytes={PCM_BYTES} ms={:.3}",
        cache.len(),
        started.elapsed().as_secs_f64() * 1000.
    );
    cache
}

#[cfg(all(test, feature = "vita_renderer"))]
mod tests {
    use super::*;

    #[test]
    fn prefetched_wav_matches_decoder_samples_at_saved_offsets() {
        for channels in [1, 2] {
            let mut bytes = Cursor::new(Vec::new());
            let spec = hound::WavSpec {
                channels,
                sample_rate: 22050,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::new(&mut bytes, spec).unwrap();
            for i in 0..1000_i16 {
                writer.write_sample(i.wrapping_mul(159)).unwrap();
            }
            writer.finalize().unwrap();
            let bytes = bytes.into_inner();
            assert!(decode(bytes.clone(), 1999).is_none());
            let prefetched = decode(bytes.clone(), 2000).unwrap();
            for seconds in [0., 0.000031, 0.01337, 0.25] {
                let offset = Duration::from_secs_f32(seconds);
                let baseline = rodio::Decoder::new(Cursor::new(bytes.clone())).unwrap();
                assert_eq!(baseline.total_duration(), prefetched.total_duration());
                assert!(baseline
                    .skip_duration(offset)
                    .eq(prefetched.clone().skip_duration(offset)));
            }
        }
    }
}
