# Local source patch

Upstream: <https://github.com/Pabldi08/airplay2-rs>

Pinned revision: `1baeaae336ca3a9828e732500082f5fd1767d2fd`

The upstream `airplay-audio` crate required FDK AAC unconditionally even when
only the ALAC live path was used. This preserved source copy makes `fdk-aac`
optional behind an `aac` feature, defaults that feature off, conditionally
compiles `AacEncoder`, and returns an unsupported-format error if an AAC format
is requested without it. No FDK code is needed for Fozmo's ALAC AirPlay 2 path.

