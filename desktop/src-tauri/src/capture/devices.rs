//! Thin Core Audio property access: default devices, UIDs, stream layouts, rates and the
//! default-device listeners. Errors name the stage and the OSStatus only, never a device name or
//! UID.

use std::ffi::c_void;
use std::mem::{self, MaybeUninit};
use std::ptr::{self, NonNull};
use std::sync::mpsc::Sender;

use anyhow::{Context, anyhow, bail};
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertyScope,
    AudioObjectPropertySelector, AudioObjectRemovePropertyListener, AudioObjectSetPropertyData,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyStreamConfiguration, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectSystemObject, kAudioObjectUnknown,
};
use objc2_foundation::NSString;

use super::rebuild::Msg;

const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

/// `AudioBuffer` from CoreAudioTypes, declared here because its binding sits behind a cargo
/// feature that pulls in a crate D22 did not approve.
#[repr(C)]
pub struct AudioBuffer {
    pub channels: u32,
    pub bytes: u32,
    pub data: *mut c_void,
}

/// `AudioBufferList`: a count followed by that many `AudioBuffer`s.
#[repr(C)]
pub struct AudioBufferList {
    pub count: u32,
    pub first: [AudioBuffer; 1],
}

impl AudioBufferList {
    /// # Safety
    /// `list` must point to a valid `AudioBufferList` holding `count` buffers that outlives `'a`.
    pub unsafe fn buffers<'a>(list: *const AudioBufferList) -> &'a [AudioBuffer] {
        // SAFETY: the caller guarantees `count` buffers follow the header.
        unsafe {
            let count = (*list).count as usize;
            std::slice::from_raw_parts(ptr::addr_of!((*list).first).cast::<AudioBuffer>(), count)
        }
    }
}

/// `AudioStreamBasicDescription`, for the same reason as `AudioBuffer`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct StreamFormat {
    pub sample_rate: f64,
    pub format_id: u32,
    pub format_flags: u32,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
    pub bytes_per_frame: u32,
    pub channels: u32,
    pub bits_per_channel: u32,
    pub reserved: u32,
}

pub fn address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

pub fn status(stage: &str, code: i32) -> anyhow::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(anyhow!("Core Audio {stage} failed (OSStatus {code})"))
    }
}

/// Reads a fixed-size property. `T` must be the plain C type Core Audio stores for it.
pub fn get<T: Copy>(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
    qualifier: Option<&[u8]>,
    stage: &str,
) -> anyhow::Result<T> {
    let mut value = MaybeUninit::<T>::uninit();
    let mut size = mem::size_of::<T>() as u32;
    let (q_size, q_ptr) = qualifier.map_or((0, ptr::null()), |q| (q.len() as u32, q.as_ptr()));
    // SAFETY: every pointer is valid for the call; `size` is the size of `value`, and Core Audio
    // writes at most that many bytes.
    let code = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            q_size,
            q_ptr.cast(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(value.as_mut_ptr().cast()),
        )
    };
    status(stage, code)?;
    if size as usize != mem::size_of::<T>() {
        bail!(
            "Core Audio {stage} returned {size} bytes instead of {}",
            mem::size_of::<T>()
        );
    }
    // SAFETY: Core Audio filled all `size_of::<T>()` bytes (checked above).
    Ok(unsafe { value.assume_init() })
}

pub fn set<T: Copy>(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
    value: &T,
    stage: &str,
) -> anyhow::Result<()> {
    // SAFETY: `value` is valid for `size_of::<T>()` bytes for the duration of the call.
    let code = unsafe {
        AudioObjectSetPropertyData(
            object,
            NonNull::from(&address),
            0,
            ptr::null(),
            mem::size_of::<T>() as u32,
            NonNull::from(value).cast(),
        )
    };
    status(stage, code)
}

fn default_device(
    selector: AudioObjectPropertySelector,
    stage: &str,
) -> anyhow::Result<AudioObjectID> {
    get(
        SYSTEM,
        address(selector, kAudioObjectPropertyScopeGlobal),
        None,
        stage,
    )
}

pub fn default_input() -> anyhow::Result<AudioObjectID> {
    let id = default_device(
        kAudioHardwarePropertyDefaultInputDevice,
        "read default input device",
    )?;
    if id == kAudioObjectUnknown {
        bail!("No input device; connect a microphone and try again");
    }
    Ok(id)
}

/// The device's persistent UID, needed to name it in the aggregate device description.
pub fn device_uid(id: AudioObjectID) -> anyhow::Result<Retained<NSString>> {
    let raw: *mut NSString = get(
        id,
        address(
            kAudioDevicePropertyDeviceUID,
            kAudioObjectPropertyScopeGlobal,
        ),
        None,
        "read device UID",
    )?;
    // SAFETY: the property returns a +1 CFStringRef, toll-free bridged to NSString; taking it as
    // `Retained` balances that retain.
    unsafe { Retained::from_raw(raw) }.context("Core Audio read device UID returned nothing")
}

