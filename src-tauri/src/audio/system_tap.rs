//! System-audio capture via a Core Audio process tap (macOS 14.2+).
//!
//! Recipe (order and shape matter — every step below is load-bearing):
//! 1. `CATapDescription(stereoGlobalTapButExcludeProcesses: [])` — a global
//!    mixdown tap. Do NOT touch `isExclusive` afterwards: it is the
//!    include/exclude *direction* flag set by the initializer, not a lock
//!    toggle; flipping it inverts the tap to "only these PIDs" (= silence).
//! 2. `AudioHardwareCreateProcessTap` — first call triggers the TCC prompt
//!    (`NSAudioCaptureUsageDescription`, "System Audio Recording Only").
//! 3. Build a *private* aggregate device whose main sub-device is the real
//!    default output device, with the tap attached in the tap list and
//!    `tapautostart` enabled. A tap-only aggregate silently yields zeros.
//! 4. Install a classic `AudioDeviceCreateIOProcID` callback directly on the
//!    aggregate (NOT AVAudioEngine, which can't be retargeted; and not the
//!    block variant, whose nil-queue path silently fails on macOS 26).
//! 5. Teardown in reverse: stop -> destroy IOProc -> destroy aggregate ->
//!    destroy tap.

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{bounded, Sender};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AllocAnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyStreams, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectSystemObject, kAudioObjectUnknown,
    kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat, AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID,
    AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop, AudioHardwareCreateAggregateDevice,
    AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
};
use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use super::pipeline::{ChannelMeters, ChannelPipeline, LiveChunk, Source};

pub struct SystemThread {
    pub handle: JoinHandle<Result<PathBuf, String>>,
}

pub fn spawn(
    wav_path: PathBuf,
    stop: Arc<AtomicBool>,
    meters: Arc<ChannelMeters>,
    live_tx: Option<Sender<LiveChunk>>,
    ready_tx: Sender<Result<(), String>>,
) -> SystemThread {
    let handle = std::thread::Builder::new()
        .name("system-capture".into())
        .spawn(move || run(wav_path, stop, meters, live_tx, ready_tx))
        .expect("failed to spawn system capture thread");
    SystemThread { handle }
}

/// Quickly create and destroy a tap to surface the TCC prompt / probe access.
/// Returns Ok(()) if a tap could be created.
pub fn probe_access() -> Result<(), String> {
    let tap = TapHandle::create()?;
    drop(tap);
    Ok(())
}

// ---------------------------------------------------------------------------
// RAII wrappers so teardown happens in the right order on every exit path.
// ---------------------------------------------------------------------------

struct TapHandle {
    id: AudioObjectID,
    uid: String,
}

impl TapHandle {
    fn create() -> Result<Self, String> {
        // SAFETY: standard objc2 alloc/init pattern; empty exclude list means
        // "tap everything".
        let desc = unsafe {
            let excluded: Retained<NSArray<NSNumber>> = NSArray::new();
            let desc = CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &excluded,
            );
            desc.setName(&NSString::from_str("LilNotes system audio tap"));
            desc.setPrivate(true);
            desc
        };
        let uid = unsafe { desc.UUID().UUIDString().to_string() };

        let mut tap_id: AudioObjectID = kAudioObjectUnknown;
        // SAFETY: desc is a valid CATapDescription, tap_id a valid out-pointer.
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&desc), &mut tap_id) };
        if status != 0 || tap_id == kAudioObjectUnknown {
            return Err(format!(
                "could not create system-audio tap (OSStatus {status}). \
                 This usually means System Audio Recording permission was denied."
            ));
        }
        Ok(Self { id: tap_id, uid })
    }

    /// Ask the tap for its stream format (sample rate / channels).
    fn format(&self) -> Option<AudioStreamBasicDescription> {
        // SAFETY: ASBD is a plain-old-data struct; zeroed is a valid value.
        let mut asbd: AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioTapPropertyFormat,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        // SAFETY: valid object id, address, and sized out-buffer.
        let status = unsafe {
            AudioObjectGetPropertyData(
                self.id,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(&mut asbd as *mut _ as *mut c_void).unwrap(),
            )
        };
        (status == 0).then_some(asbd)
    }
}

impl Drop for TapHandle {
    fn drop(&mut self) {
        // SAFETY: id came from AudioHardwareCreateProcessTap.
        unsafe { AudioHardwareDestroyProcessTap(self.id) };
    }
}

struct AggregateHandle {
    id: AudioObjectID,
}

