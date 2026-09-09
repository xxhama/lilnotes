//! One-time background migration of legacy WAV recordings to FLAC.
//!
//! Recordings made before 0.3 are 16 kHz mono i16 WAVs. On every launch of a
//! returning user (DB unlocked at startup) a low-priority thread looks for
//! finished meetings that still reference a `.wav` in any audio column and,
//! one meeting at a time:
//!
//! 1. transcodes each existing `.wav` to a sibling `.flac` and verifies the
//!    result decodes bit-exactly ([`codec::convert_wav_to_flac`]);
//! 2. points the meeting row at the new files ([`Db::set_audio_paths`]);
//! 3. only then deletes the `.wav` sources.
//!
//! Idempotent by construction: rows already on `.flac` are never selected,
//! a crash between 1 and 2 just redoes the conversion (the encoder truncates
//! an existing target), a crash between 2 and 3 leaves an orphan `.wav` that
//! the next run cannot see any more — cheap, and the FLAC is already in use.
//! A meeting whose conversion fails (unreadable, wrong format) is logged and
//! left completely untouched; the next launch retries it. The start is
//! delayed so the sweep never competes with the user's first action after
//! launch, and paths recorded in the DB whose file is already gone keep
//! their column (that is the "audio deleted" state the UI understands).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use super::codec;
use crate::db::{AudioPaths, Db, LazyDb};

/// Grace period before the sweep starts, so it never races the user's first
/// transcription/playback after launch.
const START_DELAY: Duration = Duration::from_secs(10);

/// Payload of the `audio_migration:progress` event. Emitted once at the start
/// (`done == 0`) and after every meeting; `done == total` means finished.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MigrationProgress {
    /// Meetings processed so far (converted or skipped after a failure).
    pub done: usize,
    /// Meetings that referenced a legacy WAV when the sweep started.
    pub total: usize,
    /// Meetings left untouched because a conversion failed.
    pub failed: usize,
}

/// Spawn the sweep on its own thread (returning users, after the DB unlocks).
/// Does nothing if there is nothing to migrate.
pub fn spawn(app: AppHandle, db: Arc<LazyDb>) {
    let spawned = std::thread::Builder::new()
        .name("audio-migrate".into())
        .spawn(move || {
            std::thread::sleep(START_DELAY);
            let Some(db) = db.get() else {
                return;
            };
            run(db, |p| {
                let _ = app.emit_to("main", "audio_migration:progress", p);
            });
        });
    if let Err(e) = spawned {
        eprintln!("[migrate] could not start the audio migration thread: {e}");
    }
}

