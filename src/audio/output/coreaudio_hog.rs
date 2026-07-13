use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use coreaudio_sys::*;
use std::mem;
use std::ptr;

/// Enumerate all audio device IDs currently registered with the macOS CoreAudio system.
///
/// # Safety
///
/// CoreAudio must be initialized for the current process. The returned IDs
/// are snapshots and callers must tolerate devices disappearing afterward.
pub unsafe fn get_all_devices() -> Vec<AudioDeviceID> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: 0, // kAudioObjectPropertyElementMain
    };

    let mut size: u32 = 0;
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
        eprintln!("Failed to get audio devices size, OSStatus: {}", status);
        return Vec::new();
    }

    let count = (size as usize) / mem::size_of::<AudioDeviceID>();
    let mut devices = vec![0; count];

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
        eprintln!("Failed to read audio devices list, OSStatus: {}", status);
        return Vec::new();
    }

    devices
}

/// Retrieve the current CoreAudio default output device ID.
///
/// # Safety
///
/// CoreAudio must be initialized for the current process.
pub unsafe fn get_default_output_device() -> Option<AudioDeviceID> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: 0, // kAudioObjectPropertyElementMain
    };

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

    if status == 0 && device_id != 0 {
        Some(device_id)
    } else {
        eprintln!("Failed to read default output device, OSStatus: {}", status);
        None
    }
}

/// Retrieve the device name of a given AudioDeviceID as a Rust String.
///
/// # Safety
///
/// `device_id` must be an ID obtained from CoreAudio for this process.
pub unsafe fn get_device_name(device_id: AudioDeviceID) -> Option<String> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyDeviceNameCFString,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: 0, // kAudioObjectPropertyElementMain
    };

    let mut cf_str: CFStringRef = ptr::null();
    let mut size = mem::size_of::<CFStringRef>() as u32;

    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            &address,
            0,
            ptr::null(),
            &mut size,
            &mut cf_str as *mut _ as *mut libc::c_void,
        )
    };

    if status == 0 && !cf_str.is_null() {
        let rust_str = unsafe { CFString::wrap_under_create_rule(cf_str) }.to_string();
        Some(rust_str)
    } else {
        None
    }
}

/// Enable or disable Hog Mode (exclusive access) for the given device ID.
/// To enable, we set the property to our current process's PID.
/// To disable, we set the property to -1.
///
/// # Safety
///
/// `device_id` must identify a live CoreAudio output device. The caller is
/// responsible for releasing hog mode when exclusive access is no longer needed.
pub unsafe fn set_hog_mode(device_id: AudioDeviceID, enable: bool) -> Result<(), i32> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyHogMode,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: 0, // kAudioObjectPropertyElementMain
    };

    let mut pid: libc::pid_t = if enable {
        unsafe { libc::getpid() }
    } else {
        -1 // releases the hog mode
    };

    let size = mem::size_of::<libc::pid_t>() as u32;

    let status = unsafe {
        AudioObjectSetPropertyData(
            device_id,
            &address,
            0,
            ptr::null(),
            size,
            &mut pid as *mut _ as *const libc::c_void,
        )
    };

    if status == 0 {
        println!(
            "Successfully {} exclusive Hog Mode on CoreAudio device ID {}",
            if enable { "acquired" } else { "released" },
            device_id
        );
        Ok(())
    } else {
        Err(status)
    }
}

/// Helper function to find the AudioDeviceID of a device by its name.
/// We do a fuzzy or exact string match against all connected devices.
pub fn find_device_id_by_name(target_name: &str) -> Option<AudioDeviceID> {
    unsafe {
        let devices = get_all_devices();
        for dev_id in devices {
            if let Some(name) = get_device_name(dev_id) {
                // Check if device name matches target name
                if name.trim() == target_name.trim() {
                    return Some(dev_id);
                }
            }
        }
    }
    None
}
