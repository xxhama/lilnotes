//! On-disk codec for the per-channel recordings.
//!
//! Every channel file is 16 kHz mono 16-bit PCM. Since 0.3 it is stored as
//! **FLAC** (lossless, ~2-10x smaller than WAV for speech: the mic channel is
//! mostly digital silence once the other side talks) instead of WAV. This
//! module is the only place that knows the container:
//!
//! - [`MonoWriter`] streams FLAC to disk frame by frame during capture through
//!   libFLAC's reference encoder (`flac-bound`). libFLAC rewrites STREAMINFO
//!   (total samples + MD5) on [`MonoWriter::finalize`], which is what WebKit
//!   reads the `<audio>` duration from — always finalize.
//! - [`read_mono_f32`] decodes either a `.flac` (claxon) or a legacy `.wav`
//!   (hound; recordings made before the switch) into f32 samples in [-1, 1],
//!   dispatched on the file extension. The DB stores bare paths, so the
//!   extension is the only format signal; keep WAV reading around forever.
//! - [`convert_wav_to_flac`] is the one-time migration primitive used by
//!   `audio::migrate` for recordings that predate FLAC.
//! - [`transcode_flac_to_wav`] is the inverse, used by `audio::playback` to
//!   hand the webview a constant-bitrate WAV: AVFoundation seeks FLAC by
//!   byte-offset estimation when WebKit streams it, which lands seconds off
//!   in a variable-bitrate speech recording.
//!
//! A crash mid-recording leaves a FLAC whose STREAMINFO still says
//! `total_samples = 0` and possibly a truncated last frame. The frames before
//! the cut are intact, but [`read_mono_f32`] is strict (any decode error is an
//! error, like the WAV path always was) and WebKit reports an infinite
//! duration. Salvaging such files is a follow-up, not something this module
//! attempts.

use std::path::{Path, PathBuf};

use flac_bound::FlacEncoder;

use super::resampler::TARGET_RATE;

/// Mic channel file name inside a session directory.
pub const MIC_FILE: &str = "mic.flac";
/// System-audio channel file name inside a session directory.
pub const SYSTEM_FILE: &str = "system.flac";
/// Offline echo-cleaned mic, written next to the mic file by `clean_echo`.
pub const MIC_CLEANED_FILE: &str = "mic_cleaned.flac";

/// libFLAC compression level. 5 is the reference encoder's default: for 16 kHz
/// speech the size difference to level 8 is ~1% while encoding is 2-3x cheaper,
/// which matters because this runs on the capture thread.
const COMPRESSION_LEVEL: u32 = 5;

