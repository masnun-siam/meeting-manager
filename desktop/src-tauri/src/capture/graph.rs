//! The capture graph: a global process tap and the default input device joined in one private
//! aggregate device, read by one IOProc. Every Core Audio object is owned by `Parts`, whose `Drop`
//! destroys what exists in reverse order, so a build that fails halfway leaks nothing (a leaked
//! tap or aggregate device stays in the system until reboot).

use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, bail};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, Message};
use objc2_core_audio::{
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap, AudioObjectID,
    CATapDescription, CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey,
    kAudioAggregateDeviceIsStackedKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioObjectPropertyScopeGlobal, kAudioObjectUnknown,
    kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};

use super::devices::{self, AudioBufferList, StreamFormat};
use super::mix::{Downmix, Resampler, mix, to_i16};
use super::{Denied, SAMPLE_RATE, Samples};

/// Frames reserved per scratch buffer; Core Audio's default IO buffer is 512 frames.
const SCRATCH_FRAMES: usize = 16_384;

type IoProc = unsafe extern "C-unwind" fn(
    AudioObjectID,
    *const c_void,
    *const AudioBufferList,
    *const c_void,
    *mut AudioBufferList,
    *const c_void,
    *mut c_void,
) -> i32;
/// `AudioDeviceIOProcID` is the IOProc's function pointer type.
type IoProcId = Option<IoProc>;

// The IOProc functions, declared here because their bindings need the `objc2-core-audio-types`
// and `dispatch2` crates, which D22 does not approve.
#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C-unwind" {
    fn AudioDeviceCreateIOProcID(
        device: AudioObjectID,
        proc_: IoProcId,
        client: *mut c_void,
        out: *mut IoProcId,
    ) -> i32;
    fn AudioDeviceDestroyIOProcID(device: AudioObjectID, id: IoProcId) -> i32;
    fn AudioDeviceStart(device: AudioObjectID, id: IoProcId) -> i32;
    fn AudioDeviceStop(device: AudioObjectID, id: IoProcId) -> i32;
}

/// A running graph. Dropping it stops the IOProc and destroys the IOProc id, the aggregate
/// device and the tap, in that order.
pub struct Graph {
    _parts: Parts,
}

impl Graph {
    /// Builds the graph on the current default devices and starts delivering to `samples`.
    /// `broken` is set when the IOProc sees buffers in a layout it was not built for.
    pub fn build(samples: Arc<Mutex<Samples>>, broken: Arc<AtomicBool>) -> anyhow::Result<Graph> {
        let mut parts = Parts::create()?;
        let aggregate = parts.aggregate;

        if let Err(e) = devices::set_nominal_rate(aggregate, f64::from(SAMPLE_RATE)) {
            tracing::warn!("{e}; resampling instead");
        }
        let rate = devices::nominal_rate(aggregate)?;

        let layout = devices::input_channels(aggregate, "read aggregate stream layout")?;
        let expected: Vec<u32> = parts
            .mic_layout
            .iter()
            .copied()
            .chain([parts.tap_channels])
            .collect();
        if layout != expected {
            bail!(
                "Core Audio aggregate device delivers {} streams instead of {}; \
                 try again or reconnect the microphone",
                layout.len(),
                expected.len()
            );
        }

        let context = Box::into_raw(Box::new(Context {
            mic_buffers: parts.mic_layout.len(),
            buffers: expected.len(),
            mic: Downmix::with_capacity(SCRATCH_FRAMES),
            tap: Downmix::with_capacity(SCRATCH_FRAMES),
            mixed: Vec::with_capacity(SCRATCH_FRAMES),
            resampler: Resampler::new(rate, f64::from(SAMPLE_RATE)),
            resampled: Vec::with_capacity(SCRATCH_FRAMES * 2),
            out: Vec::with_capacity(SCRATCH_FRAMES * 2),
            samples,
            broken,
        }));
        parts.context = context;

        let mut proc_id: IoProcId = None;
        // SAFETY: `io_proc` matches Core Audio's IOProc signature; `context` stays alive until
        // the IOProc id is destroyed in `Parts::drop`.
        let code = unsafe {
            AudioDeviceCreateIOProcID(aggregate, Some(io_proc), context.cast(), &mut proc_id)
        };
        devices::status("create IOProc", code)?;
        parts.proc_id = proc_id;
        // SAFETY: `proc_id` was just created on this device.
        devices::status("start device", unsafe {
            AudioDeviceStart(aggregate, proc_id)
        })?;
        parts.started = true;
        Ok(Graph { _parts: parts })
    }
}

// SAFETY: `Graph` only holds Core Audio object ids, the IOProc id and the context the IOProc
// reads; Core Audio objects may be stopped and destroyed from any thread, and the context is never
// touched outside the IOProc until the IOProc id is destroyed.
unsafe impl Send for Graph {}