impl AggregateHandle {
    fn create(tap_uid: &str, output_uid: &str) -> Result<Self, String> {
        // Helpers: the aggregate-description keys are C string constants,
        // and the values are heterogeneous (strings, bools, arrays), so the
        // dictionaries are built as NSDictionary<NSString, AnyObject>.
        let key = |k: &'static std::ffi::CStr| -> Retained<NSString> {
            NSString::from_str(k.to_str().expect("key is valid utf8"))
        };
        fn any<T: objc2::Message>(v: Retained<T>) -> Retained<AnyObject> {
            // SAFETY: upcasting any Objective-C object to AnyObject is valid.
            unsafe { Retained::cast_unchecked(v) }
        }
        fn dict(
            keys: &[&NSString],
            values: Vec<Retained<AnyObject>>,
        ) -> Retained<NSDictionary<NSString, AnyObject>> {
            let value_refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
            NSDictionary::from_slices(keys, &value_refs)
        }

        let sub_device = dict(
            &[&key(kAudioSubDeviceUIDKey)],
            vec![any(NSString::from_str(output_uid))],
        );
        let sub_tap = dict(
            &[
                &key(kAudioSubTapUIDKey),
                &key(kAudioSubTapDriftCompensationKey),
            ],
            vec![
                any(NSString::from_str(tap_uid)),
                any(NSNumber::new_bool(true)),
            ],
        );

        let agg_uid = format!("co.elastic.lilnote.tap-aggregate.{}", std::process::id());
        let desc = dict(
            &[
                &key(kAudioAggregateDeviceNameKey),
                &key(kAudioAggregateDeviceUIDKey),
                &key(kAudioAggregateDeviceMainSubDeviceKey),
                &key(kAudioAggregateDeviceIsPrivateKey),
                &key(kAudioAggregateDeviceIsStackedKey),
                &key(kAudioAggregateDeviceTapAutoStartKey),
                &key(kAudioAggregateDeviceSubDeviceListKey),
                &key(kAudioAggregateDeviceTapListKey),
            ],
            vec![
                any(NSString::from_str("LilNotes tap aggregate")),
                any(NSString::from_str(&agg_uid)),
                any(NSString::from_str(output_uid)),
                any(NSNumber::new_bool(true)),
                any(NSNumber::new_bool(false)),
                any(NSNumber::new_bool(true)),
                any(NSArray::from_retained_slice(&[any(sub_device)])),
                any(NSArray::from_retained_slice(&[any(sub_tap)])),
            ],
        );

        // NSDictionary is toll-free bridged to CFDictionary.
        let cf: &CFDictionary =
            unsafe { &*(Retained::as_ptr(&desc) as *const CFDictionary) };

        let mut agg_id: AudioObjectID = kAudioObjectUnknown;
        // SAFETY: cf is a valid dictionary; agg_id a valid out-pointer.
        let status =
            unsafe { AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut agg_id)) };
        if status != 0 || agg_id == kAudioObjectUnknown {
            return Err(format!(
                "could not create aggregate device for the system tap (OSStatus {status})"
            ));
        }
        Ok(Self { id: agg_id })
    }
}

impl Drop for AggregateHandle {
    fn drop(&mut self) {
        // SAFETY: id came from AudioHardwareCreateAggregateDevice.
        unsafe { AudioHardwareDestroyAggregateDevice(self.id) };
    }
}

struct IOProcHandle {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    started: bool,
    /// Owned callback context; freed on drop after the proc is destroyed.
    ctx: *mut IOContext,
}

struct IOContext {
    tx: Sender<Vec<f32>>,
    /// Leading ABL buffers that belong to the output device's own input
    /// streams (e.g. an AirPods mic) — NOT system audio; skipped.
    skip_buffers: usize,
    /// Chunks dropped because the writer thread fell behind.
    dropped: std::sync::atomic::AtomicU64,
    /// One-time buffer-layout log for diagnostics.
    layout_logged: AtomicBool,
}

impl IOProcHandle {
    fn install(
        device: AudioObjectID,
        tx: Sender<Vec<f32>>,
        skip_buffers: usize,
    ) -> Result<Self, String> {
        let ctx = Box::into_raw(Box::new(IOContext {
            tx,
            skip_buffers,
            dropped: std::sync::atomic::AtomicU64::new(0),
            layout_logged: AtomicBool::new(false),
        }));
        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: io_proc matches AudioDeviceIOProc; ctx outlives the proc.
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                device,
                Some(io_proc),
                ctx as *mut c_void,
                NonNull::from(&mut proc_id),
            )
        };
        if status != 0 || proc_id.is_none() {
            // SAFETY: reclaim the leaked context on failure.
            unsafe { drop(Box::from_raw(ctx)) };
            return Err(format!("could not install IOProc (OSStatus {status})"));
        }
        // SAFETY: valid device + proc id.
        let status = unsafe { AudioDeviceStart(device, proc_id) };
        if status != 0 {
            unsafe {
                AudioDeviceDestroyIOProcID(device, proc_id);
                drop(Box::from_raw(ctx));
            }
            return Err(format!("could not start aggregate device (OSStatus {status})"));
        }
        Ok(Self {
            device,
            proc_id,
            started: true,
            ctx,
        })
    }
}

