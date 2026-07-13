#[derive(Debug, Clone)]
pub struct DeviceVolumeStatus {
    pub supported: bool,
    pub volume: Option<f32>,
    pub message: Option<String>,
}

impl DeviceVolumeStatus {
    fn supported(volume: f32) -> Self {
        Self {
            supported: true,
            volume: Some(volume.clamp(0.0, 1.0)),
            message: None,
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            supported: false,
            volume: None,
            message: Some(message.into()),
        }
    }
}

pub fn output_device_volume_status(device_name: Option<&str>) -> DeviceVolumeStatus {
    match get_output_device_volume(device_name) {
        Ok(volume) => DeviceVolumeStatus::supported(volume),
        Err(message) => DeviceVolumeStatus::unsupported(message),
    }
}

pub fn get_output_device_volume(device_name: Option<&str>) -> Result<f32, String> {
    platform::get_output_device_volume(device_name)
}

pub fn set_output_device_volume(device_name: Option<&str>, volume: f32) -> Result<(), String> {
    if !volume.is_finite() {
        return Err("Device volume must be finite".to_string());
    }
    platform::set_output_device_volume(device_name, volume.clamp(0.0, 1.0))
}

#[cfg(target_os = "macos")]
mod platform {
    use crate::audio::output::coreaudio_hog::find_device_id_by_name;
    use coreaudio_sys::*;
    use std::{mem, ptr};

    pub fn get_output_device_volume(device_name: Option<&str>) -> Result<f32, String> {
        let device_id = resolve_device_id(device_name)?;
        unsafe {
            read_service_volume(device_id)
                .or_else(|| read_object_volume(device_id, kAudioObjectPropertyElementMaster))
                .or_else(|| read_channel_pair_volume(device_id))
                .map(|volume| volume.clamp(0.0, 1.0))
                .ok_or_else(|| {
                    "Selected CoreAudio output does not expose a device volume".to_string()
                })
        }
    }

    pub fn set_output_device_volume(device_name: Option<&str>, volume: f32) -> Result<(), String> {
        let device_id = resolve_device_id(device_name)?;
        unsafe {
            if write_service_volume(device_id, volume)
                || write_object_volume(device_id, kAudioObjectPropertyElementMaster, volume)
                || write_channel_pair_volume(device_id, volume)
            {
                Ok(())
            } else {
                Err(
                    "Selected CoreAudio output does not expose a settable device volume"
                        .to_string(),
                )
            }
        }
    }

    fn resolve_device_id(device_name: Option<&str>) -> Result<AudioDeviceID, String> {
        match device_name.and_then(|name| {
            let trimmed = name.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        }) {
            Some(name) => find_device_id_by_name(name)
                .ok_or_else(|| format!("CoreAudio output device '{name}' was not found")),
            None => unsafe { default_output_device_id() }
                .ok_or_else(|| "No default CoreAudio output device found".to_string()),
        }
    }

    unsafe fn default_output_device_id() -> Option<AudioDeviceID> {
        let address = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMaster,
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

        (status == 0 && device_id != 0).then_some(device_id)
    }