/// Creates a tap and an aggregate device and destroys them at once. The first tap creation makes
/// macOS ask for System Audio Recording permission; a failure means it is denied (D36).
pub fn probe() -> anyhow::Result<()> {
    Parts::create().map(drop)
}

struct Parts {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    mic_layout: Vec<u32>,
    tap_channels: u32,
    proc_id: IoProcId,
    started: bool,
    context: *mut Context,
}

impl Parts {
    fn create() -> anyhow::Result<Parts> {
        let input = devices::default_input()?;
        let input_uid = devices::device_uid(input)?;
        let mic_layout = devices::input_channels(input, "read microphone stream layout")?;

        let mut parts = Parts {
            tap: kAudioObjectUnknown,
            aggregate: kAudioObjectUnknown,
            mic_layout,
            tap_channels: 0,
            proc_id: None,
            started: false,
            context: ptr::null_mut(),
        };

        let exclude: Retained<NSArray<NSNumber>> = match devices::own_process_object() {
            Some(id) => NSArray::from_retained_slice(&[NSNumber::new_u32(id)]),
            None => NSArray::new(),
        };
        let tap_uuid = NSUUID::new();
        // SAFETY: plain Objective-C calls on a freshly allocated description.
        let description = unsafe {
            let d = CATapDescription::initMonoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &exclude,
            );
            d.setName(&NSString::from_str("Meeting capture"));
            d.setUUID(&tap_uuid);
            d.setPrivate(true);
            // The user still hears the call.
            d.setMuteBehavior(CATapMuteBehavior::Unmuted);
            d
        };
        // SAFETY: `description` is a valid CATapDescription and `parts.tap` a valid out pointer.
        let code = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut parts.tap) };
        devices::status("create process tap", code).context(Denied::SystemAudio)?;

        let format: StreamFormat = devices::get(
            parts.tap,
            devices::address(kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal),
            None,
            "read tap format",
        )?;
        parts.tap_channels = format.channels;

        let sub_device = dictionary(&[(kAudioSubDeviceUIDKey, object(input_uid.clone()))]);
        let tap = dictionary(&[
            (kAudioSubTapUIDKey, object(tap_uuid.UUIDString())),
            (
                kAudioSubTapDriftCompensationKey,
                object(NSNumber::new_bool(true)),
            ),
        ]);
        let aggregate_uid = format!(
            "com.masnunsiam.meeting-manager.capture.{}",
            NSUUID::new().UUIDString()
        );
        let description = dictionary(&[
            (
                kAudioAggregateDeviceNameKey,
                object(NSString::from_str("Meeting capture")),
            ),
            (
                kAudioAggregateDeviceUIDKey,
                object(NSString::from_str(&aggregate_uid)),
            ),
            (
                kAudioAggregateDeviceIsPrivateKey,
                object(NSNumber::new_bool(true)),
            ),
            (
                kAudioAggregateDeviceIsStackedKey,
                object(NSNumber::new_bool(false)),
            ),
            (
                kAudioAggregateDeviceTapAutoStartKey,
                object(NSNumber::new_bool(true)),
            ),
            (kAudioAggregateDeviceMainSubDeviceKey, object(input_uid)),
            (
                kAudioAggregateDeviceSubDeviceListKey,
                object(NSArray::from_retained_slice(&[sub_device])),
            ),
            (
                kAudioAggregateDeviceTapListKey,
                object(NSArray::from_retained_slice(&[tap])),
            ),
        ]);
        // SAFETY: NSDictionary is toll-free bridged to CFDictionary; the cast only changes the
        // static type, and the dictionary outlives the call.
        let code = unsafe {
            AudioHardwareCreateAggregateDevice(
                &*Retained::as_ptr(&description).cast(),
                NonNull::from(&mut parts.aggregate),
            )
        };
        devices::status("create aggregate device", code).context(Denied::SystemAudio)?;
        Ok(parts)
    }
}

impl Drop for Parts {
    fn drop(&mut self) {
        let report = |stage: &str, code: i32| {
            if let Err(e) = devices::status(stage, code) {
                tracing::warn!("{e}");
            }
        };
        // SAFETY: each id is destroyed once, only if it was created, and in reverse order of
        // creation. The context is freed after its IOProc id is destroyed, when Core Audio no
        // longer calls the IOProc.
        unsafe {
            if self.started {
                report("stop device", AudioDeviceStop(self.aggregate, self.proc_id));
            }
            if self.proc_id.is_some() {
                report(
                    "destroy IOProc",
                    AudioDeviceDestroyIOProcID(self.aggregate, self.proc_id),
                );
            }
            if !self.context.is_null() {
                drop(Box::from_raw(self.context));
            }
            if self.aggregate != kAudioObjectUnknown {
                report(
                    "destroy aggregate device",
                    AudioHardwareDestroyAggregateDevice(self.aggregate),
                );
            }
            if self.tap != kAudioObjectUnknown {
                report(
                    "destroy process tap",
                    AudioHardwareDestroyProcessTap(self.tap),
                );
            }
        }
    }
}

