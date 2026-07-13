use crate::compat::{DitherPreference, DitherState};

pub fn quantize_interleaved_i16(
    samples: &[f64],
    volume: f64,
    preference: DitherPreference,
    dither_state: &mut DitherState,
    out: &mut Vec<i16>,
) -> (f64, f64) {
    let sample_count = samples.len() & !1;
    out.clear();
    out.reserve(sample_count);
    let mut max_l = 0.0f64;
    let mut max_r = 0.0f64;
    for (index, sample) in samples.iter().take(sample_count).enumerate() {
        let value = (sample * volume).clamp(-1.0, 1.0);
        if index % 2 == 0 {
            max_l = max_l.max(value.abs());
        } else {
            max_r = max_r.max(value.abs());
        }
        let dither = match preference {
            DitherPreference::Off => 0.0,
            DitherPreference::Auto | DitherPreference::Tpdf => dither_state.tpdf(),
        };
        let scaled = value * 32768.0 + dither;
        out.push(scaled.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16);
    }
    (max_l, max_r)
}