impl Drop for IOProcHandle {
    fn drop(&mut self) {
        unsafe {
            if self.started {
                AudioDeviceStop(self.device, self.proc_id);
            }
            AudioDeviceDestroyIOProcID(self.device, self.proc_id);
            drop(Box::from_raw(self.ctx));
        }
    }
}

/// The realtime IO callback: downmix whatever the buffer list contains
/// (interleaved stereo, non-interleaved, or mono — don't assume) and hand it
/// off. Kept minimal; all heavy lifting happens on the writer thread.
unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    in_input_data: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _out_output_data: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client_data: *mut c_void,
) -> i32 {
    let ctx = &*(client_data as *const IOContext);
    let abl = in_input_data.as_ref();
    let n_buffers = abl.mNumberBuffers as usize;
    if n_buffers == 0 {
        return 0;
    }
    let all_buffers = std::slice::from_raw_parts(abl.mBuffers.as_ptr(), n_buffers);

    // One-time layout log: which buffers arrived and what we skip.
    if !ctx.layout_logged.swap(true, Ordering::Relaxed) {
        let desc: Vec<String> = all_buffers
            .iter()
            .map(|b| format!("{}ch/{}B", b.mNumberChannels, b.mDataByteSize))
            .collect();
        eprintln!(
            "[system-tap] ABL layout: {} buffers [{}], skipping first {}",
            n_buffers,
            desc.join(", "),
            ctx.skip_buffers
        );
    }

    // Skip the output device's own input streams; the tap's stream(s) come
    // after sub-device streams. If skipping would consume everything (an
    // unexpected layout), fall back to using all buffers.
    let buffers = if ctx.skip_buffers > 0 && ctx.skip_buffers < n_buffers {
        &all_buffers[ctx.skip_buffers..]
    } else {
        all_buffers
    };
    let n_buffers = buffers.len();

    let mono: Vec<f32> = if n_buffers > 1 {
        // Non-interleaved: one buffer per channel; average frame-wise.
        let frames = (buffers[0].mDataByteSize as usize) / 4;
        let mut acc = vec![0.0f32; frames];
        let mut used = 0usize;
        for b in buffers {
            if b.mData.is_null() {
                continue;
            }
            let ch = std::slice::from_raw_parts(b.mData as *const f32, (b.mDataByteSize as usize) / 4);
            for (a, &s) in acc.iter_mut().zip(ch.iter()) {
                *a += s;
            }
            used += 1;
        }
        if used > 1 {
            for a in acc.iter_mut() {
                *a /= used as f32;
            }
        }
        acc
    } else {
        let b = &buffers[0];
        if b.mData.is_null() {
            return 0;
        }
        let data = std::slice::from_raw_parts(b.mData as *const f32, (b.mDataByteSize as usize) / 4);
        let ch = (b.mNumberChannels as usize).max(1);
        super::pipeline::downmix_interleaved(data, ch)
    };

    if ctx.tx.try_send(mono).is_err() {
        ctx.dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    0
}

fn default_output_device() -> Result<(AudioObjectID, String), String> {
    // System object -> default output device id
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut device: AudioObjectID = kAudioObjectUnknown;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    // SAFETY: sized out-buffer for a u32 device id.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(&mut device as *mut _ as *mut c_void).unwrap(),
        )
    };
    if status != 0 || device == kAudioObjectUnknown {
        return Err(format!("no default output device (OSStatus {status})"));
    }

    // Device id -> UID string (CFString, toll-free bridged to NSString; the
    // "get property" returns a +1 reference we take ownership of).
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyDeviceUID,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut uid_ptr: *mut NSString = std::ptr::null_mut();
    let mut size = std::mem::size_of::<*mut NSString>() as u32;
    // SAFETY: out-buffer holds one CFStringRef.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(&mut uid_ptr as *mut _ as *mut c_void).unwrap(),
        )
    };
    if status != 0 || uid_ptr.is_null() {
        return Err(format!("could not read output device UID (OSStatus {status})"));
    }
    // SAFETY: we own the +1 reference returned by the property call.
    let uid = unsafe { Retained::from_raw(uid_ptr) }
        .ok_or_else(|| "device UID was null".to_string())?;
    Ok((device, uid.to_string()))
}