fn object<T: Message>(value: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every Objective-C object is an AnyObject; this is an upcast.
    unsafe { Retained::cast_unchecked(value) }
}

fn dictionary(
    entries: &[(&std::ffi::CStr, Retained<AnyObject>)],
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let keys: Vec<Retained<NSString>> = entries
        .iter()
        .map(|(k, _)| NSString::from_str(k.to_str().expect("Core Audio keys are ASCII")))
        .collect();
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<Retained<AnyObject>> = entries.iter().map(|(_, v)| v.clone()).collect();
    NSDictionary::from_retained_objects(&keys, &values)
}

/// What the IOProc owns: the buffer layout it was built for and preallocated scratch buffers.
struct Context {
    mic_buffers: usize,
    buffers: usize,
    mic: Downmix,
    tap: Downmix,
    mixed: Vec<f32>,
    resampler: Resampler,
    resampled: Vec<f32>,
    out: Vec<i16>,
    samples: Arc<Mutex<Samples>>,
    broken: Arc<AtomicBool>,
}

impl Context {
    /// Real-time: no locks that block, no logging, no allocation within the reserved capacity.
    fn deliver(&mut self, buffers: &[devices::AudioBuffer]) {
        if buffers.len() != self.buffers {
            self.broken.store(true, Ordering::Relaxed);
            return;
        }
        self.mic.clear();
        self.tap.clear();
        for (i, buffer) in buffers.iter().enumerate() {
            if buffer.data.is_null() || buffer.channels == 0 {
                continue;
            }
            // SAFETY: Core Audio delivers 32-bit float samples, `bytes` long, valid for this call.
            let samples = unsafe {
                std::slice::from_raw_parts(buffer.data.cast::<f32>(), buffer.bytes as usize / 4)
            };
            let sink = if i < self.mic_buffers {
                &mut self.mic
            } else {
                &mut self.tap
            };
            sink.add(samples, buffer.channels as usize);
        }
        mix(self.mic.finish(), self.tap.finish(), &mut self.mixed);
        let pcm = if self.resampler.is_identity() {
            &self.mixed
        } else {
            self.resampler.process(&self.mixed, &mut self.resampled);
            &self.resampled
        };
        to_i16(pcm, &mut self.out);
        // A rebuild never holds this lock while an IOProc runs; if it is ever held, the buffer is
        // dropped rather than blocking the real-time thread.
        if let Ok(mut samples) = self.samples.try_lock() {
            samples(&self.out);
        }
    }
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: *const c_void,
    input: *const AudioBufferList,
    _input_time: *const c_void,
    _output: *mut AudioBufferList,
    _output_time: *const c_void,
    data: *mut c_void,
) -> i32 {
    if input.is_null() || data.is_null() {
        return 0;
    }
    // SAFETY: `data` is the Context registered with this IOProc, used only on this thread while
    // the IOProc runs; `input` is Core Audio's buffer list for this cycle.
    let (context, buffers) = unsafe {
        (
            &mut *data.cast::<Context>(),
            AudioBufferList::buffers(input),
        )
    };
    // A panic must not unwind into Core Audio.
    if panic::catch_unwind(AssertUnwindSafe(|| context.deliver(buffers))).is_err() {
        context.broken.store(true, Ordering::Relaxed);
    }
    0
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Needs a microphone and both permissions for the test binary's host app:
    /// `cargo test -p meeting-desktop capture -- --ignored`.
    #[test]
    #[ignore]
    fn the_real_graph_delivers_48_khz_mono() {
        let count = Arc::new(Mutex::new(0usize));
        let samples: Samples = Box::new({
            let count = count.clone();
            move |s: &[i16]| *count.lock().unwrap() += s.len()
        });
        let broken = Arc::new(AtomicBool::new(false));
        let graph = Graph::build(Arc::new(Mutex::new(samples)), broken.clone()).expect("graph");
        let before = *count.lock().unwrap();
        let start = Instant::now();
        std::thread::sleep(Duration::from_secs(2));
        let rate = (*count.lock().unwrap() - before) as f64 / start.elapsed().as_secs_f64();
        drop(graph);
        assert!(!broken.load(Ordering::Relaxed));
        assert!(
            (rate - 48_000.0).abs() < 4_800.0,
            "{rate} samples per second"
        );
    }
}
