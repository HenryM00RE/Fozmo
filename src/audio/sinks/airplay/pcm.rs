use crate::audio::dsp::dither::{DitherMode, DitherPreference, DitherState, quantize_signed_pcm};

pub fn airplay_dither_mode(preference: DitherPreference) -> DitherMode {
    match preference {
        DitherPreference::Off => DitherMode::Off,
        // AirPlay output is always int16, so Auto picks the noise-shaped
        // 16-bit mode; explicit TPDF remains the flat reference path.
        DitherPreference::Auto => DitherMode::Shaped16,
        DitherPreference::Tpdf => DitherMode::Tpdf,
    }
}

pub fn quantize_interleaved_i16(
    samples: &[f64],
    volume: f64,
    preference: DitherPreference,
    dither_state: &mut DitherState,
    out: &mut Vec<i16>,
) -> (f64, f64) {
    let mode = airplay_dither_mode(preference);
    let sample_count = samples.len() & !1;
    out.clear();
    out.reserve(sample_count);

    let mut max_l = 0.0f64;
    let mut max_r = 0.0f64;
    for (i, sample) in samples.iter().take(sample_count).enumerate() {
        let val = (sample * volume).clamp(-1.0, 1.0);
        if i % 2 == 0 {
            max_l = max_l.max(val.abs());
        } else {
            max_r = max_r.max(val.abs());
        }
        let code = quantize_signed_pcm(val, 16, i % 2, dither_state, mode) as i16;
        out.push(code);
    }

    (max_l, max_r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_dithers_airplay_int16_output() {
        assert_eq!(
            airplay_dither_mode(DitherPreference::Auto),
            DitherMode::Shaped16
        );
        assert_eq!(
            airplay_dither_mode(DitherPreference::Tpdf),
            DitherMode::Tpdf
        );
        assert_eq!(airplay_dither_mode(DitherPreference::Off), DitherMode::Off);
    }

    #[test]
    fn quantized_pcm_clamps_full_scale() {
        let mut dither = DitherState::new(1);
        let mut out = Vec::new();
        quantize_interleaved_i16(
            &[-2.0, 2.0, -1.0, 1.0],
            1.0,
            DitherPreference::Off,
            &mut dither,
            &mut out,
        );
        assert_eq!(out, vec![-32768, 32767, -32768, 32767]);
    }

    #[test]
    fn tpdf_keeps_sub_lsb_signal_from_collapsing() {
        let mut off = DitherState::new(2);
        let mut tpdf = DitherState::new(2);
        let mut out_off = Vec::new();
        let mut out_tpdf = Vec::new();
        let samples = vec![1.0 / 32768.0 / 4.0; 4096];

        quantize_interleaved_i16(&samples, 1.0, DitherPreference::Off, &mut off, &mut out_off);
        quantize_interleaved_i16(
            &samples,
            1.0,
            DitherPreference::Auto,
            &mut tpdf,
            &mut out_tpdf,
        );

        assert!(out_off.iter().all(|sample| *sample == 0));
        assert!(out_tpdf.iter().any(|sample| *sample != 0));
    }
}