/// Path of the echo-cleaned mic for a given mic recording: `mic_cleaned.flac`
/// in the same directory, whatever the source's own extension is (a legacy
/// `mic.wav` still gets a `.flac` cleaned copy).
pub fn cleaned_mic_path(mic: &str) -> PathBuf {
    Path::new(mic).with_file_name(MIC_CLEANED_FILE)
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// True if `path` names a legacy WAV recording.
pub fn is_wav(path: &str) -> bool {
    has_extension(Path::new(path), "wav")
}

/// Streaming FLAC writer for one 16 kHz mono i16 channel.
///
/// Not `Send`: libFLAC's encoder handle is used from the capture thread that
/// created it, which is how `ChannelPipeline` uses it. Dropping without
/// [`finalize`](Self::finalize) still closes the file (libFLAC finishes on
/// drop) but swallows any error, so callers should finalize explicitly.
pub struct MonoWriter {
    enc: Option<FlacEncoder<'static>>,
    path: PathBuf,
    /// Reusable i16 → i32 widening buffer (libFLAC takes i32 samples).
    scratch: Vec<i32>,
    frames: u64,
}

impl MonoWriter {
    pub fn create(path: &Path) -> Result<Self, String> {
        let enc = FlacEncoder::new()
            .ok_or("libFLAC: cannot allocate encoder")?
            .channels(1)
            .bits_per_sample(16)
            .sample_rate(TARGET_RATE)
            .compression_level(COMPRESSION_LEVEL)
            .init_file(&path)
            .map_err(|e| format!("failed to create {}: {e:?}", path.display()))?;
        Ok(Self {
            enc: Some(enc),
            path: path.to_path_buf(),
            scratch: Vec::new(),
            frames: 0,
        })
    }

    /// Append samples. One libFLAC call per invocation; the encoder buffers
    /// internally until it has a full block, so call this per capture chunk,
    /// not per sample.
    pub fn write_samples(&mut self, samples: &[i16]) -> Result<(), String> {
        if samples.is_empty() {
            return Ok(());
        }
        let enc = self.enc.as_mut().ok_or("flac writer already finalized")?;
        self.scratch.clear();
        self.scratch.extend(samples.iter().map(|&s| s as i32));
        enc.process_interleaved(&self.scratch, samples.len() as u32)
            .map_err(|()| {
                format!(
                    "flac write failed for {}: {:?}",
                    self.path.display(),
                    enc.state()
                )
            })?;
        self.frames += samples.len() as u64;
        Ok(())
    }

    /// Samples written so far.
    pub fn frames_written(&self) -> u64 {
        self.frames
    }

    /// Flush the last block and patch STREAMINFO. Returns the file path.
    pub fn finalize(mut self) -> Result<PathBuf, String> {
        let enc = self.enc.take().ok_or("flac writer already finalized")?;
        enc.finish().map_err(|enc| {
            format!(
                "flac finalize failed for {}: {:?}",
                self.path.display(),
                enc.state()
            )
        })?;
        Ok(self.path)
    }
}

/// Decode a channel file (FLAC or legacy WAV, by extension) as mono f32 in
/// [-1, 1]. Multi-channel files yield channel 0 (the capture path is mono;
/// this is only defensive). Returns the sample rate so callers can reject
/// anything that isn't the pipeline's 16 kHz.
pub fn read_mono_f32(path: &str) -> Result<(u32, Vec<f32>), String> {
    if has_extension(Path::new(path), "flac") {
        let (rate, bits, raw) = read_flac_channel0(Path::new(path))?;
        let scale = 1.0 / (1u32 << (bits - 1)) as f32;
        Ok((rate, raw.into_iter().map(|v| v as f32 * scale).collect()))
    } else {
        read_wav_mono_f32(path)
    }
}

/// Decode a FLAC's channel 0 as raw integer samples: `(sample_rate,
/// bits_per_sample, samples)`. Strict: any malformed frame is an error.
fn read_flac_channel0(path: &Path) -> Result<(u32, u32, Vec<i32>), String> {
    let mut reader = claxon::FlacReader::open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let info = reader.streaminfo();
    if info.bits_per_sample == 0 || info.bits_per_sample > 32 {
        return Err(format!(
            "cannot read {}: unsupported bit depth {}",
            path.display(),
            info.bits_per_sample
        ));
    }
    let channels = info.channels.max(1);
    let mut out: Vec<i32> = Vec::with_capacity(info.samples.unwrap_or(0) as usize);
    let mut frames = reader.blocks();
    let mut buf = Vec::with_capacity(info.max_block_size as usize * channels as usize);
    loop {
        match frames.read_next_or_eof(buf) {
            Ok(Some(block)) => {
                out.extend_from_slice(block.channel(0));
                buf = block.into_buffer();
            }
            Ok(None) => break,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        }
    }
    Ok((info.sample_rate, info.bits_per_sample, out))
}

/// Legacy WAV reader (recordings made before FLAC). Handles 16-bit int and
/// 32-bit float, channel 0 of multi-channel files.
fn read_wav_mono_f32(path: &str) -> Result<(u32, Vec<f32>), String> {
    use hound::{SampleFormat, WavReader};
    let mut reader = WavReader::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let raw: Vec<f32> = match spec.sample_format {
        SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>(),
        SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>(),
    }
    .map_err(|e| format!("cannot read {path}: {e}"))?;
    let mono = if ch == 1 {
        raw
    } else {
        raw.into_iter().step_by(ch).collect()
    };
    Ok((spec.sample_rate, mono))
}

/// Transcode a legacy 16 kHz mono i16 WAV to `<same name>.flac` next to it
/// and verify the result decodes bit-exactly to the source. The source is
/// left in place (the caller deletes it once the DB points at the FLAC). On
/// any failure the partial `.flac` is removed and an error returned; a file
/// that isn't 16 kHz mono i16 is an error, not converted.
pub fn convert_wav_to_flac(src: &Path) -> Result<PathBuf, String> {
    use hound::{SampleFormat, WavReader};
    let src_str = src.to_string_lossy();
    let mut reader = WavReader::open(src).map_err(|e| format!("cannot open {src_str}: {e}"))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != TARGET_RATE
        || spec.bits_per_sample != 16
        || spec.sample_format != SampleFormat::Int
    {
        return Err(format!(
            "{src_str}: expected 16 kHz mono 16-bit PCM, got {} ch {} Hz {}-bit {:?}",
            spec.channels, spec.sample_rate, spec.bits_per_sample, spec.sample_format
        ));
    }
    let pcm: Vec<i16> = reader
        .samples::<i16>()
        .collect::<Result<_, _>>()
        .map_err(|e| format!("cannot read {src_str}: {e}"))?;

    let dst = src.with_extension("flac");
    let write = || -> Result<(), String> {
        let mut w = MonoWriter::create(&dst)?;
        // Encode in blocks so the scratch buffer stays small.
        for chunk in pcm.chunks(TARGET_RATE as usize) {
            w.write_samples(chunk)?;
        }
        w.finalize()?;
        let (rate, bits, back) = read_flac_channel0(&dst)?;
        if rate != TARGET_RATE || bits != 16 {
            return Err(format!(
                "{}: verification failed (got {rate} Hz {bits}-bit)",
                dst.display()
            ));
        }
        if back.len() != pcm.len() || back.iter().zip(&pcm).any(|(&b, &p)| b != p as i32) {
            return Err(format!(
                "{}: verification failed (decoded audio differs from the source)",
                dst.display()
            ));
        }
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&dst);
        return Err(e);
    }
    Ok(dst)
}