/// Convert every legacy meeting, reporting progress through `report`.
/// Returns the final progress (also what the last `report` call received).
pub fn run(db: &Db, mut report: impl FnMut(MigrationProgress)) -> MigrationProgress {
    let rows = match db.meetings_with_wav_audio() {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("[migrate] cannot list legacy recordings: {e}");
            return MigrationProgress {
                done: 0,
                total: 0,
                failed: 0,
            };
        }
    };
    let mut progress = MigrationProgress {
        done: 0,
        total: rows.len(),
        failed: 0,
    };
    if rows.is_empty() {
        return progress;
    }
    eprintln!(
        "[migrate] {} meeting(s) still on WAV — converting to FLAC in the background",
        rows.len()
    );
    report(progress.clone());

    for (id, paths) in rows {
        match migrate_meeting(&paths) {
            Ok((new_paths, sources)) => {
                // Point the row at the FLACs before removing the sources: an
                // interruption here leaves a redundant WAV, never a dangling
                // path.
                match db.set_audio_paths(id, &new_paths) {
                    Ok(()) => {
                        for src in &sources {
                            if let Err(e) = std::fs::remove_file(src) {
                                eprintln!(
                                    "[migrate] meeting {id}: cannot remove {}: {e}",
                                    src.display()
                                );
                            }
                        }
                        eprintln!(
                            "[migrate] meeting {id}: converted {} file(s) to FLAC",
                            sources.len()
                        );
                    }
                    Err(e) => {
                        eprintln!("[migrate] meeting {id}: DB update failed, keeping WAVs: {e}");
                        progress.failed += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("[migrate] meeting {id}: left on WAV: {e}");
                progress.failed += 1;
            }
        }
        progress.done += 1;
        report(progress.clone());
    }
    eprintln!(
        "[migrate] done: {} converted, {} failed",
        progress.total - progress.failed,
        progress.failed
    );
    progress
}

/// Convert one meeting's WAVs. Returns the updated path set plus the source
/// files to delete once the DB points at the new ones. Columns whose file is
/// already gone are kept as they are. On any failure every FLAC written for
/// this meeting is removed again so nothing half-done is left behind.
fn migrate_meeting(paths: &AudioPaths) -> Result<(AudioPaths, Vec<std::path::PathBuf>), String> {
    let mut out = paths.clone();
    let mut sources = Vec::new();
    let mut written = Vec::new();
    let mut convert = |slot: &mut Option<String>| -> Result<(), String> {
        let Some(current) = slot.clone() else {
            return Ok(());
        };
        if !codec::is_wav(&current) {
            return Ok(());
        }
        let src = Path::new(&current);
        if !src.is_file() {
            return Ok(()); // already deleted; keep the column as-is
        }
        let dst = codec::convert_wav_to_flac(src)?;
        written.push(dst.clone());
        *slot = Some(dst.to_string_lossy().into_owned());
        sources.push(src.to_path_buf());
        Ok(())
    };
    let result = convert(&mut out.mic)
        .and_then(|()| convert(&mut out.system))
        .and_then(|()| convert(&mut out.cleaned));
    if let Err(e) = result {
        for f in &written {
            let _ = std::fs::remove_file(f);
        }
        return Err(e);
    }
    Ok((out, sources))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::resampler::TARGET_RATE;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lilnotes-migrate-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_wav(path: &Path, n: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TARGET_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..n {
            w.write_sample(((i % 200) as i16 - 100) * 50).unwrap();
        }
        w.finalize().unwrap();
    }

    fn open_db(dir: &Path) -> Db {
        Db::open(&dir.join("test.sqlite3"), &[0x42u8; 32]).unwrap()
    }

    #[test]
    fn migrate_meeting_converts_existing_wavs_and_keeps_missing_columns() {
        let dir = temp_dir("meeting");
        let mic = dir.join("mic.wav");
        let system = dir.join("system.wav");
        write_wav(&mic, 3000);
        write_wav(&system, 3000);
        let paths = AudioPaths {
            mic: Some(mic.to_string_lossy().into_owned()),
            system: Some(system.to_string_lossy().into_owned()),
            // Recorded in the DB but the file is gone.
            cleaned: Some(dir.join("mic_cleaned.wav").to_string_lossy().into_owned()),
        };
        let (out, sources) = migrate_meeting(&paths).unwrap();
        assert_eq!(
            out.mic.as_deref(),
            Some(dir.join("mic.flac").to_str().unwrap())
        );
        assert_eq!(
            out.system.as_deref(),
            Some(dir.join("system.flac").to_str().unwrap())
        );
        assert_eq!(out.cleaned, paths.cleaned, "missing file keeps its column");
        assert_eq!(sources, vec![mic.clone(), system.clone()]);
        assert!(
            mic.exists() && system.exists(),
            "sources are not deleted here"
        );
        let (_, samples) = codec::read_mono_f32(out.mic.as_deref().unwrap()).unwrap();
        assert_eq!(samples.len(), 3000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn migrate_meeting_failure_leaves_no_flac_behind() {
        let dir = temp_dir("fail");
        let mic = dir.join("mic.wav");
        write_wav(&mic, 1000);
        let bad = dir.join("system.wav");
        std::fs::write(&bad, b"definitely not a wav").unwrap();
        let paths = AudioPaths {
            mic: Some(mic.to_string_lossy().into_owned()),
            system: Some(bad.to_string_lossy().into_owned()),
            cleaned: None,
        };
        assert!(migrate_meeting(&paths).is_err());
        assert!(!dir.join("mic.flac").exists(), "partial output rolled back");
        assert!(mic.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn run_converts_legacy_meetings_updates_db_and_deletes_wavs() {
        let dir = temp_dir("run");
        let db = open_db(&dir);
        let mic = dir.join("mic.wav");
        let system = dir.join("system.wav");
        write_wav(&mic, 2000);
        write_wav(&system, 2000);
        let legacy = db.insert_meeting_started("s1", "t", 0).unwrap();
        db.finalize_meeting(legacy, 1, mic.to_str().unwrap(), system.to_str().unwrap())
            .unwrap();
        // A meeting whose WAV is unreadable stays as it is.
        let bad = dir.join("bad.wav");
        std::fs::write(&bad, b"nope").unwrap();
        let broken = db.insert_meeting_started("s2", "t", 5).unwrap();
        db.finalize_meeting(broken, 6, bad.to_str().unwrap(), bad.to_str().unwrap())
            .unwrap();

        let mut reports = Vec::new();
        let last = run(&db, |p| reports.push(p));
        assert_eq!(
            last,
            MigrationProgress {
                done: 2,
                total: 2,
                failed: 1
            }
        );
        assert_eq!(reports.first().unwrap().done, 0);
        assert_eq!(reports.last().unwrap(), &last);

        let m = db.get_meeting(legacy).unwrap();
        assert_eq!(
            m.mic_wav.as_deref(),
            Some(dir.join("mic.flac").to_str().unwrap())
        );
        assert_eq!(
            m.system_wav.as_deref(),
            Some(dir.join("system.flac").to_str().unwrap())
        );
        assert!(!mic.exists() && !system.exists(), "WAV sources deleted");
        assert!(dir.join("mic.flac").exists() && dir.join("system.flac").exists());
        let b = db.get_meeting(broken).unwrap();
        assert_eq!(b.mic_wav.as_deref(), Some(bad.to_str().unwrap()));
        assert!(bad.exists());

        // Second run: only the broken one is left, and it fails again.
        let again = run(&db, |_| {});
        assert_eq!((again.total, again.failed), (1, 1));
        drop(db);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn run_is_a_no_op_without_legacy_rows() {
        let dir = temp_dir("noop");
        let db = open_db(&dir);
        let id = db.insert_meeting_started("s", "t", 0).unwrap();
        db.finalize_meeting(id, 1, "/x/mic.flac", "/x/system.flac")
            .unwrap();
        let mut calls = 0;
        let p = run(&db, |_| calls += 1);
        assert_eq!((p.done, p.total, calls), (0, 0, 0));
        drop(db);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
