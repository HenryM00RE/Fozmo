#[cfg(all(target_os = "windows", feature = "asio"))]
use crate::audio::asio_output;
#[cfg(target_os = "macos")]
use crate::audio::output::coreaudio_hog::find_device_id_by_name;
use crate::audio::sonos;
use cpal::traits::{DeviceTrait, HostTrait};

pub(crate) fn output_device_available(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    if sonos::is_sonos_device_name(trimmed) {
        return true;
    }

    #[cfg(target_os = "macos")]
    if find_device_id_by_name(trimmed).is_some() {
        return true;
    }

    #[cfg(all(target_os = "windows", feature = "asio"))]
    if let Some(driver_name) = trimmed.strip_prefix("ASIO: ") {
        return asio_output::list_devices()
            .into_iter()
            .any(|driver| driver.trim() == driver_name.trim());
    }

    cpal::default_host()
        .output_devices()
        .map(|devices| {
            devices
                .filter_map(|device| device.name().ok())
                .any(|device| device.trim() == trimmed)
        })
        .unwrap_or(false)
}

pub(crate) fn output_device_names() -> Vec<String> {
    let names: Vec<String> = cpal::default_host()
        .output_devices()
        .map(|devices| devices.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();

    #[cfg(all(target_os = "windows", feature = "asio"))]
    {
        let mut names = names;
        for name in asio_output::list_devices() {
            let name = format!("ASIO: {name}");
            if !names.iter().any(|existing| existing == &name) {
                names.push(name);
            }
        }
        names
    }

    #[cfg(not(all(target_os = "windows", feature = "asio")))]
    {
        names
    }
}