    unsafe fn service_volume_address() -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: kAudioHardwareServiceDeviceProperty_VirtualMasterVolume,
            mScope: kAudioObjectPropertyScopeOutput,
            mElement: kAudioObjectPropertyElementMaster,
        }
    }

    unsafe fn object_volume_address(
        element: AudioObjectPropertyElement,
    ) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyVolumeScalar,
            mScope: kAudioDevicePropertyScopeOutput,
            mElement: element,
        }
    }

    unsafe fn read_service_volume(device_id: AudioDeviceID) -> Option<f32> {
        let address = unsafe { service_volume_address() };
        if unsafe { AudioHardwareServiceHasProperty(device_id, &address) } == 0 {
            return None;
        }

        let mut volume = 0.0f32;
        let mut size = mem::size_of::<f32>() as u32;
        let status = unsafe {
            AudioHardwareServiceGetPropertyData(
                device_id,
                &address,
                0,
                ptr::null(),
                &mut size,
                &mut volume as *mut _ as *mut libc::c_void,
            )
        };
        (status == 0).then_some(volume)
    }

    unsafe fn write_service_volume(device_id: AudioDeviceID, volume: f32) -> bool {
        let address = unsafe { service_volume_address() };
        if unsafe { AudioHardwareServiceHasProperty(device_id, &address) } == 0 {
            return false;
        }
        let mut settable: Boolean = 0;
        let status =
            unsafe { AudioHardwareServiceIsPropertySettable(device_id, &address, &mut settable) };
        if status != 0 || settable == 0 {
            return false;
        }

        let status = unsafe {
            AudioHardwareServiceSetPropertyData(
                device_id,
                &address,
                0,
                ptr::null(),
                mem::size_of::<f32>() as u32,
                &volume as *const _ as *const libc::c_void,
            )
        };
        status == 0
    }

    unsafe fn read_object_volume(
        device_id: AudioDeviceID,
        element: AudioObjectPropertyElement,
    ) -> Option<f32> {
        let address = unsafe { object_volume_address(element) };
        if unsafe { AudioObjectHasProperty(device_id, &address) } == 0 {
            return None;
        }

        let mut volume = 0.0f32;
        let mut size = mem::size_of::<f32>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                device_id,
                &address,
                0,
                ptr::null(),
                &mut size,
                &mut volume as *mut _ as *mut libc::c_void,
            )
        };
        (status == 0).then_some(volume)
    }

    unsafe fn write_object_volume(
        device_id: AudioDeviceID,
        element: AudioObjectPropertyElement,
        volume: f32,
    ) -> bool {
        let address = unsafe { object_volume_address(element) };
        if unsafe { AudioObjectHasProperty(device_id, &address) } == 0 {
            return false;
        }
        let mut settable: Boolean = 0;
        let status = unsafe { AudioObjectIsPropertySettable(device_id, &address, &mut settable) };
        if status != 0 || settable == 0 {
            return false;
        }

        let status = unsafe {
            AudioObjectSetPropertyData(
                device_id,
                &address,
                0,
                ptr::null(),
                mem::size_of::<f32>() as u32,
                &volume as *const _ as *const libc::c_void,
            )
        };
        status == 0
    }

    unsafe fn read_channel_pair_volume(device_id: AudioDeviceID) -> Option<f32> {
        let mut total = 0.0f32;
        let mut count = 0.0f32;
        for element in [1, 2] {
            if let Some(volume) = unsafe { read_object_volume(device_id, element) } {
                total += volume;
                count += 1.0;
            }
        }
        (count > 0.0).then_some(total / count)
    }

    unsafe fn write_channel_pair_volume(device_id: AudioDeviceID, volume: f32) -> bool {
        let mut wrote = false;
        for element in [1, 2] {
            wrote |= unsafe { write_object_volume(device_id, element, volume) };
        }
        wrote
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use wasapi::{Direction, initialize_mta};
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender,
    };
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows::core::{HSTRING, PCWSTR};

    pub fn get_output_device_volume(device_name: Option<&str>) -> Result<f32, String> {
        let endpoint = endpoint_volume(device_name)?;
        let volume = unsafe { endpoint.GetMasterVolumeLevelScalar() }
            .map_err(|e| format!("WASAPI endpoint volume read failed: {e:?}"))?;
        Ok(volume.clamp(0.0, 1.0))
    }

    pub fn set_output_device_volume(device_name: Option<&str>, volume: f32) -> Result<(), String> {
        if device_name
            .map(|name| name.trim().starts_with("ASIO: "))
            .unwrap_or(false)
        {
            return Err("ASIO drivers do not expose a standard device-volume control".to_string());
        }

        let endpoint = endpoint_volume(device_name)?;
        unsafe {
            endpoint
                .SetMasterVolumeLevelScalar(volume.clamp(0.0, 1.0), std::ptr::null())
                .map_err(|e| format!("WASAPI endpoint volume write failed: {e:?}"))?;
        }
        Ok(())
    }

    fn endpoint_volume(device_name: Option<&str>) -> Result<IAudioEndpointVolume, String> {
        let device = resolve_device(device_name)?;
        unsafe {
            device
                .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
                .map_err(|e| format!("WASAPI endpoint volume activation failed: {e:?}"))
        }
    }

    fn resolve_device(device_name: Option<&str>) -> Result<IMMDevice, String> {
        let hr = initialize_mta();
        if hr.is_err() {
            return Err(format!("CoInitializeEx(MTA) failed: HRESULT {hr:?}"));
        }

        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|e| format!("MMDeviceEnumerator creation failed: {e:?}"))?;

        match device_name.and_then(|name| {
            let trimmed = name.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        }) {
            Some(name) if name.starts_with("ASIO: ") => {
                Err("ASIO drivers do not expose a standard device-volume control".to_string())
            }
            Some(name) => {
                let wasapi_device = wasapi::DeviceEnumerator::new()
                    .and_then(|enumerator| enumerator.get_device_collection(&Direction::Render))
                    .and_then(|collection| collection.get_device_with_name(name))
                    .map_err(|e| format!("WASAPI output device '{name}' was not found: {e:?}"))?;
                let id = wasapi_device
                    .get_id()
                    .map_err(|e| format!("WASAPI output device id read failed: {e:?}"))?;
                let id = HSTRING::from(id.as_str());
                unsafe {
                    enumerator
                        .GetDevice(PCWSTR::from_raw(id.as_ptr()))
                        .map_err(|e| format!("WASAPI output device open failed: {e:?}"))
                }
            }
            None => unsafe {
                enumerator
                    .GetDefaultAudioEndpoint(eRender, eConsole)
                    .map_err(|e| format!("Default WASAPI output device open failed: {e:?}"))
            },
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    pub fn get_output_device_volume(_device_name: Option<&str>) -> Result<f32, String> {
        Err("Device volume is not supported on this platform".to_string())
    }

    pub fn set_output_device_volume(
        _device_name: Option<&str>,
        _volume: f32,
    ) -> Result<(), String> {
        Err("Device volume is not supported on this platform".to_string())
    }
}
