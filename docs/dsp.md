# DSP

Digital signal processing (DSP) adjusts audio before output. It can shape the
sound, change the sample rate, or prepare DSD output. All processing is
optional.

## EQ

Fozmo has a ten-band EQ and a preamp for correcting room, speaker, or headphone
response.

Each band controls where the change happens, how strong it is and how wide it
spreads. Small moves usually work best. If you boost anything, lower the
preamp a little so loud peaks have room and do not clip.

## Upsampling

Fozmo can send higher-rate PCM or convert PCM to DSD when the output supports
it. Upsampling does not restore detail that is missing from a recording; it
just prepares the same audio in a different way.

The three filters differ mainly in phase and where ringing—a small ripple
around a sharp transient—appears:

- **Linear Phase** keeps frequencies aligned in time. Its response is
  symmetrical, so ringing can appear both before and after a transient.
- **Minimum Phase** concentrates its response after the transient. This avoids
  most pre-ringing but changes the relative timing of different frequencies.
- **Split Phase** stays linear at low frequencies and moves towards minimum
  phase at high frequencies. This preserves low-frequency timing while
  reducing high-frequency pre-ringing.

These are different timing tradeoffs rather than quality levels. Split Phase
is the default.

Higher rates use more processing power and are not automatically better. If
playback stutters, step down a rate or return to PCM.

## DSD

There are two DSD modes:

- **7th Order** uses less CPU and supports DSD64, DSD128 and DSD256.
- **7th Order Search** works at DSD64 and DSD128. It is designed for higher-end
  CPUs, such as Apple M4-series Macs or a Windows PC with an AMD Ryzen 7
  9800X3D.

Fozmo applies the recommended headroom for each mode. Choose the mode that
runs reliably on your system.

## Current setup

I use **Split Phase + 7th Order Search at DSD128**. This is a personal
preference rather than a universal recommendation.

## Where to start

PCM offers broad compatibility. For a DSD-capable DAC, DSD128 is a practical
starting point. If playback is not stable, use a lower DSD rate, 7th Order, or
PCM.
