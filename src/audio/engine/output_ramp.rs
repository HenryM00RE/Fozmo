const PCM_EDGE_RAMP_FRAMES: usize = 256;

pub(crate) struct PcmTransitionRamp {
    was_playing: bool,
    fade_in_remaining: usize,
    fade_out_remaining: usize,
    last_frame: Vec<f64>,
}

impl PcmTransitionRamp {
    /// Allocate channel history before entering the real-time callback.
    pub(crate) fn new(channels: usize) -> Self {
        Self {
            was_playing: false,
            fade_in_remaining: 0,
            fade_out_remaining: 0,
            last_frame: vec![0.0; channels.max(1)],
        }
    }

    #[allow(dead_code)]
    pub(crate) fn reset(&mut self) {
        self.was_playing = false;
        self.fade_in_remaining = 0;
        self.fade_out_remaining = 0;
        self.last_frame.fill(0.0);
    }

    pub(crate) fn process(
        &mut self,
        samples: &mut [f64],
        valid_samples: usize,
        channels: usize,
        is_playing: bool,
    ) {
        let channels = channels.max(1);

        if is_playing {
            self.apply_fade_in(samples, valid_samples.min(samples.len()), channels);
        } else {
            self.fill_fade_out(samples, channels);
        }
    }

    fn apply_fade_in(&mut self, samples: &mut [f64], valid_samples: usize, channels: usize) {
        if !self.was_playing {
            self.fade_in_remaining = PCM_EDGE_RAMP_FRAMES;
            self.fade_out_remaining = 0;
        }

        let frames = valid_samples / channels;
        for frame in 0..frames {
            if self.fade_in_remaining == 0 {
                break;
            }
            let completed = PCM_EDGE_RAMP_FRAMES - self.fade_in_remaining;
            let gain = (completed + 1) as f64 / PCM_EDGE_RAMP_FRAMES as f64;
            let start = frame * channels;
            for sample in &mut samples[start..start + channels] {
                *sample *= gain;
            }
            self.fade_in_remaining -= 1;
        }

        if frames > 0 {
            let start = (frames - 1) * channels;
            for (last, sample) in self
                .last_frame
                .iter_mut()
                .zip(&samples[start..start + channels])
            {
                *last = *sample;
            }
        }
        self.was_playing = true;
    }

    fn fill_fade_out(&mut self, samples: &mut [f64], channels: usize) {
        if self.was_playing && self.fade_out_remaining == 0 {
            self.fade_out_remaining = PCM_EDGE_RAMP_FRAMES;
        }

        let frames = samples.len() / channels;
        for frame in 0..frames {
            let start = frame * channels;
            if self.fade_out_remaining > 0 {
                let gain = self.fade_out_remaining as f64 / PCM_EDGE_RAMP_FRAMES as f64;
                for ch in 0..channels {
                    samples[start + ch] =
                        self.last_frame.get(ch).copied().unwrap_or_default() * gain;
                }
                self.fade_out_remaining -= 1;
            } else {
                samples[start..start + channels].fill(0.0);
            }
        }
        let remainder = frames * channels;
        samples[remainder..].fill(0.0);

        if self.fade_out_remaining == 0 {
            self.was_playing = false;
            self.last_frame.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PcmTransitionRamp;

    #[test]
    fn fade_in_scales_first_playing_frames() {
        let mut ramp = PcmTransitionRamp::new(2);
        let mut samples = vec![1.0; 8];

        ramp.process(&mut samples, 8, 2, true);

        assert!(samples[0] > 0.0);
        assert!(samples[0] < samples[2]);
        assert!(samples[2] < samples[4]);
    }

    #[test]
    fn fade_out_uses_last_playing_frame_then_reaches_silence() {
        let mut ramp = PcmTransitionRamp::new(2);
        let mut playing = vec![0.5; 512];
        ramp.process(&mut playing, 512, 2, true);

        let mut stopped = vec![0.0; 520];
        ramp.process(&mut stopped, 0, 2, false);

        assert!(stopped[0] > stopped[stopped.len() - 4]);
        assert_eq!(stopped[stopped.len() - 1], 0.0);
    }

    #[test]
    fn reset_discards_previous_frame_without_fade_out() {
        let mut ramp = PcmTransitionRamp::new(2);
        let mut playing = vec![0.5; 512];
        ramp.process(&mut playing, 512, 2, true);

        ramp.reset();

        let mut stopped = vec![0.0; 16];
        ramp.process(&mut stopped, 0, 2, false);

        assert!(stopped.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn playback_boundary_without_reset_fades_out_then_fades_in() {
        let mut ramp = PcmTransitionRamp::new(2);
        let mut playing = vec![0.5; 512];
        ramp.process(&mut playing, 512, 2, true);

        let mut boundary_silence = vec![0.0; 520];
        ramp.process(&mut boundary_silence, 0, 2, false);

        assert!(boundary_silence[0] > boundary_silence[boundary_silence.len() - 2]);
        assert_eq!(boundary_silence[boundary_silence.len() - 1], 0.0);

        let mut next_track = vec![1.0; 8];
        ramp.process(&mut next_track, 8, 2, true);

        assert!(next_track[0] > 0.0);
        assert!(next_track[0] < next_track[2]);
    }

    #[test]
    fn processing_does_not_grow_callback_owned_storage() {
        let mut ramp = PcmTransitionRamp::new(2);
        let capacity = ramp.last_frame.capacity();
        let mut samples = vec![1.0; 8];

        ramp.process(&mut samples, 8, 2, true);
        ramp.process(&mut samples, 0, 2, false);

        assert_eq!(ramp.last_frame.capacity(), capacity);
    }
}
