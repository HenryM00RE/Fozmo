//! CoreAudio FFI helpers for the Fozmo Capture HAL device: device lookup by
//! UID, scalar property reads/writes, and default-output get/set. macOS only.

#![cfg(target_os = "macos")]

use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use coreaudio_sys::{
    AudioDeviceID, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectHasProperty, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
    kAudioDevicePropertyDeviceUID, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMaster, kAudioObjectPropertyName,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
};
use std::{mem, ptr};

const K_AUDIO_DEVICE_PROPERTY_TRANSPORT_TYPE: u32 = 0x7472_616e; // 'tran'
const K_AUDIO_DEVICE_TRANSPORT_TYPE_BUILT_IN: u32 = 0x626c_746e; // 'bltn'
const K_AUDIO_DEVICE_TRANSPORT_TYPE_PCI: u32 = 0x7063_6920; // 'pci '
const K_AUDIO_DEVICE_TRANSPORT_TYPE_USB: u32 = 0x7573_6220; // 'usb '
const K_AUDIO_DEVICE_TRANSPORT_TYPE_FIREWIRE: u32 = 0x3133_3934; // '1394'
const K_AUDIO_DEVICE_TRANSPORT_TYPE_HDMI: u32 = 0x6864_6d69; // 'hdmi'
const K_AUDIO_DEVICE_TRANSPORT_TYPE_DISPLAY_PORT: u32 = 0x6470_7274; // 'dprt'
const K_AUDIO_DEVICE_TRANSPORT_TYPE_THUNDERBOLT: u32 = 0x7468_756e; // 'thun'

pub(super) fn property_address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMaster,
    }
}

fn all_device_ids() -> Vec<AudioDeviceID> {
    let address = property_address(kAudioHardwarePropertyDevices);
    let mut size = 0_u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            kAudioObjectSystemObject,
            &address,
            0,
            ptr::null(),
            &mut size,
        )
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    let count = size as usize / mem::size_of::<AudioDeviceID>();
    let mut devices = vec![0 as AudioDeviceID; count];
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject,
            &address,
            0,
            ptr::null(),
            &mut size,
            devices.as_mut_ptr() as *mut libc::c_void,
        )
    };
    if status != 0 {
        return Vec::new();
    }
    devices
}

pub(super) fn device_uid(device_id: AudioDeviceID) -> Option<String> {
    read_cf_string(device_id, kAudioDevicePropertyDeviceUID)
}

pub(super) fn device_name(device_id: AudioDeviceID) -> Option<String> {
    read_cf_string(device_id, kAudioObjectPropertyName)
}

pub(super) fn device_id_for_uid(uid: &str) -> Option<AudioDeviceID> {
    all_device_ids()
        .into_iter()
        .find(|device_id| device_uid(*device_id).as_deref() == Some(uid))
}

pub(super) fn device_name_for_uid(uid: &str) -> Option<String> {
    device_id_for_uid(uid).and_then(device_name)
}

/// Returns true only when CoreAudio identifies a matching output as a local
/// physical transport. Virtual, aggregate, Bluetooth, AirPlay, and other
/// network devices deliberately fail closed.
pub(super) fn output_device_is_local_physical_by_name(name: &str) -> bool {
    let trimmed = name.trim();
    all_device_ids().into_iter().any(|device_id| {
        device_name(device_id).as_deref().map(str::trim) == Some(trimmed)
            && device_transport_type(device_id).is_some_and(is_local_physical_transport)
    })
}

fn device_transport_type(device_id: AudioDeviceID) -> Option<u32> {
    read_scalar::<u32>(device_id, K_AUDIO_DEVICE_PROPERTY_TRANSPORT_TYPE)
}

fn is_local_physical_transport(transport: u32) -> bool {
    matches!(
        transport,
        K_AUDIO_DEVICE_TRANSPORT_TYPE_BUILT_IN
            | K_AUDIO_DEVICE_TRANSPORT_TYPE_PCI
            | K_AUDIO_DEVICE_TRANSPORT_TYPE_USB
            | K_AUDIO_DEVICE_TRANSPORT_TYPE_FIREWIRE
            | K_AUDIO_DEVICE_TRANSPORT_TYPE_HDMI
            | K_AUDIO_DEVICE_TRANSPORT_TYPE_DISPLAY_PORT
            | K_AUDIO_DEVICE_TRANSPORT_TYPE_THUNDERBOLT
    )
}

pub(super) fn default_output_device_id() -> Option<AudioDeviceID> {
    let address = property_address(kAudioHardwarePropertyDefaultOutputDevice);
    let mut device_id: AudioDeviceID = 0;
    let mut size = mem::size_of::<AudioDeviceID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject,
            &address,
            0,
            ptr::null(),
            &mut size,
            &mut device_id as *mut _ as *mut libc::c_void,
        )
    };
    (status == 0 && device_id != 0).then_some(device_id)
}

pub(super) fn default_output_device_uid() -> Option<String> {
    default_output_device_id().and_then(device_uid)
}

pub(super) fn set_default_output_device(device_id: AudioDeviceID) -> Result<(), String> {
    let address = property_address(kAudioHardwarePropertyDefaultOutputDevice);
    let size = mem::size_of::<AudioDeviceID>() as u32;
    let status = unsafe {
        AudioObjectSetPropertyData(
            kAudioObjectSystemObject,
            &address,
            0,
            ptr::null(),
            size,
            &device_id as *const _ as *const libc::c_void,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "CoreAudio refused to change the default output device (status {status})."
        ))
    }
}

pub(super) fn read_u32(device_id: AudioDeviceID, selector: u32) -> Option<u32> {
    read_scalar::<u32>(device_id, selector)
}

pub(super) fn read_u64(device_id: AudioDeviceID, selector: u32) -> Option<u64> {
    read_scalar::<u64>(device_id, selector)
}

pub(super) fn read_f64(device_id: AudioDeviceID, selector: u32) -> Option<f64> {
    read_scalar::<f64>(device_id, selector)
}

pub(super) fn read_scalar<T: Copy>(device_id: AudioDeviceID, selector: u32) -> Option<T> {
    let address = property_address(selector);
    if unsafe { AudioObjectHasProperty(device_id, &address) } == 0 {
        return None;
    }
    let mut value = unsafe { mem::zeroed::<T>() };
    let mut size = mem::size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            &address,
            0,
            ptr::null(),
            &mut size,
            &mut value as *mut _ as *mut libc::c_void,
        )
    };
    (status == 0).then_some(value)
}

pub(super) fn write_scalar<T: Copy>(
    device_id: AudioDeviceID,
    selector: u32,
    value: T,
) -> Result<(), String> {
    let address = property_address(selector);
    let status = unsafe {
        AudioObjectSetPropertyData(
            device_id,
            &address,
            0,
            ptr::null(),
            mem::size_of::<T>() as u32,
            &value as *const _ as *const libc::c_void,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "CoreAudio property write failed (status {status})."
        ))
    }
}

pub(super) fn read_cf_string(device_id: AudioDeviceID, selector: u32) -> Option<String> {
    let address = property_address(selector);
    if unsafe { AudioObjectHasProperty(device_id, &address) } == 0 {
        return None;
    }
    let mut cf_value: CFStringRef = ptr::null();
    let mut size = mem::size_of::<CFStringRef>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            &address,
            0,
            ptr::null(),
            &mut size,
            &mut cf_value as *mut _ as *mut libc::c_void,
        )
    };
    if status == 0 && !cf_value.is_null() {
        Some(unsafe { CFString::wrap_under_create_rule(cf_value) }.to_string())
    } else {
        None
    }
}
