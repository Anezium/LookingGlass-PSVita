//! Direct reader for the integer PCM WAVs in the game data (`wav_direct_decode`).
//!
//! `parse` accepts only files whose header hound 3.5.1 accepts with the same layout, holding
//! 8-bit or 16-bit integer samples in a complete data chunk. It walks the chunks exactly as
//! hound does (no pad byte after odd chunks, four bytes read from `fact`). Anything else
//! returns `None`, and the caller keeps the rodio/hound decoder, so a file is either read
//! here with the same samples or not read here at all.

pub(super) struct Wav<'a> {
    pub channels: u16,
    pub rate: u32,
    wide: bool,
    data: &'a [u8],
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// `(channels, rate, wide)` from a PCM fmt chunk body, or `None` where hound would refuse
/// it or would read samples other than 1-byte 8-bit or 2-byte 16-bit integers.
fn format(body: &[u8]) -> Option<(u16, u32, bool)> {
    if !matches!(body.len(), 16 | 18 | 40) || u16_at(body, 0) != 1 {
        return None;
    }
    let (channels, rate, byte_rate) = (u16_at(body, 2), u32_at(body, 4), u32_at(body, 8));
    let (block_align, bits) = (u16_at(body, 12), u16_at(body, 14));
    if channels == 0 || Some(byte_rate) != (block_align as u32).checked_mul(rate) {
        return None;
    }
    match (block_align / channels, bits) {
        (1, 8) => Some((channels, rate, false)),
        (2, 16) => Some((channels, rate, true)),
        _ => None,
    }
}

pub(super) fn parse(bytes: &[u8]) -> Option<Wav<'_>> {
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut at, mut fmt) = (12usize, None);
    loop {
        let header = bytes.get(at..at.checked_add(8)?)?;
        let len = u32_at(header, 4) as usize;
        at += 8;
        let body = at.checked_add(len)?;
        match &header[..4] {
            b"fmt " => {
                fmt = Some(format(bytes.get(at..body)?)?);
                at = body;
            }
            b"fact" if len == 4 => at = body,
            b"fact" => return None,
            b"data" => {
                let (channels, rate, wide) = fmt?;
                let width = 1 + wide as usize;
                if len % width != 0 || (len / width) % channels as usize != 0 {
                    return None;
                }
                return Some(Wav { channels, rate, wide, data: bytes.get(at..body)? });
            }
            _ => at = body,
        }
    }
}

