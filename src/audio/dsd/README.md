# DSD

DSD rendering and transport code lives here.

Current shape:

- `dsd_render.rs`: PCM-to-DSD rendering, rate selection, and DSD upsampler
  chains.
- `delta_sigma.rs`: Delta-sigma modulators and noise helpers.
- `dsd_coeffs.rs`: Modulator coefficient data.
- `dop.rs`: DSD-over-PCM packing and idle markers.
- `native_dsd.rs`: Native DSD byte packing and channel ordering.

The audio engine owns output-mode selection and fallback behavior; this module
family owns the DSD transforms and transport packing.
