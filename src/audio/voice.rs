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
const WAV_BYTES: usize = 1024 * 1024;
/// With `voice_wav_cap_1_25`: jlair2's `jabberwock/vo/jbwk004.wav` is 1,060,974 B.
#[cfg(feature = "vita_renderer")]
const WAV_BYTES_1_25: usize = 1280 * 1024;

#[cfg(feature = "vita_renderer")]
fn decode(bytes: Vec<u8>, reserve: impl FnOnce(usize) -> bool) -> Option<Pcm> {
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
    let size = count.checked_mul(size_of::<i16>())?;
    if size == 0 || size > PCM_BYTES {
        return None;
    }
    let decoder = rodio::Decoder::new_wav(Cursor::new(bytes)).ok()?;
    let duration = decoder.total_duration()?;
    if !reserve(size) {
        return None;
    }
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
pub(crate) struct Cache {
    clips: BTreeMap<String, Pcm>,
    replies: Vec<String>,
    bytes: usize,
    decode_time: Duration,
    first_lines: (usize, usize),
    wav_cap: usize,
}

#[cfg(feature = "vita_renderer")]
impl Default for Cache {
    fn default() -> Self {
        Self::with_wav_cap(WAV_BYTES)
    }
}

#[cfg(feature = "vita_renderer")]
impl Cache {
    fn with_wav_cap(wav_cap: usize) -> Self {
        Self {
            clips: BTreeMap::new(),
            replies: Vec::new(),
            bytes: 0,
            decode_time: Duration::ZERO,
            first_lines: (0, 0),
            wav_cap,
        }
    }

    /// The cache for this run's config: `voice_wav_cap_1_25` raises the largest admitted WAV.
    pub(crate) fn configured() -> Self {
        Self::for_cap(crate::render::vita_renderer::config().voice_wav_cap_1_25)
    }

    pub(crate) fn for_cap(raised: bool) -> Self {
        Self::with_wav_cap(if raised { WAV_BYTES_1_25 } else { WAV_BYTES })
    }

    /// PCM bytes, first lines cached of total, and the cached paths.
    #[cfg(test)]
    pub(crate) fn census(&self) -> (usize, (usize, usize), Vec<&str>) {
        (self.bytes, self.first_lines, self.clips.keys().map(String::as_str).collect())
    }

    pub(crate) fn add(&mut self, path: &str, wav: Vec<u8>, first_line: bool) {
        if self.clips.contains_key(path) {
            if first_line {
                self.replies.retain(|reply| reply != path);
            }
            return;
        }
        if wav.len() > self.wav_cap || (!first_line && self.bytes == PCM_BYTES) {
            return;
        }
        let started = std::time::Instant::now();
        if let Some(clip) = decode(wav, |size| self.reserve(size, first_line)) {
            self.bytes += clip.samples.capacity() * size_of::<i16>();
            self.clips.insert(path.to_owned(), clip);
            if !first_line {
                self.replies.push(path.to_owned());
            }
        }
        self.decode_time += started.elapsed();
    }

    fn reserve(&mut self, size: usize, first_line: bool) -> bool {
        let mut remaining = PCM_BYTES - self.bytes;
        if size <= remaining {
            return true;
        }
        if first_line {
            // Establish that the line fits before removing any useful replies.
            for (index, path) in self.replies.iter().enumerate().rev() {
                remaining += self.clips[path].samples.capacity() * size_of::<i16>();
                if size <= remaining {
                    for reply in self.replies.drain(index..) {
                        let clip = self.clips.remove(&reply).unwrap();
                        self.bytes -= clip.samples.capacity() * size_of::<i16>();
                    }
                    return true;
                }
            }
        }
        false
    }

    pub(crate) fn retain<'a>(
        &mut self,
        registered: impl Fn(&str) -> bool,
        first_lines: impl Iterator<Item = &'a str>,
    ) {
        self.clips.retain(|path, clip| {
            if registered(path) {
                return true;
            }
            self.bytes -= clip.samples.capacity() * size_of::<i16>();
            false
        });
        self.replies.retain(|path| registered(path));
        self.first_lines = first_lines.fold((0, 0), |(cached, total), path| {
            (
                cached + usize::from(self.clips.contains_key(path)),
                total + 1,
            )
        });
    }

    pub(super) fn install(self, map: &str) -> BTreeMap<String, Pcm> {
        let (cached, total) = self.first_lines;
        eprintln!(
            "[lg/story-voice-prefetch] map={map} clips={} bytes={} cap_bytes={PCM_BYTES} ms={:.3} first_lines={cached}/{total}",
            self.clips.len(),
            self.bytes,
            self.decode_time.as_secs_f64() * 1000.
        );
        self.clips
    }
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
            assert!(decode(bytes.clone(), |size| size <= 1999).is_none());
            let prefetched = decode(bytes.clone(), |size| size == 2000).unwrap();
            for seconds in [0., 0.000031, 0.01337, 0.25] {
                let offset = Duration::from_secs_f32(seconds);
                let baseline = rodio::Decoder::new(Cursor::new(bytes.clone())).unwrap();
                assert_eq!(baseline.total_duration(), prefetched.total_duration());
                assert!(
                    baseline
                        .skip_duration(offset)
                        .eq(prefetched.clone().skip_duration(offset))
                );
            }
        }
    }

    #[test]
    fn raised_cap_admits_a_wav_between_one_and_one_and_a_quarter_mib() {
        let mut bytes = Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(
            &mut bytes,
            hound::WavSpec {
                channels: 1,
                sample_rate: 22050,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        // jbwk004.wav is 1,060,974 B: 44 B of header and 530,465 samples.
        for i in 0..530_465_u32 {
            writer.write_sample((i % 251) as i16).unwrap();
        }
        writer.finalize().unwrap();
        let bytes = bytes.into_inner();
        assert_eq!(bytes.len(), 1_060_974);
        for (mut cache, admitted) in [(Cache::default(), false), (Cache::for_cap(false), false), (Cache::for_cap(true), true)] {
            cache.add("jbwk004", bytes.clone(), true);
            assert_eq!(cache.clips.contains_key("jbwk004"), admitted);
            assert_eq!(cache.bytes, if admitted { 1_060_930 } else { 0 });
        }
        let mut over = bytes.clone();
        over.resize(WAV_BYTES_1_25 + 1, 0);
        let mut cache = Cache::for_cap(true);
        cache.add("over", over, true);
        assert!(cache.clips.is_empty(), "the raised cap still refuses a WAV over 1.25 MiB");
    }

    #[test]
    fn first_lines_evict_newest_replies_from_a_saturated_map() {
        // Eight equal clips fit exactly after expanding 8-bit WAV to i16 PCM.
        let mut bytes = Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(
            &mut bytes,
            hound::WavSpec {
                channels: 1,
                sample_rate: 22050,
                bits_per_sample: 8,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..PCM_BYTES / 16 {
            writer.write_sample(0_i8).unwrap();
        }
        writer.finalize().unwrap();
        let bytes = bytes.into_inner();
        let mut cache = Cache::default();
        cache.add("opening", bytes.clone(), true);
        for i in 1..=7 {
            cache.add(&format!("reply-{i}"), bytes.clone(), false);
        }
        assert_eq!(cache.bytes, PCM_BYTES);

        cache.add("reply-7", bytes.clone(), true);
        cache.add("late-opening", bytes.clone(), true);
        assert!(!cache.clips.contains_key("reply-6"));
        assert!(cache.clips.contains_key("reply-5"));
        for i in 1..=5 {
            cache.add(&format!("reply-{i}"), bytes.clone(), true);
        }
        cache.add("extra-opening", bytes, true);
        assert_eq!(cache.bytes, PCM_BYTES);
        assert_eq!(cache.clips.len(), 8);
        assert!(!cache.clips.contains_key("extra-opening"));
        cache.retain(
            |_| true,
            ["opening", "reply-7", "late-opening", "extra-opening"].into_iter(),
        );
        assert_eq!(cache.first_lines, (3, 4));
    }
}
