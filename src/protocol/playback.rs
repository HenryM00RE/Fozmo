use crate::audio::eq::EqConfig;
use crate::settings::DsdSourceRule;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackConfig {
    pub filter_type: String,
    pub target_rate: u32,
    #[serde(default = "default_target_bit_depth")]
    pub target_bit_depth: u32,
    pub upsampling_enabled: bool,
    pub exclusive: bool,
    pub dither_mode: String,
    #[serde(default = "default_output_mode")]
    pub output_mode: String,
    #[serde(default = "default_dsd_modulator")]
    pub dsd_modulator: String,
    #[serde(default)]
    pub dsd_isi_penalty: f32,
    #[serde(default)]
    pub dsd_rules: Vec<DsdSourceRule>,
    #[serde(default)]
    pub headroom_db: f32,
    #[serde(default)]
    pub dsp_buffer_ms: u32,
    pub volume: f32,
    #[serde(default)]
    pub eq: EqConfig,
    #[serde(default)]
    pub output_device: Option<String>,
}

fn default_output_mode() -> String {
    "Pcm".to_string()
}

fn default_target_bit_depth() -> u32 {
    24
}

fn default_dsd_modulator() -> String {
    "EcDepth2".to_string()
}

#[cfg(test)]
mod tests {
    use super::PlaybackConfig;

    #[test]
    fn playback_config_defaults_missing_dsp_buffer_to_auto() {
        let config: PlaybackConfig = serde_json::from_value(serde_json::json!({
            "filter_type": "SincBest",
            "target_rate": 192000,
            "upsampling_enabled": true,
            "exclusive": true,
            "dither_mode": "Auto",
            "volume": 1.0
        }))
        .expect("legacy playback config should deserialize");

        assert_eq!(config.output_mode, "Pcm");
        assert_eq!(config.target_bit_depth, 24);
        assert_eq!(config.dsd_modulator, "EcDepth2");
        assert_eq!(config.dsp_buffer_ms, 0);
    }
}