/// Number of *input* streams a device exposes. The aggregate's IOProc input
/// buffer list is [sub-device input streams…, tap streams…] in that order,
/// so this is how many leading buffers to skip. AirPods / USB headsets have
/// a mic input stream here; MacBook speakers have none.
fn input_stream_count(device: AudioObjectID) -> usize {
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreams,
        mScope: kAudioObjectPropertyScopeInput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size: u32 = 0;
    // SAFETY: standard property-size query.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(device, NonNull::from(&addr), 0, std::ptr::null(), NonNull::from(&mut size))
    };
    if status != 0 {
        return 0;
    }
    size as usize / std::mem::size_of::<AudioObjectID>()
}

/// A device's nominal sample rate (the rate its IO actually runs at).
fn nominal_sample_rate(device: AudioObjectID) -> Option<f64> {
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyNominalSampleRate,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut rate: f64 = 0.0;
    let mut size = std::mem::size_of::<f64>() as u32;
    // SAFETY: sized out-buffer for one Float64.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(&mut rate as *mut _ as *mut c_void).unwrap(),
        )
    };
    (status == 0 && rate > 0.0).then_some(rate)
}

// ---------------------------------------------------------------------------
// Capture thread body
// ---------------------------------------------------------------------------

fn run(
    wav_path: PathBuf,
    stop: Arc<AtomicBool>,
    meters: Arc<ChannelMeters>,
    live_tx: Option<Sender<LiveChunk>>,
    ready_tx: Sender<Result<(), String>>,
) -> Result<PathBuf, String> {
    let setup = (|| -> Result<(TapHandle, AggregateHandle, u32), String> {
        let tap = TapHandle::create()?;
        let (out_device, out_uid) = default_output_device()?;
        // The output device's own input streams (if any — AirPods mic, USB
        // headset mic…) appear FIRST in the aggregate's input buffer list
        // and must not be mixed into the system recording.
        let skip_buffers = input_stream_count(out_device);
        let agg = AggregateHandle::create(&tap.uid, &out_uid)?;

        // The IOProc delivers data at the AGGREGATE's IO rate — which is
        // the output device's rate, not necessarily what the tap reports.
        // Getting this wrong records audio at the wrong speed.
        let tap_rate = tap.format().map(|f| f.mSampleRate);
        let agg_rate = nominal_sample_rate(agg.id);
        let sample_rate = agg_rate.or(tap_rate).unwrap_or(48_000.0) as u32;
        eprintln!(
            "[system-tap] output device {out_uid}: input streams to skip = {skip_buffers}, \
             aggregate rate = {agg_rate:?}, tap-reported rate = {tap_rate:?}, using {sample_rate} Hz"
        );
        Ok((tap, agg, sample_rate, skip_buffers))
    })();

    let (tap, agg, sample_rate, skip_buffers) = match setup {
        Ok(x) => x,
        Err(e) => {
            let _ = ready_tx.send(Err(e.clone()));
            return Err(e);
        }
    };

    let mut pipeline =
        match ChannelPipeline::new(&wav_path, sample_rate, Source::System, meters, live_tx) {
            Ok(p) => p,
            Err(e) => {
                let _ = ready_tx.send(Err(e.clone()));
                return Err(e);
            }
        };

    // 256 chunks ≈ several seconds of headroom at typical IO cycle sizes.
    let (chunk_tx, chunk_rx) = bounded::<Vec<f32>>(256);
    let ioproc = match IOProcHandle::install(agg.id, chunk_tx, skip_buffers) {
        Ok(h) => h,
        Err(e) => {
            let _ = ready_tx.send(Err(e.clone()));
            return Err(e);
        }
    };
    let _ = ready_tx.send(Ok(()));

    let started = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match chunk_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => pipeline.push(&chunk)?,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Diagnostics before teardown: dropped chunks + wall-clock vs recorded
    // duration (a mismatch >1% means rate misconfiguration or drops).
    let dropped = unsafe { (*ioproc.ctx).dropped.load(Ordering::Relaxed) };

    // Teardown order: stop IO -> destroy proc -> destroy aggregate -> destroy
    // tap. The Drop impls encode this; explicit drops make the order obvious.
    drop(ioproc);
    while let Ok(chunk) = chunk_rx.try_recv() {
        pipeline.push(&chunk)?;
    }
    drop(agg);
    drop(tap);

    let wall_ms = started.elapsed().as_millis() as u64;
    let rec_ms = pipeline.duration_ms();
    eprintln!(
        "[system-tap] stopped: wall={wall_ms}ms recorded={rec_ms}ms dropped_chunks={dropped}"
    );
    if dropped > 0 {
        eprintln!("[system-tap] WARNING: writer fell behind; audio has gaps");
    }

    pipeline.finalize()
}
