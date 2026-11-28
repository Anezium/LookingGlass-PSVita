//! Music file bytes shared between a music decoder and the game thread
//! (`audio_release_early`). At the release point the game keeps only the bytes
//! the paused track can still read, so the whole file does not stay live through
//! the next level build. The decoder, its position and its clock are untouched:
//! the reader returns the same bytes as a `Cursor` over the whole file while the
//! reads stay inside the kept spans, and a rollback puts the whole file back.
use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
};

/// Bytes kept after the read position, and at the file start when the kept span
/// reaches the end of a looping track. The music tracks are 128 kbit/s, so 256 KiB
/// is 16.4 s of playback against an outgoing fade of at most 5 s plus the 0.74 s
/// decode-ahead ring.
pub(super) const WINDOW: usize = 256 * 1024;

/// Reads that fell outside the kept spans. Each one ends the track early, so a
/// nonzero count means the window was too small.
pub(super) static MISSES: AtomicU32 = AtomicU32::new(0);

struct Bytes {
    len: u64,
    pos: u64,
    misses: u32,
    /// Sorted, disjoint `(start, bytes)` spans of the file.
    spans: Vec<(u64, Vec<u8>)>,
}
impl Bytes {
    fn span_at(&self, pos: u64) -> Option<&[u8]> {
        self.spans.iter().find_map(|(start, bytes)| {
            (pos >= *start && pos < start + bytes.len() as u64).then(|| &bytes[(pos - start) as usize..])
        })
    }
    /// The bytes of `[start, end)` still held, up to the first gap.
    fn copy(&self, start: u64, end: u64) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = start;
        while at < end {
            let Some(bytes) = self.span_at(at) else { break };
            let take = bytes.len().min((end - at) as usize);
            if out.capacity() == 0 { out.reserve_exact((end - start) as usize); }
            out.extend_from_slice(&bytes[..take]);
            at += take as u64;
        }
        out
    }
    fn held(&self) -> usize {
        self.spans.iter().map(|(_, b)| b.len()).sum()
    }
}

/// Game-side handle; the decoder owns the matching [`Reader`].
#[derive(Clone)]
pub(super) struct File(Arc<Mutex<Bytes>>);
pub(super) struct Reader(Arc<Mutex<Bytes>>);