/// Decode a FLAC channel file to a 16 kHz mono i16 WAV at `dst`, streaming
/// block by block (a multi-hour file is never held in memory). Anything that
/// is not 16 kHz 16-bit is an error, not converted. Returns the number of
/// samples written. On error the partial `dst` is left for the caller to
/// remove (it typically writes to a temp name and renames on success).
pub fn transcode_flac_to_wav(src: &Path, dst: &Path) -> Result<u64, String> {
    let mut reader =
        claxon::FlacReader::open(src).map_err(|e| format!("cannot open {}: {e}", src.display()))?;
    let info = reader.streaminfo();
    if info.sample_rate != TARGET_RATE || info.bits_per_sample != 16 {
        return Err(format!(
            "{}: expected {TARGET_RATE} Hz 16-bit FLAC, got {} Hz {}-bit",
            src.display(),
            info.sample_rate,
            info.bits_per_sample
        ));
    }
    let channels = info.channels.max(1) as usize;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(dst, spec)
        .map_err(|e| format!("cannot create {}: {e}", dst.display()))?;
    let mut frames = reader.blocks();
    let mut buf = Vec::with_capacity(info.max_block_size as usize * channels);
    let mut written = 0u64;
    loop {
        match frames.read_next_or_eof(buf) {
            Ok(Some(block)) => {
                let ch0 = block.channel(0);
                let mut w = writer.get_i16_writer(ch0.len() as u32);
                for &v in ch0 {
                    w.write_sample(v as i16);
                }
                w.flush()
                    .map_err(|e| format!("cannot write {}: {e}", dst.display()))?;
                written += ch0.len() as u64;
                buf = block.into_buffer();
            }
            Ok(None) => break,
            Err(e) => return Err(format!("cannot read {}: {e}", src.display())),
        }
    }
    writer
        .finalize()
        .map_err(|e| format!("cannot finalize {}: {e}", dst.display()))?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Seek, SeekFrom, Write};

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lilnotes-codec-{}-{name}", std::process::id()))
    }

    /// Deterministic sine + LCG noise, long enough to span several 4096-sample
    /// FLAC blocks plus a partial tail.
    fn pattern(n: usize) -> Vec<i16> {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        (0..n)
            .map(|i| {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let noise = ((seed >> 33) as i64 % 2001 - 1000) as f32;
                let tone = (i as f32 * 0.07).sin() * 12_000.0;
                (tone + noise).round().clamp(-32768.0, 32767.0) as i16
            })
            .collect()
    }

    fn write_flac(path: &Path, pcm: &[i16]) {
        let mut w = MonoWriter::create(path).unwrap();
        for chunk in pcm.chunks(1000) {
            w.write_samples(chunk).unwrap();
        }
        assert_eq!(w.frames_written(), pcm.len() as u64);
        w.finalize().unwrap();
    }

    fn assert_matches(read: &[f32], pcm: &[i16]) {
        assert_eq!(read.len(), pcm.len());
        for (r, &p) in read.iter().zip(pcm) {
            assert_eq!(*r, p as f32 / 32768.0);
        }
    }

    #[test]
    fn flac_round_trip_is_bit_exact() {
        let path = temp_path("roundtrip.flac");
        let pcm = pattern(50_000);
        write_flac(&path, &pcm);
        let (rate, read) = read_mono_f32(&path.to_string_lossy()).unwrap();
        assert_eq!(rate, TARGET_RATE);
        assert_matches(&read, &pcm);
        // It really is smaller than PCM.
        assert!(fs::metadata(&path).unwrap().len() < (pcm.len() * 2) as u64);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn flac_empty_file_finalizes() {
        let path = temp_path("empty.flac");
        write_flac(&path, &[]);
        let (rate, read) = read_mono_f32(&path.to_string_lossy()).unwrap();
        assert_eq!(rate, TARGET_RATE);
        assert!(read.is_empty());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn wav_fallback_reads_legacy_int16() {
        let path = temp_path("legacy.wav");
        let pcm = pattern(5_000);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TARGET_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for &s in &pcm {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
        let (rate, read) = read_mono_f32(&path.to_string_lossy()).unwrap();
        assert_eq!(rate, TARGET_RATE);
        assert_matches(&read, &pcm);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn wav_float_stereo_takes_channel_0() {
        let path = temp_path("stereo.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..100 {
            w.write_sample(i as f32 / 100.0).unwrap(); // left
            w.write_sample(-1.0f32).unwrap(); // right
        }
        w.finalize().unwrap();
        let (rate, read) = read_mono_f32(&path.to_string_lossy()).unwrap();
        assert_eq!(rate, 48_000);
        assert_eq!(read.len(), 100);
        assert_eq!(read[0], 0.0);
        assert_eq!(read[50], 0.5);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn truncated_flac_is_an_error() {
        let path = temp_path("truncated.flac");
        let pcm = pattern(30_000);
        write_flac(&path, &pcm);
        // Chop the file in the middle of the audio frames.
        let len = fs::metadata(&path).unwrap().len();
        let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(len * 2 / 3).unwrap();
        drop(f);
        assert!(read_mono_f32(&path.to_string_lossy()).is_err());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn garbage_flac_is_an_error() {
        let path = temp_path("garbage.flac");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(b"not a flac file at all").unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        drop(f);
        assert!(read_mono_f32(&path.to_string_lossy()).is_err());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn convert_wav_to_flac_round_trips() {
        let wav = temp_path("convert.wav");
        let pcm = pattern(20_000);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TARGET_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for &s in &pcm {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();

        let flac = convert_wav_to_flac(&wav).unwrap();
        assert_eq!(flac, wav.with_extension("flac"));
        assert!(wav.exists(), "source must be left in place");
        let (rate, read) = read_mono_f32(&flac.to_string_lossy()).unwrap();
        assert_eq!(rate, TARGET_RATE);
        assert_matches(&read, &pcm);
        fs::remove_file(&wav).unwrap();
        fs::remove_file(&flac).unwrap();
    }

    #[test]
    fn convert_rejects_non_pipeline_wav_and_leaves_no_output() {
        let wav = temp_path("reject.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for _ in 0..10 {
            w.write_sample(1i16).unwrap();
            w.write_sample(2i16).unwrap();
        }
        w.finalize().unwrap();
        assert!(convert_wav_to_flac(&wav).is_err());
        assert!(!wav.with_extension("flac").exists());
        fs::remove_file(&wav).unwrap();
    }

    #[test]
    fn transcode_flac_to_wav_round_trips() {
        let flac = temp_path("to-wav.flac");
        let wav = temp_path("to-wav.wav");
        let pcm = pattern(50_000);
        write_flac(&flac, &pcm);
        assert_eq!(
            transcode_flac_to_wav(&flac, &wav).unwrap(),
            pcm.len() as u64
        );
        let (rate, read) = read_mono_f32(&wav.to_string_lossy()).unwrap();
        assert_eq!(rate, TARGET_RATE);
        assert_matches(&read, &pcm);
        let spec = hound::WavReader::open(&wav).unwrap().spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, TARGET_RATE);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_format, hound::SampleFormat::Int);
        // Constant bitrate is the whole point: header + 2 bytes per sample.
        assert_eq!(fs::metadata(&wav).unwrap().len(), 44 + 2 * pcm.len() as u64);
        fs::remove_file(&flac).unwrap();
        fs::remove_file(&wav).unwrap();
    }

    #[test]
    fn transcode_empty_flac_yields_empty_wav() {
        let flac = temp_path("to-wav-empty.flac");
        let wav = temp_path("to-wav-empty.wav");
        write_flac(&flac, &[]);
        assert_eq!(transcode_flac_to_wav(&flac, &wav).unwrap(), 0);
        let (_, read) = read_mono_f32(&wav.to_string_lossy()).unwrap();
        assert!(read.is_empty());
        fs::remove_file(&flac).unwrap();
        fs::remove_file(&wav).unwrap();
    }

    #[test]
    fn transcode_truncated_flac_is_an_error() {
        let flac = temp_path("to-wav-truncated.flac");
        let wav = temp_path("to-wav-truncated.wav");
        write_flac(&flac, &pattern(30_000));
        let len = fs::metadata(&flac).unwrap().len();
        let f = fs::OpenOptions::new().write(true).open(&flac).unwrap();
        f.set_len(len * 2 / 3).unwrap();
        drop(f);
        assert!(transcode_flac_to_wav(&flac, &wav).is_err());
        fs::remove_file(&flac).unwrap();
        let _ = fs::remove_file(&wav);
    }

    #[test]
    fn cleaned_mic_path_ignores_source_extension() {
        assert_eq!(
            cleaned_mic_path("/x/y/mic.flac"),
            PathBuf::from("/x/y/mic_cleaned.flac")
        );
        assert_eq!(
            cleaned_mic_path("/x/y/mic.wav"),
            PathBuf::from("/x/y/mic_cleaned.flac")
        );
        assert!(is_wav("/x/y/mic.WAV"));
        assert!(!is_wav("/x/y/mic.flac"));
    }
}
