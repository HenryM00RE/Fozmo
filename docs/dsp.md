# DSP

Digital signal processing (DSP) shapes your audio before it reaches the output. It can adjust the sound, change the sample rate, or prepare a DSD stream — and all of it is optional. If you just want to press play, Fozmo works fine without touching any of this.

## EQ

Fozmo includes a ten-band EQ with a preamp, useful for taming a room, a pair of speakers, or headphones.

Each band controls where the change happens, how strong it is, and how wide it spreads. Small moves usually work best. And if you boost anything, lower the preamp a little so loud peaks have room to breathe and don't clip.

## Upsampling

When the output supports it, Fozmo can send higher-rate PCM or convert PCM to DSD. Upsampling doesn't restore detail that was never in the recording — it just prepares the same audio in a different way.

The three filters differ mainly in phase behaviour and where ringing — a small ripple around a sharp transient — ends up:

- **Linear Phase** keeps all frequencies aligned in time. Its response is symmetrical, so ringing can appear both before and after a transient.
- **Minimum Phase** concentrates its response after the transient. That avoids most pre-ringing, but shifts the relative timing of different frequencies.
- **Split Phase** stays linear at low frequencies and eases towards minimum phase up high, preserving low-frequency timing while reducing high-frequency pre-ringing.

Think of these as different timing trade-offs rather than quality levels. Split Phase is the default.

Higher rates take more processing power, and bigger numbers aren't automatically better. If playback stutters, step down a rate or return to PCM.

## DSD

There are two DSD modes:

- **7th Order** is the lighter option, supporting DSD64, DSD128, and DSD256.
- **7th Order Search** works at DSD64 and DSD128, and is designed for higher-end CPUs — think Apple M4-series Macs or a Windows PC with an AMD Ryzen 7 9800X3D.

Fozmo applies the recommended headroom for each mode automatically, so the main decision is simply which one runs reliably on your system.

## Current setup

I use **Split Phase + 7th Order Search at DSD128**. That's a personal preference rather than a universal recommendation — give the other combinations a listen and see what suits your setup.

## Where to start

PCM offers the broadest compatibility. If your DAC handles DSD, DSD128 is a practical starting point. And if playback isn't stable, drop to a lower DSD rate, switch to 7th Order, or fall back to PCM.