impl Wav<'_> {
    /// Interleaved sample count, hound's `len()`.
    pub fn len(&self) -> usize {
        self.data.len() >> self.wide as usize
    }

    /// The interleaved samples exactly as rodio's WAV source yields them.
    pub fn samples(&self) -> impl Iterator<Item = i16> + '_ {
        let width = 1 + self.wide as usize;
        self.data.chunks_exact(width).map(move |s| {
            if width == 2 {
                i16::from_le_bytes([s[0], s[1]])
            } else {
                (s[0] as i16 - 128) * 256
            }
        })
    }

    /// Mono `f32` samples equal to rodio's `convert_samples::<f32>()` followed by the
    /// per-frame channel mean of `decode_clip`.
    pub fn mono(&self) -> Vec<f32> {
        let channels = self.channels as usize;
        let mut out = Vec::with_capacity(self.len() / channels);
        let mut samples = self.samples();
        while let Some(first) = samples.next() {
            let mut sum = first as f32 / 32768.;
            for _ in 1..channels {
                sum += samples.next().unwrap_or(0) as f32 / 32768.;
            }
            out.push(sum / channels as f32);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::{decode_clip_with, Clip};
    use super::parse;
    use anyhow::Result;
    use std::collections::BTreeMap;

    fn chunk(id: &[u8; 4], body: &[u8], declared: Option<u32>) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend(declared.unwrap_or(body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }

    fn fmt(tag: u16, channels: u16, rate: u32, align: u16, bits: u16, tail: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend(tag.to_le_bytes());
        body.extend(channels.to_le_bytes());
        body.extend(rate.to_le_bytes());
        body.extend((rate * align as u32).to_le_bytes());
        body.extend(align.to_le_bytes());
        body.extend(bits.to_le_bytes());
        body.extend(tail);
        chunk(b"fmt ", &body, None)
    }

    fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
        let body = chunks.concat();
        let mut out = b"RIFF".to_vec();
        out.extend((body.len() as u32 + 4).to_le_bytes());
        out.extend(b"WAVE");
        out.extend(body);
        out
    }

    fn clip_bits(clip: &Result<Clip>) -> std::result::Result<(u32, Vec<u32>), String> {
        match clip {
            Ok(c) => Ok((c.rate, c.samples.iter().map(|s| s.to_bits()).collect())),
            Err(e) => Err(format!("{e:#}")),
        }
    }

    /// Both decoders of one file, compared bit for bit. Returns whether the direct reader took it.
    fn assert_same(bytes: &[u8], name: &str) -> bool {
        let old = clip_bits(&decode_clip_with(bytes.to_vec(), false));
        let new = clip_bits(&decode_clip_with(bytes.to_vec(), true));
        assert!(old == new, "decode_clip differs: {name}");
        #[cfg(feature = "vita_renderer")]
        assert!(
            super::super::voice::decoded(bytes.to_vec(), false)
                == super::super::voice::decoded(bytes.to_vec(), true),
            "voice decode differs: {name}"
        );
        parse(bytes).is_some()
    }

    #[test]
    fn synthetic_layouts_match_the_rodio_decoder() {
        let pcm16: Vec<u8> = (0..1200u32).flat_map(|i| ((i * 7919) as u16).to_le_bytes()).collect();
        let pcm8: Vec<u8> = (0..1200u32).map(|i| (i * 31) as u8).collect();
        let ext = |sub: u8| [22, 0, 16, 0, 3, 0, 0, 0, sub, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
        let list = chunk(b"LIST", b"odd", None);
        let fact = chunk(b"fact", &1200u32.to_le_bytes(), None);
        let cases: Vec<(&str, Vec<u8>, bool)> = vec![
            ("mono16", riff(&[fmt(1, 1, 22050, 2, 16, &[]), chunk(b"data", &pcm16, None)]), true),
            ("stereo16", riff(&[fmt(1, 2, 44100, 4, 16, &[]), chunk(b"data", &pcm16, None)]), true),
            ("mono8", riff(&[fmt(1, 1, 11025, 1, 8, &[]), chunk(b"data", &pcm8, None)]), true),
            ("stereo8", riff(&[fmt(1, 2, 22050, 2, 8, &[]), chunk(b"data", &pcm8, None)]), true),
            ("fmt18", riff(&[fmt(1, 1, 22050, 2, 16, &[0, 0]), chunk(b"data", &pcm16, None)]), true),
            ("fmt40", riff(&[fmt(1, 1, 22050, 2, 16, &[0; 24]), chunk(b"data", &pcm16, None)]), true),
            ("odd list and fact", riff(&[list.clone(), fmt(1, 1, 22050, 2, 16, &[]), fact,
                chunk(b"data", &pcm16, None)]), true),
            ("trailing chunk", riff(&[fmt(1, 1, 22050, 2, 16, &[]), chunk(b"data", &pcm16, None), list]), true),
            ("fact of 8", riff(&[fmt(1, 1, 22050, 2, 16, &[]), chunk(b"fact", &[0; 8], None),
                chunk(b"data", &pcm16, None)]), false),
            ("24-bit", riff(&[fmt(1, 1, 22050, 3, 24, &[]), chunk(b"data", &pcm16[..2400], None)]), false),
            ("float", riff(&[fmt(3, 1, 22050, 4, 32, &[]), chunk(b"data", &[0; 400], None)]), false),
            ("extensible", riff(&[fmt(0xfffe, 1, 22050, 2, 16, &ext(1)), chunk(b"data", &pcm16, None)]), false),
            ("8-bit in 2 bytes", riff(&[fmt(1, 1, 22050, 2, 8, &[]), chunk(b"data", &pcm16, None)]), false),
            ("truncated data", riff(&[fmt(1, 1, 22050, 2, 16, &[]), chunk(b"data", &pcm16, Some(4000))]), false),
            ("odd data", riff(&[fmt(1, 1, 22050, 2, 16, &[]), chunk(b"data", &pcm16[..2399], None)]), false),
            ("frame split", riff(&[fmt(1, 2, 22050, 4, 16, &[]), chunk(b"data", &pcm16[..2398], None)]), false),
            ("empty", riff(&[fmt(1, 1, 22050, 2, 16, &[]), chunk(b"data", &[], None)]), true),
            ("three channels", riff(&[fmt(1, 3, 22050, 6, 16, &[]), chunk(b"data", &pcm16, None)]), true),
            ("low rate", riff(&[fmt(1, 1, 4000, 2, 16, &[]), chunk(b"data", &pcm16, None)]), true),
            ("no fmt", riff(&[chunk(b"data", &pcm16, None)]), false),
            ("not riff", pcm16.clone(), false),
        ];
        for (name, bytes, parsed) in &cases {
            assert_eq!(assert_same(bytes, name), *parsed, "{name}");
        }
    }

    /// Every WAV in the archives at LOOKING_GLASS_TEST_DATA decodes to the same samples on
    /// both paths. Skips when the variable is unset. Prints one line per distinct format.
    #[test]
    fn game_wavs_match_the_rodio_decoder() -> Result<()> {
        let Some(root) = std::env::var_os("LOOKING_GLASS_TEST_DATA") else {
            println!("WAV_DIRECT skipped: LOOKING_GLASS_TEST_DATA unset");
            return Ok(());
        };
        let mut assets = crate::assets::Assets::open(std::path::Path::new(&root))?;
        let names: Vec<String> = assets.names().filter(|n| n.to_ascii_lowercase().ends_with(".wav"))
            .map(str::to_owned).collect();
        let mut formats = BTreeMap::<String, (usize, usize)>::new();
        for name in &names {
            let bytes = assets.read(name)?;
            let direct = assert_same(&bytes, name);
            let key = match hound::WavReader::new(std::io::Cursor::new(&bytes)) {
                Ok(r) => {
                    let s = r.spec();
                    format!("{:?}{} ch={} rate={}", s.sample_format, s.bits_per_sample, s.channels, s.sample_rate)
                }
                Err(e) => format!("hound-error {e}"),
            };
            let row = formats.entry(key).or_default();
            row.0 += 1;
            row.1 += direct as usize;
        }
        for (key, (files, direct)) in &formats {
            println!("WAV_FORMAT {key} files={files} direct={direct}");
        }
        println!("WAV_DIRECT files={} formats={} direct={}", names.len(), formats.len(),
            formats.values().map(|v| v.1).sum::<usize>());
        anyhow::ensure!(!names.is_empty(), "no WAV in the archives");
        Ok(())
    }

    /// Host decode time per output sample, rodio path vs direct path, minimum of five runs per
    /// file over every archive WAV the direct reader takes.
    #[test]
    #[ignore = "requires LOOKING_GLASS_TEST_DATA and an idle timing host"]
    fn wav_direct_decode_timing() -> Result<()> {
        use std::time::Instant;
        let root = std::env::var_os("LOOKING_GLASS_TEST_DATA").ok_or_else(|| anyhow::anyhow!("no data"))?;
        let mut assets = crate::assets::Assets::open(std::path::Path::new(&root))?;
        let names: Vec<String> = assets.names().filter(|n| n.to_ascii_lowercase().ends_with(".wav"))
            .map(str::to_owned).collect();
        let (mut samples, mut old_s, mut new_s, mut files) = (0usize, 0f64, 0f64, 0usize);
        for name in &names {
            let bytes = assets.read(name)?;
            if parse(&bytes).is_none() {
                continue;
            }
            let Ok(clip) = decode_clip_with(bytes.clone(), false) else { continue };
            let (mut old, mut new) = (f64::MAX, f64::MAX);
            for _ in 0..5 {
                for (direct, best) in [(false, &mut old), (true, &mut new)] {
                    let input = bytes.clone();
                    let t = Instant::now();
                    std::hint::black_box(decode_clip_with(input, direct)?);
                    *best = best.min(t.elapsed().as_secs_f64());
                }
            }
            if ["cheer_loop.wav", "chain_loop.wav", "cards_loop1.wav", "powerup.wav", "door_slam.wav"]
                .iter().any(|n| name.ends_with(n)) {
                println!("WAV_TIMING_CLIP {name} samples={} rodio_us={:.1} direct_us={:.1} ratio={:.2}",
                    clip.samples.len(), old * 1e6, new * 1e6, old / new);
            }
            samples += clip.samples.len();
            old_s += old;
            new_s += new;
            files += 1;
        }
        println!("WAV_TIMING files={files} samples={samples} rodio_s={old_s:.4} direct_s={new_s:.4} \
                  rodio_ns_per_sample={:.2} direct_ns_per_sample={:.2} ratio={:.2}",
            old_s * 1e9 / samples as f64, new_s * 1e9 / samples as f64, old_s / new_s);
        Ok(())
    }
}