/// This process as Core Audio knows it, so the tap leaves out the app's own sounds. `None` while
/// the app has not touched audio output yet.
pub fn own_process_object() -> Option<AudioObjectID> {
    let pid = std::process::id() as i32;
    let id: AudioObjectID = get(
        SYSTEM,
        address(
            kAudioHardwarePropertyTranslatePIDToProcessObject,
            kAudioObjectPropertyScopeGlobal,
        ),
        Some(&pid.to_ne_bytes()),
        "translate pid",
    )
    .ok()?;
    (id != kAudioObjectUnknown).then_some(id)
}

/// The channel count of each input buffer the device delivers, in IOProc order.
pub fn input_channels(id: AudioObjectID, stage: &str) -> anyhow::Result<Vec<u32>> {
    let address = address(
        kAudioDevicePropertyStreamConfiguration,
        kAudioObjectPropertyScopeInput,
    );
    let mut size = 0u32;
    // SAFETY: valid pointers for the call.
    let code = unsafe {
        AudioObjectGetPropertyDataSize(
            id,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
        )
    };
    status(stage, code)?;
    // u64 words keep the buffer aligned for the pointers inside `AudioBuffer`.
    let mut storage = vec![0u64; (size as usize).div_ceil(8).max(1)];
    // SAFETY: `storage` holds at least `size` bytes.
    let code = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(storage.as_mut_ptr().cast()),
        )
    };
    status(stage, code)?;
    if (size as usize) < mem::size_of::<u32>() {
        return Ok(Vec::new());
    }
    let list = storage.as_ptr().cast::<AudioBufferList>();
    // SAFETY: Core Audio wrote a complete AudioBufferList into `storage`, which outlives `buffers`.
    let buffers = unsafe { AudioBufferList::buffers(list) };
    Ok(buffers.iter().map(|b| b.channels).collect())
}

pub fn nominal_rate(id: AudioObjectID) -> anyhow::Result<f64> {
    get(
        id,
        address(
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
        ),
        None,
        "read sample rate",
    )
}

pub fn set_nominal_rate(id: AudioObjectID, rate: f64) -> anyhow::Result<()> {
    set(
        id,
        address(
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
        ),
        &rate,
        "set sample rate",
    )
}

const WATCHED: [AudioObjectPropertySelector; 2] = [
    kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice,
];

/// Default-device listeners; dropping it removes them.
pub struct Listeners {
    tx: *mut Sender<Msg>,
    added: Vec<AudioObjectPropertySelector>,
}

// SAFETY: the raw pointer is a leaked Box (see `Drop`); the Sender it points to is Send + Sync.
unsafe impl Send for Listeners {}

/// Sends `Msg::Changed` whenever the default input or output device changes.
pub fn listen(tx: Sender<Msg>) -> anyhow::Result<Listeners> {
    let mut listeners = Listeners {
        tx: Box::into_raw(Box::new(tx)),
        added: Vec::new(),
    };
    for selector in WATCHED {
        let address = address(selector, kAudioObjectPropertyScopeGlobal);
        // SAFETY: `on_change` matches the listener signature; `tx` is never freed (see
        // `Drop`).
        let code = unsafe {
            AudioObjectAddPropertyListener(
                SYSTEM,
                NonNull::from(&address),
                Some(on_change),
                listeners.tx.cast(),
            )
        };
        status("add device listener", code)?;
        listeners.added.push(selector);
    }
    Ok(listeners)
}

/// Runs on a Core Audio notification thread: it only sends a message.
unsafe extern "C-unwind" fn on_change(
    _object: AudioObjectID,
    _count: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    data: *mut c_void,
) -> i32 {
    // SAFETY: `data` is the `Sender` registered in `listen`, never freed.
    let tx = unsafe { &*data.cast::<Sender<Msg>>() };
    // The worker may have exited; a closed channel only means nobody listens any more.
    let _ = tx.send(Msg::Changed);
    0
}

impl Drop for Listeners {
    fn drop(&mut self) {
        for &selector in &self.added {
            let address = address(selector, kAudioObjectPropertyScopeGlobal);
            // SAFETY: the same object, address, proc and client data as when it was added.
            let code = unsafe {
                AudioObjectRemovePropertyListener(
                    SYSTEM,
                    NonNull::from(&address),
                    Some(on_change),
                    self.tx.cast(),
                )
            };
            if let Err(e) = status("remove device listener", code) {
                tracing::warn!("{e}");
            }
        }
        // The boxed `Sender` is leaked on purpose (one per recording): Core Audio does not
        // document that removing a listener waits for a call already running on its thread, so
        // freeing it here could let that call read freed memory. Its receiver is gone by now, so
        // a late call only fails to send.
    }
}
