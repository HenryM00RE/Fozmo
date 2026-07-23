#[cfg(all(target_os = "windows", feature = "asio"))]
use crate::audio::asio_output;
use crate::audio::device_caps;
use crate::protocol::{OutputDeviceCapabilities, system_audio_backend};

pub(super) fn agent_platform_label() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Mac"
    }
    #[cfg(target_os = "windows")]
    {
        "Windows PC"
    }
    #[cfg(target_os = "linux")]
    {
        "Linux"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        "Device"
    }
}

fn cpal_output_device_names() -> Vec<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    cpal::default_host()
        .output_devices()
        .map(|devices| devices.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

pub(super) fn output_device_capabilities() -> Vec<OutputDeviceCapabilities> {
    let mut devices = Vec::new();
    for name in cpal_output_device_names() {
        if devices
            .iter()
            .any(|caps: &OutputDeviceCapabilities| caps.name == name)
        {
            continue;
        }
        let caps = device_caps::output_device_capabilities(Some(&name));
        devices.push(OutputDeviceCapabilities {
            name,
            backend: Some(system_audio_backend().to_string()),
            max_sample_rate: caps.max_sample_rate,
            max_bit_depth: caps.max_bit_depth,
            supports_dsd128: caps.supports_dsd128,
            supports_dsd256: caps.supports_dsd256,
        });
    }

    #[cfg(all(target_os = "windows", feature = "asio"))]
    for driver in asio_output::list_devices() {
        let name = format!("ASIO: {driver}");
        if devices.iter().any(|caps| caps.name == name) {
            continue;
        }
        let caps = device_caps::output_device_capabilities(Some(&name));
        devices.push(OutputDeviceCapabilities {
            name,
            backend: Some("asio".to_string()),
            max_sample_rate: caps.max_sample_rate,
            max_bit_depth: caps.max_bit_depth,
            supports_dsd128: caps.supports_dsd128,
            supports_dsd256: caps.supports_dsd256,
        });
    }

    devices
}

pub(super) fn log_agent_output_device_summary(devices: &[OutputDeviceCapabilities]) {
    #[cfg(all(target_os = "windows", feature = "asio"))]
    let asio_count = devices
        .iter()
        .filter(|caps| caps.backend.as_deref() == Some("asio"))
        .count();
    #[cfg(all(target_os = "windows", feature = "asio"))]
    {
        println!(
            "AudioWorker: ASIO support enabled; discovered {asio_count} ASIO output device(s)."
        );
    }
    #[cfg(all(target_os = "windows", not(feature = "asio")))]
    {
        println!(
            "AudioWorker: ASIO support is not compiled into this binary; rebuild with `cargo build --release --features asio` to enumerate ASIO drivers."
        );
    }
    println!(
        "AudioWorker: Advertised {} output device(s) to core.",
        devices.len()
    );
}
