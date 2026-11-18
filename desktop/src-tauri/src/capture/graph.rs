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
        if !(rate.is_finite() && rate > 0.0) {
            bail!(
                "Core Audio aggregate device reports no usable sample rate; reconnect the microphone and try again"
            );
        }

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

        let context = Box::into_raw(Box::new(Context::new(
            parts.mic_layout.len(),
            expected.len(),
            rate,
            samples,
            broken,
        )));
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

        let aggregate_uid = format!(
            "com.masnunsiam.meeting-manager.capture.{}",
            NSUUID::new().UUIDString()
        );
        let description = aggregate_description(input_uid, tap_uuid.UUIDString(), &aggregate_uid);
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

/// The private aggregate device: the default input as main sub-device (the clock), joined by
/// the tap with drift compensation.
fn aggregate_description(
    input_uid: Retained<NSString>,
    tap_uid: Retained<NSString>,
    aggregate_uid: &str,
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let sub_device = dictionary(&[(kAudioSubDeviceUIDKey, object(input_uid.clone()))]);
    let tap = dictionary(&[
        (kAudioSubTapUIDKey, object(tap_uid)),
        (
            kAudioSubTapDriftCompensationKey,
            object(NSNumber::new_bool(true)),
        ),
    ]);
    dictionary(&[
        (
            kAudioAggregateDeviceNameKey,
            object(NSString::from_str("Meeting capture")),
        ),
        (
            kAudioAggregateDeviceUIDKey,
            object(NSString::from_str(aggregate_uid)),
        ),
        (
            kAudioAggregateDeviceIsPrivateKey,
            object(NSNumber::new_bool(true)),
        ),
        (
            kAudioAggregateDeviceIsStackedKey,
            object(NSNumber::new_bool(false)),
        ),
        // With auto start the aggregate runs no IO cycles until some process plays audio, so a
        // mic-only recording would get no frames (#257).
        (
            kAudioAggregateDeviceTapAutoStartKey,
            object(NSNumber::new_bool(false)),
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
    ])
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
    /// Reserves every scratch buffer up front; the resampled side grows with the upsampling ratio.
    fn new(
        mic_buffers: usize,
        buffers: usize,
        rate: f64,
        samples: Arc<Mutex<Samples>>,
        broken: Arc<AtomicBool>,
    ) -> Context {
        let ratio = (f64::from(SAMPLE_RATE) / rate).max(1.0);
        let resampled = (SCRATCH_FRAMES as f64 * ratio).ceil() as usize + 1;
        Context {
            mic_buffers,
            buffers,
            mic: Downmix::with_capacity(SCRATCH_FRAMES),
            tap: Downmix::with_capacity(SCRATCH_FRAMES),
            mixed: Vec::with_capacity(SCRATCH_FRAMES),
            resampler: Resampler::new(rate, f64::from(SAMPLE_RATE)),
            resampled: Vec::with_capacity(resampled),
            out: Vec::with_capacity(resampled.max(SCRATCH_FRAMES)),
            samples,
            broken,
        }
    }

    /// Real-time: no locks that block, no logging, no allocation.
    fn deliver(&mut self, buffers: &[devices::AudioBuffer]) {
        if buffers.len() != self.buffers {
            self.broken.store(true, Ordering::Relaxed);
            return;
        }
        // A cycle larger than the reserved scratch would allocate here; drop it instead.
        if buffers
            .iter()
            .any(|b| b.bytes as usize / 4 / (b.channels.max(1) as usize) > SCRATCH_FRAMES)
        {
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
        // dropped rather than blocking the real-time thread. A panic in `samples` poisons it, so
        // every later `try_lock` fails; `io_proc` sets `broken` then, and the health check ends
        // capture.
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

    struct Fake {
        context: Context,
        got: Arc<Mutex<Vec<i16>>>,
        broken: Arc<AtomicBool>,
    }

    /// A context for one mono microphone buffer and one mono tap buffer at `rate`.
    fn fake(rate: f64) -> Fake {
        let got = Arc::new(Mutex::new(Vec::new()));
        let samples: Samples = Box::new({
            let got = got.clone();
            move |s: &[i16]| got.lock().unwrap().extend_from_slice(s)
        });
        let broken = Arc::new(AtomicBool::new(false));
        Fake {
            context: Context::new(1, 2, rate, Arc::new(Mutex::new(samples)), broken.clone()),
            got,
            broken,
        }
    }

    fn buffer(data: &mut [f32]) -> devices::AudioBuffer {
        devices::AudioBuffer {
            channels: 1,
            bytes: (data.len() * 4) as u32,
            data: data.as_mut_ptr().cast(),
        }
    }

    #[test]
    fn the_first_buffers_are_the_microphone_and_the_last_the_tap() {
        let mut f = fake(48_000.0);
        let mut mic = [0.25f32, 0.25, 0.25];
        let mut tap = [0.5f32, -0.25];
        f.context.deliver(&[buffer(&mut mic), buffer(&mut tap)]);
        assert_eq!(*f.got.lock().unwrap(), [24575, 0, 8192]);
        assert!(!f.broken.load(Ordering::Relaxed));
    }

    #[test]
    fn an_unexpected_buffer_count_flags_the_graph_and_delivers_nothing() {
        let mut f = fake(48_000.0);
        let mut mic = [0.5f32; 4];
        f.context.deliver(&[buffer(&mut mic)]);
        assert!(f.got.lock().unwrap().is_empty());
        assert!(f.broken.load(Ordering::Relaxed));
    }

    #[test]
    fn a_buffer_without_data_counts_as_silence() {
        let mut f = fake(48_000.0);
        let mut tap = [0.5f32; 2];
        let empty = devices::AudioBuffer {
            channels: 1,
            bytes: 8,
            data: ptr::null_mut(),
        };
        f.context.deliver(&[empty, buffer(&mut tap)]);
        assert_eq!(*f.got.lock().unwrap(), [16384, 16384]);
    }

    #[test]
    fn a_cycle_larger_than_the_scratch_is_dropped_without_allocating() {
        let mut f = fake(48_000.0);
        let capacity = f.context.out.capacity();
        let mut mic = vec![0.1f32; SCRATCH_FRAMES + 1];
        let mut tap = [0.1f32; 4];
        f.context.deliver(&[buffer(&mut mic), buffer(&mut tap)]);
        assert!(f.got.lock().unwrap().is_empty());
        assert_eq!(f.context.out.capacity(), capacity);
        assert!(!f.broken.load(Ordering::Relaxed));
    }

    #[test]
    fn upsampling_a_full_cycle_stays_within_the_reserved_capacity() {
        let mut f = fake(16_000.0);
        let (resampled, out) = (f.context.resampled.capacity(), f.context.out.capacity());
        let mut mic = vec![0.1f32; SCRATCH_FRAMES];
        let mut tap = vec![0.1f32; SCRATCH_FRAMES];
        f.context.deliver(&[buffer(&mut mic), buffer(&mut tap)]);
        let delivered = f.got.lock().unwrap().len();
        assert!(delivered.abs_diff(SCRATCH_FRAMES * 3) <= 3, "{delivered}");
        assert_eq!(f.context.resampled.capacity(), resampled);
        assert_eq!(f.context.out.capacity(), out);
    }

    fn value(d: &NSDictionary<NSString, AnyObject>, key: &std::ffi::CStr) -> Retained<AnyObject> {
        d.objectForKey(&NSString::from_str(key.to_str().unwrap()))
            .expect("key present")
    }

    #[test]
    fn the_aggregate_runs_without_waiting_for_system_audio_and_is_clocked_by_the_mic() {
        let d = aggregate_description(
            NSString::from_str("mic-uid"),
            NSString::from_str("tap-uid"),
            "aggregate-uid",
        );
        let auto_start = value(&d, kAudioAggregateDeviceTapAutoStartKey)
            .downcast::<NSNumber>()
            .expect("number");
        assert!(!auto_start.boolValue());
        let main = value(&d, kAudioAggregateDeviceMainSubDeviceKey)
            .downcast::<NSString>()
            .expect("string");
        assert_eq!(main.to_string(), "mic-uid");
    }

    /// Needs a microphone and both permissions for the test binary's host app:
    /// `cargo test -p meeting-desktop capture -- --ignored`. Run it with nothing playing, the case
    /// #257 broke, and again while audio plays.
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
