//! Read-only queries of the default audio devices through CoreAudio.

use std::ffi::c_void;
use std::mem::{MaybeUninit, size_of};
use std::ptr::{NonNull, null};

use objc2_core_audio::{
    AudioDeviceID, AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectPropertyScope, AudioObjectPropertySelector, kAudioDevicePropertyDataSource,
    kAudioDevicePropertyMute, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyTransportType, kAudioDeviceTransportTypeBluetooth,
    kAudioDeviceTransportTypeBluetoothLE, kAudioDeviceTransportTypeBuiltIn,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
};
use objc2_core_foundation::{CFRetained, CFString};

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

// Values from the AudioToolbox headers, not exported by objc2-core-audio.
const VIRTUAL_MAIN_VOLUME: AudioObjectPropertySelector = fourcc(b"vmvc");
const SOURCE_INTERNAL_SPEAKER: u32 = fourcc(b"ispk");
const SOURCE_HEADPHONES: u32 = fourcc(b"hdpn");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    BuiltInSpeakers,
    Headphones,
    Bluetooth,
    Unknown,
}

impl OutputKind {
    pub fn label(self) -> &'static str {
        match self {
            OutputKind::BuiltInSpeakers => "parlantes integrados",
            OutputKind::Headphones => "auriculares con cable",
            OutputKind::Bluetooth => "Bluetooth",
            OutputKind::Unknown => "externa o desconocida",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: AudioDeviceID,
    pub name: String,
    pub sample_rate: f64,
    pub volume: Option<f32>,
    pub muted: Option<bool>,
    pub output_kind: OutputKind,
}

fn get<T: Copy>(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Option<T> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut value = MaybeUninit::<T>::uninit();
    let mut size = size_of::<T>() as u32;
    // SAFETY: address, size and value all outlive the call, and size matches the
    // storage. CoreAudio writes at most `size` bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(value.as_mut_ptr().cast::<c_void>()),
        )
    };
    // SAFETY: a zero status with the full size means T was written.
    (status == 0 && size as usize == size_of::<T>()).then(|| unsafe { value.assume_init() })
}

fn name_of(device: AudioDeviceID) -> Option<String> {
    let raw: *const CFString = get(
        device,
        kAudioObjectPropertyName,
        kAudioObjectPropertyScopeGlobal,
    )?;
    let raw = NonNull::new(raw.cast_mut())?;
    // SAFETY: this property hands the caller an owned (+1) CFString.
    let name = unsafe { CFRetained::from_raw(raw) };
    Some(name.to_string())
}

fn read(selector: AudioObjectPropertySelector, scope: AudioObjectPropertyScope) -> Option<Device> {
    let id: AudioDeviceID = get(
        kAudioObjectSystemObject as AudioObjectID,
        selector,
        kAudioObjectPropertyScopeGlobal,
    )?;
    let transport: u32 = get(
        id,
        kAudioDevicePropertyTransportType,
        kAudioObjectPropertyScopeGlobal,
    )
    .unwrap_or(0);
    let source: Option<u32> = get(id, kAudioDevicePropertyDataSource, scope);
    let output_kind = if transport == kAudioDeviceTransportTypeBluetooth
        || transport == kAudioDeviceTransportTypeBluetoothLE
    {
        OutputKind::Bluetooth
    } else if source == Some(SOURCE_HEADPHONES) {
        OutputKind::Headphones
    } else if transport == kAudioDeviceTransportTypeBuiltIn
        && source == Some(SOURCE_INTERNAL_SPEAKER)
    {
        OutputKind::BuiltInSpeakers
    } else {
        OutputKind::Unknown
    };
    Some(Device {
        id,
        name: name_of(id).unwrap_or_else(|| "(sin nombre)".to_owned()),
        sample_rate: get(
            id,
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
        )
        .unwrap_or(0.0),
        volume: get(id, VIRTUAL_MAIN_VOLUME, scope),
        muted: get::<u32>(id, kAudioDevicePropertyMute, scope).map(|m| m != 0),
        output_kind,
    })
}

pub fn default_output() -> Option<Device> {
    read(
        kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyScopeOutput,
    )
}

pub fn default_input() -> Option<Device> {
    read(
        kAudioHardwarePropertyDefaultInputDevice,
        kAudioObjectPropertyScopeInput,
    )
}
