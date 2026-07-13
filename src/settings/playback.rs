use super::dsd::DsdSourceRule;
use super::model::PersistedSettings;
use crate::audio::eq::EqConfig;
use crate::audio::resampler::FilterType;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZonePlaybackSettings {
    pub filter_type: Option<String>,
    /// `None` or `Some(0)` both mean "Auto Best".
    pub target_rate: Option<u32>,
    /// Target PCM bit depth for rendered/transported DSP output.
    pub target_bit_depth: Option<u32>,
    pub upsampling_enabled: Option<bool>,
    pub exclusive: Option<bool>,
    pub dither_mode: Option<String>,
    /// "Pcm" (default), "Dsd64", "Dsd128", or "Dsd256".
    pub output_mode: Option<String>,
    /// Supported values are "Standard", "EcDepth2", "EcBeam", and
    /// "EcBeam2"; stale persisted EC-depth aliases normalize to "EcDepth2".
    pub dsd_modulator: Option<String>,
    /// DSD EC transition-loss compensation. 0.0 means no compensation.
    pub dsd_isi_penalty: Option<f32>,
    #[serde(default)]
    pub dsd_rules_enabled: bool,
    #[serde(default)]
    pub dsd_rules: Vec<DsdSourceRule>,
    /// Output attenuation in dB, clamped to -24.0..=0.0.
    pub headroom_db: Option<f32>,
    /// DSP output preroll buffer in milliseconds. 0 means automatic/default.
    pub dsp_buffer_ms: Option<u32>,
    pub device_name: Option<String>,
    pub volume: Option<f32>,
    pub eq: Option<EqConfig>,
}

impl PersistedSettings {
    pub fn playback_for_zone(&self, zone_id: &str) -> ZonePlaybackSettings {
        let mut playback = self
            .zone_settings
            .get(zone_id)
            .cloned()
            .unwrap_or_else(|| ZonePlaybackSettings::from_legacy(self));
        // Missing playback settings represent a new device/profile. DSP must
        // be an explicit opt-in rather than inheriting an enabled engine
        // default from an older build.
        if playback.upsampling_enabled.is_none() {
            playback.upsampling_enabled = Some(false);
        }
        playback.normalize_filters();
        playback
    }

    pub(super) fn mirror_legacy_playback_fields(&mut self, playback: &ZonePlaybackSettings) {
        self.filter_type = playback.filter_type.clone();
        self.target_rate = playback.target_rate;
        self.target_bit_depth = playback.target_bit_depth;
        self.upsampling_enabled = playback.upsampling_enabled;
        self.exclusive = playback.exclusive;
        self.dither_mode = playback.dither_mode.clone();
        self.output_mode = playback.output_mode.clone();
        self.dsd_modulator = playback.dsd_modulator.clone();
        self.dsd_isi_penalty = playback.dsd_isi_penalty;
        self.dsd_rules_enabled = playback.dsd_rules_enabled;
        self.dsd_rules = playback.dsd_rules.clone();
        self.headroom_db = playback.headroom_db;
        self.dsp_buffer_ms = playback.dsp_buffer_ms;
        self.device_name = playback.device_name.clone();
        self.volume = playback.volume;
        self.eq = playback.eq.clone();
    }
}

impl ZonePlaybackSettings {
    pub(super) fn normalize_filters(&mut self) {
        self.filter_type = normalize_filter_name(self.filter_type.as_deref());
        for rule in &mut self.dsd_rules {
            if let Some(filter_type) = normalize_filter_name(Some(&rule.filter_type)) {
                rule.filter_type = filter_type;
            }
        }
    }

    fn from_legacy(settings: &PersistedSettings) -> Self {
        let mut playback = Self {
            filter_type: settings.filter_type.clone(),
            target_rate: settings.target_rate,
            target_bit_depth: settings.target_bit_depth,
            upsampling_enabled: Some(settings.upsampling_enabled.unwrap_or(false)),
            exclusive: settings.exclusive,
            dither_mode: settings.dither_mode.clone(),
            output_mode: settings.output_mode.clone(),
            dsd_modulator: settings.dsd_modulator.clone(),
            dsd_isi_penalty: settings.dsd_isi_penalty,
            dsd_rules_enabled: settings.dsd_rules_enabled,
            dsd_rules: settings.dsd_rules.clone(),
            headroom_db: settings.headroom_db,
            dsp_buffer_ms: settings.dsp_buffer_ms,
            device_name: settings.device_name.clone(),
            volume: settings.volume,
            eq: settings.eq.clone(),
        };
        playback.normalize_filters();
        playback
    }
}

fn normalize_filter_name(name: Option<&str>) -> Option<String> {
    name.map(|value| {
        FilterType::from_name(value)
            .map(|filter| filter.as_name().to_string())
            .unwrap_or_else(|| value.to_string())
    })
}
