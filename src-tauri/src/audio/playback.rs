//! Playback cache: constant-bitrate WAV copies of the FLAC recordings for
//! the webview's `<audio>` elements.
//!
//! WebKit streams `asset:` media through `AVAssetResourceLoader` without
//! `AVURLAssetPreferPreciseDurationAndTimingKey`, so AVFoundation seeks by
//! *byte-offset estimation*. A FLAC speech recording is heavily variable
//! bitrate (silence frames are ~11 bytes, speech frames kilobytes), so a
//! transcript click lands seconds off and the mic and system channels drift
//! apart from each other. A WAV is constant bitrate, which makes the same
//! estimate exact — the pre-FLAC playback path that was known to work.
//!
//! So the FLACs stay the source of truth on disk and this module decodes
//! them on demand into `<app data dir>/playback-cache/<meeting id>/`, which
//! is inside the asset protocol scope (`$APPDATA/**`) whatever
//! `settings.storage_dir` says. Rules:
//!
//! - Cache files are **content-keyed**: `<stem>-<hash(src, len, mtime)>.wav`.
//!   A changed source (echo clean, revert, re-clean) gets a new name and the
//!   frontend swaps `src`; stale siblings are swept on the next [`prepare`].
//!   Nothing ever deletes a WAV a live `<audio>` may still be streaming —
//!   the asset protocol reopens the file on every Range request — which is
//!   why the echo-clean commands do not invalidate and only the paths that
//!   delete a meeting's audio call [`invalidate`].
//! - Legacy `.wav` sources are returned as they are.
//! - The whole cache is wiped at startup ([`clear_all`]) and holds at most
//!   [`MAX_MEETINGS`] meetings (evicted by directory mtime, never the one
//!   being prepared).
//! - Decoding streams block by block, so a multi-hour meeting costs disk
//!   (~115 MB per channel-hour, transient) but not memory.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::codec;

/// Cache directory name under the app data dir.
pub const CACHE_DIR: &str = "playback-cache";
/// Meetings kept in the cache at once (the one being viewed plus a couple of
/// recently opened ones so going back is instant).
const MAX_MEETINGS: usize = 3;

/// Serialises `prepare` calls: React StrictMode double-invokes effects and
/// fast navigation can fire two prepares for the same meeting; the second
/// one must see a finished file, not a half-written temp.
static PREPARE_LOCK: Mutex<()> = Mutex::new(());

/// Paths the player should load. Both are either legacy WAVs or cache WAVs.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackAudio {
    pub mic_wav: String,
    pub system_wav: String,
}

/// `<app data dir>/playback-cache`.
pub fn cache_root(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(CACHE_DIR)
}

fn meeting_dir(root: &Path, meeting_id: i64) -> PathBuf {
    root.join(meeting_id.to_string())
}

/// Make `mic` and `system` playable with exact seeking: `.wav` sources pass
/// through, FLACs are decoded into the meeting's cache dir (or reused when
/// already there). Also sweeps stale files for this meeting and evicts old
/// meetings from the cache.
pub fn prepare(
    root: &Path,
    meeting_id: i64,
    mic: &str,
    system: &str,
) -> Result<PlaybackAudio, String> {
    let _guard = PREPARE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = meeting_dir(root, meeting_id);
    let mic_out = resolve(&dir, mic)?;
    let system_out = resolve(&dir, system)?;
    sweep(&dir, &[&mic_out, &system_out]);
    if dir.is_dir() {
        touch(&dir);
        evict(root, meeting_id);
    }
    Ok(PlaybackAudio {
        mic_wav: mic_out.to_string_lossy().into_owned(),
        system_wav: system_out.to_string_lossy().into_owned(),
    })
}

/// Drop a meeting's cache dir (its audio is being deleted). Missing is fine.
pub fn invalidate(root: &Path, meeting_id: i64) {
    remove_dir_quiet(&meeting_dir(root, meeting_id));
}

/// Drop the whole cache (startup). Missing is fine.
pub fn clear_all(root: &Path) {
    remove_dir_quiet(root);
}

fn remove_dir_quiet(dir: &Path) {
    if let Err(e) = fs::remove_dir_all(dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("[playback] cannot remove {}: {e}", dir.display());
        }
    }
}