impl File {
    pub(super) fn new(bytes: Vec<u8>) -> (Self, Reader) {
        let shared = Arc::new(Mutex::new(Bytes { len: bytes.len() as u64, pos: 0, misses: 0, spans: vec![(0, bytes)] }));
        (Self(shared.clone()), Reader(shared))
    }
    /// A decoder still reads this file.
    pub(super) fn live(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }
    /// Keep `[pos, pos + window)` and, when that reaches the end, `[0, window)` for
    /// the loop restart. Returns the bytes held before and after.
    pub(super) fn trim(&self, window: usize) -> (usize, usize) {
        let mut bytes = self.0.lock().unwrap();
        let (len, pos, window) = (bytes.len, bytes.pos.min(bytes.len), window as u64);
        let before = bytes.held();
        if len <= 2 * window {
            return (before, before);
        }
        let mut spans = Vec::with_capacity(2);
        let end = (pos + window).min(len);
        if end == len || pos < window {
            let head = window.min(len);
            if pos <= head {
                spans.push((0, bytes.copy(0, end.max(head))));
            } else {
                spans.push((0, bytes.copy(0, head)));
                spans.push((pos, bytes.copy(pos, end)));
            }
        } else {
            spans.push((pos, bytes.copy(pos, end)));
        }
        spans.retain(|(_, b)| !b.is_empty());
        let old = std::mem::replace(&mut bytes.spans, spans);
        let after = bytes.held();
        drop(bytes);
        drop(old);
        (before, after)
    }
    /// Put the whole file back (rollback). The bytes must be the same file.
    pub(super) fn fill(&self, whole: Vec<u8>) -> bool {
        let mut bytes = self.0.lock().unwrap();
        if whole.len() as u64 != bytes.len {
            return false;
        }
        let old = std::mem::replace(&mut bytes.spans, vec![(0, whole)]);
        drop(bytes);
        drop(old);
        true
    }
    #[cfg(test)]
    pub(super) fn held(&self) -> usize {
        self.0.lock().unwrap().held()
    }
    #[cfg(test)]
    fn misses(&self) -> u32 {
        self.0.lock().unwrap().misses
    }
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes = self.0.lock().unwrap();
        if bytes.pos >= bytes.len || buf.is_empty() {
            return Ok(0);
        }
        let Some(span) = bytes.span_at(bytes.pos) else {
            MISSES.fetch_add(1, Ordering::Relaxed);
            bytes.misses += 1;
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "music bytes released"));
        };
        let n = span.len().min(buf.len());
        buf[..n].copy_from_slice(&span[..n]);
        bytes.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Reader {
    /// Same results as `Cursor<Vec<u8>>::seek` over the whole file.
    fn seek(&mut self, style: SeekFrom) -> io::Result<u64> {
        let mut bytes = self.0.lock().unwrap();
        let (base, offset) = match style {
            SeekFrom::Start(n) => {
                bytes.pos = n;
                return Ok(n);
            }
            SeekFrom::End(n) => (bytes.len, n),
            SeekFrom::Current(n) => (bytes.pos, n),
        };
        match base.checked_add_signed(offset) {
            Some(n) => {
                bytes.pos = n;
                Ok(n)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid seek to a negative or overflowing position",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::{decoder::LoopedDecoder, Decoder, Source};
    use std::io::Cursor;

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
    }
    /// 16-bit PCM WAV, so the decoder reads through the same Read + Seek path as music.
    fn wav(seconds: f32) -> Vec<u8> {
        let frames = (44100. * seconds) as usize;
        let data = frames * 4;
        let mut out = Vec::with_capacity(44 + data);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&44100u32.to_le_bytes());
        out.extend_from_slice(&(44100u32 * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data as u32).to_le_bytes());
        let mut state = 0x1234_5678u32;
        for _ in 0..frames * 2 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            out.extend_from_slice(&((state >> 16) as i16).to_le_bytes());
        }
        out
    }

    #[test]
    fn reader_matches_cursor_reads_and_seeks() {
        let data = pattern(10_000);
        let (_file, mut reader) = File::new(data.clone());
        let mut cursor = Cursor::new(data);
        for (seek, size) in [(SeekFrom::Start(0), 100), (SeekFrom::Current(50), 4000), (SeekFrom::End(-30), 100),
                             (SeekFrom::End(20), 10), (SeekFrom::Start(9_990), 64), (SeekFrom::Current(-5000), 1)] {
            assert_eq!(reader.seek(seek).unwrap(), cursor.seek(seek).unwrap());
            let (mut a, mut b) = (vec![0; size], vec![0; size]);
            assert_eq!(reader.read(&mut a).unwrap(), cursor.read(&mut b).unwrap());
            assert_eq!(a, b);
        }
        assert!(reader.seek(SeekFrom::Current(-1_000_000)).is_err());
        assert!(cursor.seek(SeekFrom::Current(-1_000_000)).is_err());
    }

    #[test]
    fn trim_keeps_the_forward_window_and_the_loop_start_and_fill_restores_all() {
        let data = pattern(100_000);
        let (file, mut reader) = File::new(data.clone());
        reader.seek(SeekFrom::Start(40_000)).unwrap();
        assert_eq!(file.trim(10_000), (100_000, 10_000));
        let mut buf = vec![0; 10_000];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[40_000..50_000]);
        assert!(reader.read(&mut buf).is_err());
        assert_eq!(file.misses(), 1);
        // Near the end the loop restart keeps the file start too.
        reader.seek(SeekFrom::Start(95_000)).unwrap();
        assert!(file.fill(data.clone()));
        assert_eq!(file.trim(10_000), (100_000, 15_000));
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, data[95_000..]);
        reader.seek(SeekFrom::Start(0)).unwrap();
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[..10_000]);
        // A second trim only keeps what is still held.
        reader.seek(SeekFrom::Start(5_000)).unwrap();
        file.trim(10_000);
        assert_eq!(file.held(), 10_000);
        assert!(!file.fill(data[1..].to_vec()));
        assert!(file.fill(data.clone()));
        reader.seek(SeekFrom::Start(60_000)).unwrap();
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[60_000..70_000]);
    }

    #[test]
    fn trimmed_and_refilled_looping_decoder_is_sample_exact() {
        // 4 s of stereo noise is 705,644 bytes; a 256 KiB window is 1.49 s of it.
        let data = wav(4.);
        let mut whole = Decoder::new_looped(Cursor::new(data.clone())).unwrap();
        let (file, reader) = File::new(data.clone());
        let mut kept = Decoder::new_looped(reader).unwrap();
        fn step(samples: usize, whole: &mut dyn Iterator<Item = i16>, kept: &mut dyn Iterator<Item = i16>) {
            for i in 0..samples {
                assert_eq!(whole.next(), kept.next(), "sample {i}");
            }
        }
        step(88_200 * 2, &mut whole, &mut kept);
        // Commit: the outgoing track plays at most the window after the trim.
        file.trim(WINDOW);
        assert_eq!(file.held(), WINDOW);
        step(88_200, &mut whole, &mut kept);
        // Rollback: the whole file comes back and the same decoder continues.
        assert!(file.fill(data.clone()));
        step(88_200 / 2, &mut whole, &mut kept);
        // Trim 0.5 s before the end: the loop restart reads the kept file start.
        file.trim(WINDOW);
        assert!(file.held() < data.len());
        step(88_200 * 3 / 2, &mut whole, &mut kept);
        assert_eq!(file.misses(), 0);
        assert_eq!((whole.channels(), whole.sample_rate()), (kept.channels(), kept.sample_rate()));
    }

    /// Every game music track, as `mood` plays it: a looping decoder over the whole
    /// file against one over a `Reader` trimmed after 30 s (5.74 s more must match),
    /// filled back (rollback, 20 s more), then trimmed 3 s before the end (6 s more,
    /// across the loop restart).
    #[test]
    #[ignore = "requires LOOKING_GLASS_TEST_DATA"]
    fn game_music_is_sample_exact_through_trim_fill_and_loop() {
        let root = std::path::PathBuf::from(std::env::var("LOOKING_GLASS_TEST_DATA").unwrap());
        let mut assets = crate::assets::Assets::open(&root).unwrap();
        let mut tracks = std::collections::BTreeSet::new();
        for name in assets.maps() {
            if let Ok(text) = assets.read(&format!("sound/music/{name}.mus")) {
                for cue in super::super::music_cues(&String::from_utf8_lossy(&text)).unwrap().into_values() {
                    tracks.insert(cue.path);
                }
            }
        }
        assert!(tracks.len() >= 20, "{tracks:?}");
        for path in &tracks {
            let data = assets.read(path).unwrap();
            let mut whole = Decoder::new_looped(Cursor::new(data.clone())).unwrap();
            let (file, reader) = File::new(data.clone());
            let mut kept = Decoder::new_looped(reader).unwrap();
            let rate = whole.sample_rate() as usize * whole.channels() as usize;
            fn step(seconds: f32, rate: usize, played: &mut usize, path: &str,
                    whole: &mut LoopedDecoder<Cursor<Vec<u8>>>, kept: &mut LoopedDecoder<Reader>) {
                for _ in 0..(seconds * rate as f32) as usize {
                    assert_eq!(whole.next(), kept.next(), "{path} sample {played}");
                    *played += 1;
                }
            }
            let mut played = 0usize;
            step(30., rate, &mut played, path, &mut whole, &mut kept);
            let (held, after) = file.trim(WINDOW);
            assert!(after <= 2 * WINDOW && held == data.len(), "{path}");
            step(5.74, rate, &mut played, path, &mut whole, &mut kept);
            assert!(file.fill(data.clone()));
            step(20., rate, &mut played, path, &mut whole, &mut kept);
            let total = Decoder::new(Cursor::new(data.clone())).unwrap().count();
            let left = total.saturating_sub(played) as f32 / rate as f32;
            if left > 3. {
                step(left - 3., rate, &mut played, path, &mut whole, &mut kept);
            }
            file.trim(WINDOW);
            step(6., rate, &mut played, path, &mut whole, &mut kept);
            assert_eq!(file.misses(), 0, "{path}");
            println!("MUSIC_EXACT path={path} bytes={} samples={played} kept_after_trim={after}", data.len());
        }
    }
}
