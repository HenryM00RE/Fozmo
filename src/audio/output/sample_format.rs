// Output backends share this conversion module while platform transports are still feature staged.
#![allow(dead_code)]

use crate::audio::dsp::dither::{DitherMode, DitherPreference, DitherState, quantize_signed_pcm};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSampleType {
    Float,
    Int,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputSampleFormat {
    pub sample_type: OutputSampleType,
    pub valid_bits: usize,
    pub bytes_per_sample: usize,
    pub channels: usize,
}

impl OutputSampleFormat {
    pub fn dither_mode(self, preference: DitherPreference) -> DitherMode {
        match preference {
            DitherPreference::Off => DitherMode::Off,
            // Auto uses noise shaping only where the endpoint is truly
            // 16-bit; 24-bit narrowing keeps flat TPDF.
            DitherPreference::Auto => {
                match (self.sample_type, self.valid_bits, self.bytes_per_sample) {
                    (OutputSampleType::Int, 16, 2) => DitherMode::Shaped16,
                    (OutputSampleType::Int, 24, 3 | 4) => DitherMode::Tpdf,
                    _ => DitherMode::Off,
                }
            }
            DitherPreference::Tpdf => {
                match (self.sample_type, self.valid_bits, self.bytes_per_sample) {
                    (OutputSampleType::Int, 16, 2) | (OutputSampleType::Int, 24, 3 | 4) => {
                        DitherMode::Tpdf
                    }
                    _ => DitherMode::Off,
                }
            }
        }
    }
}

/// Convert interleaved f64 samples into a local hardware output wire format.
pub fn encode_interleaved_f64(
    src_f64: &[f64],
    dst: &mut [u8],
    format: OutputSampleFormat,
    preference: DitherPreference,
    dither_state: &mut DitherState,
) {
    let mode = format.dither_mode(preference);
    match (
        format.sample_type,
        format.valid_bits,
        format.bytes_per_sample,
    ) {
        (OutputSampleType::Float, 32, 4) => {
            for (i, sample) in src_f64.iter().enumerate() {
                let bytes = (*sample as f32).to_le_bytes();
                dst[i * 4..i * 4 + 4].copy_from_slice(&bytes);
            }
        }
        (OutputSampleType::Int, 32, 4) => {
            for (i, sample) in src_f64.iter().enumerate() {
                let value =
                    quantize_signed_pcm(*sample, 32, i % format.channels, dither_state, mode);
                dst[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        (OutputSampleType::Int, 24, 4) => {
            for (i, sample) in src_f64.iter().enumerate() {
                let value_24 =
                    quantize_signed_pcm(*sample, 24, i % format.channels, dither_state, mode);
                let value = value_24 << 8;
                dst[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        (OutputSampleType::Int, 24, 3) => {
            for (i, sample) in src_f64.iter().enumerate() {
                let value_24 =
                    quantize_signed_pcm(*sample, 24, i % format.channels, dither_state, mode);
                let bytes = value_24.to_le_bytes();
                dst[i * 3] = bytes[0];
                dst[i * 3 + 1] = bytes[1];
                dst[i * 3 + 2] = bytes[2];
            }
        }
        (OutputSampleType::Int, 16, 2) => {
            for (i, sample) in src_f64.iter().enumerate() {
                let value =
                    quantize_signed_pcm(*sample, 16, i % format.channels, dither_state, mode)
                        as i16;
                dst[i * 2..i * 2 + 2].copy_from_slice(&value.to_le_bytes());
            }
        }
        _ => dst.fill(0),
    }
}

/// Convert pre-packed interleaved i32 samples into a local hardware output wire
/// format without scaling or dither. This is used by DoP, where the marker and
/// payload bits must survive byte-for-byte.
pub fn encode_interleaved_i32_passthrough(
    src_i32: &[i32],
    dst: &mut [u8],
    format: OutputSampleFormat,
) {
    match (
        format.sample_type,
        format.valid_bits,
        format.bytes_per_sample,
    ) {
        (OutputSampleType::Int, 32, 4) | (OutputSampleType::Int, 24, 4) => {
            for (i, sample) in src_i32.iter().enumerate() {
                dst[i * 4..i * 4 + 4].copy_from_slice(&sample.to_le_bytes());
            }
        }
        (OutputSampleType::Int, 24, 3) => {
            for (i, sample) in src_i32.iter().enumerate() {
                let bytes = (sample >> 8).to_le_bytes();
                dst[i * 3] = bytes[0];
                dst[i * 3 + 1] = bytes[1];
                dst[i * 3 + 2] = bytes[2];
            }
        }
        _ => dst.fill(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(samples: &[f64], format: OutputSampleFormat, seed: u64) -> Vec<u8> {
        let mut dst = vec![0u8; samples.len() * format.bytes_per_sample];
        let mut dither = DitherState::new(seed);
        encode_interleaved_f64(
            samples,
            &mut dst,
            format,
            DitherPreference::Auto,
            &mut dither,
        );
        dst
    }

    #[test]
    fn f32_output_is_little_endian_float_bytes() {
        let samples = [0.0f64, -0.5, 0.25, 1.0];
        let format = OutputSampleFormat {
            sample_type: OutputSampleType::Float,
            valid_bits: 32,
            bytes_per_sample: 4,
            channels: 2,
        };

        let dst = encode(&samples, format, 1);

        for (idx, sample) in samples.iter().enumerate() {
            assert_eq!(&dst[idx * 4..idx * 4 + 4], &(*sample as f32).to_le_bytes());
        }
    }

    #[test]
    fn int16_output_clamps_full_scale_without_wrap() {
        let format = OutputSampleFormat {
            sample_type: OutputSampleType::Int,
            valid_bits: 16,
            bytes_per_sample: 2,
            channels: 2,
        };

        let dst = encode(&[-1.0, 1.0], format, 2);

        assert_eq!(i16::from_le_bytes([dst[0], dst[1]]), -32768);
        assert_eq!(i16::from_le_bytes([dst[2], dst[3]]), 32767);
    }

    #[test]
    fn int24_packed_layout_preserves_sign_bytes() {
        let format = OutputSampleFormat {
            sample_type: OutputSampleType::Int,
            valid_bits: 24,
            bytes_per_sample: 3,
            channels: 2,
        };

        let dst = encode(&[-1.0, 1.0], format, 3);

        assert_eq!(&dst[0..3], &[0x00, 0x00, 0x80]);
        assert_eq!(&dst[3..6], &[0xff, 0xff, 0x7f]);
    }

    #[test]
    fn int24_in_32_bit_container_is_left_justified() {
        let format = OutputSampleFormat {
            sample_type: OutputSampleType::Int,
            valid_bits: 24,
            bytes_per_sample: 4,
            channels: 2,
        };

        let dst = encode(&[-1.0, 1.0], format, 4);

        assert_eq!(&dst[0..4], &[0x00, 0x00, 0x00, 0x80]);
        assert_eq!(&dst[4..8], &[0x00, 0xff, 0xff, 0x7f]);
    }

    #[test]
    fn int32_output_uses_full_signed_range_without_dither() {
        let format = OutputSampleFormat {
            sample_type: OutputSampleType::Int,
            valid_bits: 32,
            bytes_per_sample: 4,
            channels: 2,
        };

        let dst = encode(&[-1.0, 0.0, 1.0], format, 5);

        assert_eq!(
            i32::from_le_bytes([dst[0], dst[1], dst[2], dst[3]]),
            i32::MIN
        );
        assert_eq!(i32::from_le_bytes([dst[4], dst[5], dst[6], dst[7]]), 0);
        assert_eq!(
            i32::from_le_bytes([dst[8], dst[9], dst[10], dst[11]]),
            i32::MAX
        );
    }

    #[test]
    fn unknown_format_writes_silence() {
        let samples = [1.0f64, -1.0];
        let format = OutputSampleFormat {
            sample_type: OutputSampleType::Int,
            valid_bits: 20,
            bytes_per_sample: 3,
            channels: 2,
        };
        let mut dst = vec![0x55; samples.len() * format.bytes_per_sample];
        let mut dither = DitherState::new(6);

        encode_interleaved_f64(
            &samples,
            &mut dst,
            format,
            DitherPreference::Auto,
            &mut dither,
        );

        assert!(dst.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn i32_passthrough_preserves_24_in_32_layout() {
        let samples = [0x0569_6900_i32, 0x7fff_ff00_i32];
        let format = OutputSampleFormat {
            sample_type: OutputSampleType::Int,
            valid_bits: 24,
            bytes_per_sample: 4,
            channels: 2,
        };
        let mut dst = vec![0; samples.len() * format.bytes_per_sample];

        encode_interleaved_i32_passthrough(&samples, &mut dst, format);

        assert_eq!(&dst[0..4], &samples[0].to_le_bytes());
        assert_eq!(&dst[4..8], &samples[1].to_le_bytes());
    }

    #[test]
    fn i32_passthrough_packs_left_justified_24_bit_values() {
        let samples = [0x0569_6900_i32, 0x7fff_ff00_i32];
        let format = OutputSampleFormat {
            sample_type: OutputSampleType::Int,
            valid_bits: 24,
            bytes_per_sample: 3,
            channels: 2,
        };
        let mut dst = vec![0; samples.len() * format.bytes_per_sample];

        encode_interleaved_i32_passthrough(&samples, &mut dst, format);

        assert_eq!(&dst[0..3], &[0x69, 0x69, 0x05]);
        assert_eq!(&dst[3..6], &[0xff, 0xff, 0x7f]);
    }

    #[test]
    fn i32_passthrough_unknown_format_writes_silence() {
        let samples = [0x0569_6900_i32];
        let format = OutputSampleFormat {
            sample_type: OutputSampleType::Float,
            valid_bits: 32,
            bytes_per_sample: 4,
            channels: 2,
        };
        let mut dst = vec![0x55; samples.len() * format.bytes_per_sample];

        encode_interleaved_i32_passthrough(&samples, &mut dst, format);

        assert!(dst.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn auto_dither_only_targets_integer_narrowing_formats() {
        let int16 = OutputSampleFormat {
            sample_type: OutputSampleType::Int,
            valid_bits: 16,
            bytes_per_sample: 2,
            channels: 2,
        };
        assert_eq!(
            int16.dither_mode(DitherPreference::Auto),
            DitherMode::Shaped16
        );
        assert_eq!(int16.dither_mode(DitherPreference::Tpdf), DitherMode::Tpdf);
        assert_eq!(int16.dither_mode(DitherPreference::Off), DitherMode::Off);

        for format in [
            OutputSampleFormat {
                sample_type: OutputSampleType::Int,
                valid_bits: 24,
                bytes_per_sample: 3,
                channels: 2,
            },
            OutputSampleFormat {
                sample_type: OutputSampleType::Int,
                valid_bits: 24,
                bytes_per_sample: 4,
                channels: 2,
            },
        ] {
            assert_eq!(format.dither_mode(DitherPreference::Auto), DitherMode::Tpdf);
            assert_eq!(format.dither_mode(DitherPreference::Tpdf), DitherMode::Tpdf);
            assert_eq!(format.dither_mode(DitherPreference::Off), DitherMode::Off);
        }

        for format in [
            OutputSampleFormat {
                sample_type: OutputSampleType::Float,
                valid_bits: 32,
                bytes_per_sample: 4,
                channels: 2,
            },
            OutputSampleFormat {
                sample_type: OutputSampleType::Int,
                valid_bits: 32,
                bytes_per_sample: 4,
                channels: 2,
            },
        ] {
            assert_eq!(format.dither_mode(DitherPreference::Auto), DitherMode::Off);
            assert_eq!(format.dither_mode(DitherPreference::Tpdf), DitherMode::Off);
            assert_eq!(format.dither_mode(DitherPreference::Off), DitherMode::Off);
        }
    }
}