/// The playable path for one source: itself for a WAV, else the (possibly
/// freshly decoded) cache WAV.
fn resolve(dir: &Path, src: &str) -> Result<PathBuf, String> {
    if codec::is_wav(src) {
        return Ok(PathBuf::from(src));
    }
    let src_path = Path::new(src);
    let meta = fs::metadata(src_path).map_err(|e| format!("cannot stat {src}: {e}"))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = DefaultHasher::new();
    src.hash(&mut h);
    meta.len().hash(&mut h);
    mtime.hash(&mut h);
    let stem = src_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("audio");
    let dst = dir.join(format!("{stem}-{:016x}.wav", h.finish()));
    if dst.is_file() {
        return Ok(dst);
    }
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dst.with_extension("wav.tmp");
    let started = std::time::Instant::now();
    let result = codec::transcode_flac_to_wav(src_path, &tmp).and_then(|n| {
        fs::rename(&tmp, &dst)
            .map(|()| n)
            .map_err(|e| format!("cannot rename {}: {e}", tmp.display()))
    });
    match result {
        Ok(n) => {
            eprintln!(
                "[playback] decoded {src} -> {} ({:.1}s of audio in {:?})",
                dst.display(),
                n as f64 / super::resampler::TARGET_RATE as f64,
                started.elapsed()
            );
            Ok(dst)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Remove everything in the meeting dir that is not one of `keep` (previous
/// keys of the same channel, orphaned temps).
fn sweep(dir: &Path, keep: &[&PathBuf]) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.iter().any(|k| **k == path) {
            continue;
        }
        let _ = fs::remove_file(&path);
    }
}

fn touch(dir: &Path) {
    if let Ok(f) = fs::File::open(dir) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// Keep the newest `MAX_MEETINGS` meeting dirs (by mtime) plus `current`.
fn evict(root: &Path, current: i64) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut dirs: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if !path.is_dir() {
                return None;
            }
            let mtime = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((mtime, path))
        })
        .collect();
    dirs.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    let current_name = current.to_string();
    for (_, path) in dirs.into_iter().skip(MAX_MEETINGS) {
        if path.file_name().and_then(|n| n.to_str()) == Some(current_name.as_str()) {
            continue;
        }
        remove_dir_quiet(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::resampler::TARGET_RATE;
    use std::time::Duration;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lilnotes-playback-{}-{name}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pcm(n: usize) -> Vec<i16> {
        (0..n).map(|i| ((i % 200) as i16 - 100) * 50).collect()
    }

    fn write_flac(path: &Path, n: usize) {
        let mut w = codec::MonoWriter::create(path).unwrap();
        w.write_samples(&pcm(n)).unwrap();
        w.finalize().unwrap();
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn prepare_returns_wav_sources_unchanged() {
        let dir = temp_dir("wav");
        let root = dir.join("cache");
        let mic = s(&dir.join("mic.wav"));
        let system = s(&dir.join("system.wav"));
        let out = prepare(&root, 7, &mic, &system).unwrap();
        assert_eq!(out.mic_wav, mic);
        assert_eq!(out.system_wav, system);
        assert!(!root.join("7").exists(), "no cache dir for wav sources");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prepare_decodes_flac_into_cache_and_hits_second_time() {
        let dir = temp_dir("hit");
        let root = dir.join("cache");
        let mic = dir.join("mic.flac");
        let system = dir.join("system.flac");
        write_flac(&mic, 3000);
        write_flac(&system, 5000);

        let out = prepare(&root, 1, &s(&mic), &s(&system)).unwrap();
        let mic_out = PathBuf::from(&out.mic_wav);
        let sys_out = PathBuf::from(&out.system_wav);
        assert_eq!(mic_out.parent().unwrap(), root.join("1"));
        assert!(mic_out
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("mic-"));
        assert!(sys_out
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("system-"));
        assert!(codec::is_wav(&out.mic_wav) && codec::is_wav(&out.system_wav));
        let (rate, samples) = codec::read_mono_f32(&out.mic_wav).unwrap();
        assert_eq!(rate, TARGET_RATE);
        assert_eq!(samples.len(), 3000);
        assert_eq!(codec::read_mono_f32(&out.system_wav).unwrap().1.len(), 5000);
        assert_eq!(entries(&root.join("1")).len(), 2);

        let mtime = fs::metadata(&mic_out).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let again = prepare(&root, 1, &s(&mic), &s(&system)).unwrap();
        assert_eq!(again, out);
        assert_eq!(fs::metadata(&mic_out).unwrap().modified().unwrap(), mtime);
        assert_eq!(entries(&root.join("1")).len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prepare_rekeys_changed_source_and_sweeps_stale() {
        let dir = temp_dir("rekey");
        let root = dir.join("cache");
        let mic = dir.join("mic.flac");
        let system = dir.join("system.flac");
        write_flac(&mic, 3000);
        write_flac(&system, 3000);
        let first = prepare(&root, 2, &s(&mic), &s(&system)).unwrap();
        // Orphaned temp from an interrupted decode is swept too.
        fs::write(root.join("2").join("mic-dead.wav.tmp"), b"x").unwrap();

        write_flac(&mic, 4000); // different length -> different key
        let second = prepare(&root, 2, &s(&mic), &s(&system)).unwrap();
        assert_ne!(second.mic_wav, first.mic_wav);
        assert_eq!(second.system_wav, first.system_wav);
        assert!(!Path::new(&first.mic_wav).exists(), "old mic swept");
        assert_eq!(entries(&root.join("2")).len(), 2);
        assert_eq!(codec::read_mono_f32(&second.mic_wav).unwrap().1.len(), 4000);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prepare_uses_distinct_names_for_mic_and_cleaned() {
        let dir = temp_dir("cleaned");
        let root = dir.join("cache");
        let mic = dir.join("mic.flac");
        let cleaned = dir.join("mic_cleaned.flac");
        let system = dir.join("system.flac");
        write_flac(&mic, 3000);
        write_flac(&cleaned, 3000);
        write_flac(&system, 3000);
        let raw = prepare(&root, 3, &s(&mic), &s(&system)).unwrap();
        let clean = prepare(&root, 3, &s(&cleaned), &s(&system)).unwrap();
        assert_ne!(raw.mic_wav, clean.mic_wav);
        assert!(clean.mic_wav.contains("mic_cleaned-"));
        assert!(
            !Path::new(&raw.mic_wav).exists(),
            "raw mic swept after clean"
        );
        assert_eq!(entries(&root.join("3")).len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prepare_failure_leaves_no_partial_output() {
        let dir = temp_dir("fail");
        let root = dir.join("cache");
        let mic = dir.join("mic.flac");
        let system = dir.join("system.flac");
        write_flac(&mic, 3000);
        fs::write(&system, b"definitely not a flac").unwrap();
        assert!(prepare(&root, 4, &s(&mic), &s(&system)).is_err());
        let left = entries(&root.join("4"));
        assert!(
            left.iter()
                .all(|n| n.starts_with("mic-") && n.ends_with(".wav")),
            "only the good channel may remain: {left:?}"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn evict_keeps_newest_n_and_current() {
        let dir = temp_dir("evict");
        let root = dir.join("cache");
        // Five meeting dirs with staggered mtimes: 10 (oldest) .. 14 (newest).
        for (i, id) in (10..15).enumerate() {
            let d = root.join(id.to_string());
            fs::create_dir_all(&d).unwrap();
            fs::File::open(&d)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(1_000_000 + i as u64 * 60))
                .unwrap();
        }
        // Current = the oldest one; it must survive.
        evict(&root, 10);
        let mut left = entries(&root);
        left.sort();
        assert_eq!(left, vec!["10", "12", "13", "14"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prepare_evicts_older_meetings() {
        let dir = temp_dir("prepare-evict");
        let root = dir.join("cache");
        let mic = dir.join("mic.flac");
        let system = dir.join("system.flac");
        write_flac(&mic, 1000);
        write_flac(&system, 1000);
        for id in 1..=(MAX_MEETINGS as i64 + 1) {
            prepare(&root, id, &s(&mic), &s(&system)).unwrap();
            std::thread::sleep(Duration::from_millis(15));
        }
        let left = entries(&root);
        assert_eq!(left.len(), MAX_MEETINGS);
        assert!(!left.contains(&"1".to_string()), "oldest evicted: {left:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalidate_and_clear_all_ignore_missing() {
        let dir = temp_dir("missing");
        let root = dir.join("cache");
        invalidate(&root, 99);
        clear_all(&root);
        fs::create_dir_all(root.join("5")).unwrap();
        invalidate(&root, 5);
        assert!(!root.join("5").exists());
        clear_all(&root);
        assert!(!root.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
