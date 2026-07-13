# DSP

PCM digital signal processing code lives here.

Current shape:

- `resampler.rs`: Resampling filters, planners, cascades, and tests.
- `eq.rs`: Parametric EQ model, processor, coefficient ramping, and response
  helpers.
- `dither.rs`: Dither modes and PCM quantization support.

The audio engine integrates these modules when rendering PCM, preparing network
sink transports, and feeding DSD conversion.
