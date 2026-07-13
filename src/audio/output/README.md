# Output

Local hardware output backends and device controls live here.

Current shape:

- `device_caps.rs`: Device capability probing and output-rate policy.
- `device_volume.rs`: Platform-specific device volume support.
- `sample_format.rs`: PCM sample-format packing and conversion.
- `coreaudio_hog.rs`: macOS CoreAudio hog-mode helper.
- `wasapi_exclusive.rs`: Windows WASAPI exclusive output.
- `asio_output.rs`: Windows ASIO and native DSD output.

The audio engine selects and drives these backends through its output session
modules.
