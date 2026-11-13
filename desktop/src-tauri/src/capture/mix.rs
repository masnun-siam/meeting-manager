//! Sample arithmetic for the capture callback. It runs on Core Audio's real-time thread, so every
//! function writes into buffers the caller reserved up front and only allocates when a callback
//! delivers more frames than that capacity.

/// Averages any number of interleaved buffers into one mono buffer: the mono sample of a frame is
/// the sum of every channel of every added buffer divided by the total channel count, so no gain
/// is applied. Buffers of different lengths are padded with silence.
pub struct Downmix {
    sum: Vec<f32>,
    channels: usize,
}

impl Downmix {
    pub fn with_capacity(frames: usize) -> Downmix {
        Downmix {
            sum: Vec::with_capacity(frames),
            channels: 0,
        }
    }

    pub fn clear(&mut self) {
        self.sum.clear();
        self.channels = 0;
    }

    /// Adds one interleaved buffer of `channels` channels. A buffer with no channels adds nothing.
    pub fn add(&mut self, buf: &[f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let frames = buf.len() / channels;
        if self.sum.len() < frames {
            self.sum.resize(frames, 0.0);
        }
        for (sum, frame) in self.sum.iter_mut().zip(buf.chunks_exact(channels)) {
            *sum += frame.iter().map(|&s| finite(s)).sum::<f32>();
        }
        self.channels += channels;
    }

    /// The mono samples of everything added since the last `clear`.
    pub fn finish(&mut self) -> &[f32] {
        if self.channels > 1 {
            let scale = 1.0 / self.channels as f32;
            for s in &mut self.sum {
                *s *= scale;
            }
            self.channels = 1;
        }
        &self.sum
    }
}

/// Sums the microphone and the system audio, padding the shorter one with silence, and clips the
/// result to [-1, 1]. NaN and infinity count as silence.
pub fn mix(mic: &[f32], tap: &[f32], out: &mut Vec<f32>) {
    out.clear();
    let len = mic.len().max(tap.len());
    out.extend((0..len).map(|i| {
        let sum = mic.get(i).copied().map_or(0.0, finite) + tap.get(i).copied().map_or(0.0, finite);
        sum.clamp(-1.0, 1.0)
    }));
}

/// Converts clipped float samples to `i16`. Full scale is symmetric: 1.0 is `i16::MAX` and -1.0 is
/// -32767, so `i16::MIN` never occurs.
pub fn to_i16(input: &[f32], out: &mut Vec<i16>) {
    out.clear();
    out.extend(
        input
            .iter()
            .map(|&s| (finite(s).clamp(-1.0, 1.0) * 32767.0).round() as i16),
    );
}

fn finite(s: f32) -> f32 {
    if s.is_finite() { s } else { 0.0 }
}

/// Linear-interpolation resampler that carries its position and last sample across calls, so
/// feeding a signal in chunks gives the same output as feeding it at once. It is the safety net
/// for a device that ignores the 48 kHz rate the aggregate device asks for.
pub struct Resampler {
    /// Input samples advanced per output sample.
    step: f64,
    /// Position of the next output sample, where 0 is `last` and 1 the first sample of the next
    /// input.
    pos: f64,
    last: f32,
}

impl Resampler {
    pub fn new(from_hz: f64, to_hz: f64) -> Resampler {
        Resampler {
            step: from_hz / to_hz,
            pos: 1.0,
            last: 0.0,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.step == 1.0
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.clear();
        if self.is_identity() {
            out.extend_from_slice(input);
            return;
        }
        let Some(&end) = input.last() else {
            return;
        };
        let at = |i: usize| if i == 0 { self.last } else { input[i - 1] };
        while (self.pos.floor() as usize) < input.len() {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;
            let (a, b) = (at(i), at(i + 1));
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= input.len() as f64;
        self.last = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixed(mic: &[f32], tap: &[f32]) -> Vec<i16> {
        let mut float = Vec::new();
        mix(mic, tap, &mut float);
        let mut out = Vec::new();
        to_i16(&float, &mut out);
        out
    }

    fn downmixed(buf: &[f32], channels: usize) -> Vec<f32> {
        let mut downmix = Downmix::with_capacity(16);
        downmix.add(buf, channels);
        downmix.finish().to_vec()
    }

    #[test]
    fn silence_stays_silent() {
        assert_eq!(mixed(&[0.0; 4], &[0.0; 4]), vec![0; 4]);
    }

    #[test]
    fn either_source_alone_passes_through_without_gain() {
        assert_eq!(mixed(&[0.5, -0.25], &[0.0, 0.0]), vec![16384, -8192]);
        assert_eq!(mixed(&[0.0, 0.0], &[0.5, -0.25]), vec![16384, -8192]);
    }

    #[test]
    fn both_sources_are_summed() {
        assert_eq!(mixed(&[0.25], &[0.25]), vec![16384]);
    }

    #[test]
    fn a_full_scale_sum_clips_instead_of_wrapping() {
        assert_eq!(mixed(&[0.8], &[0.8]), vec![i16::MAX]);
        // Negative full scale is -32767, mirroring the positive side; i16::MIN is never produced.
        assert_eq!(mixed(&[-0.8], &[-0.8]), vec![-32767]);
    }

    #[test]
    fn nan_and_infinity_are_silence() {
        assert_eq!(
            mixed(
                &[f32::NAN, f32::INFINITY, f32::NEG_INFINITY],
                &[0.5, 0.5, 0.0]
            ),
            vec![16384, 16384, 0]
        );
    }

    #[test]
    fn the_shorter_source_is_padded_with_silence() {
        assert_eq!(mixed(&[0.5, 0.5, 0.5], &[0.5]), vec![32767, 16384, 16384]);
        assert_eq!(mixed(&[0.5], &[0.5, 0.5]), vec![32767, 16384]);
        assert_eq!(mixed(&[0.5, 0.5], &[0.5, 0.5]).len(), 2);
    }

    #[test]
    fn empty_input_gives_empty_output() {
        assert_eq!(mixed(&[], &[]), Vec::<i16>::new());
        assert_eq!(downmixed(&[], 2), Vec::<f32>::new());
    }

    #[test]
    fn three_interleaved_channels_are_averaged() {
        assert_eq!(
            downmixed(&[0.3, 0.6, 0.9, -0.3, 0.0, 0.3], 3),
            vec![0.6, 0.0]
        );
    }

    #[test]
    fn zero_channels_give_silence() {
        assert_eq!(downmixed(&[0.5, 0.5], 0), Vec::<f32>::new());
    }

    #[test]
    fn separate_mono_buffers_are_averaged_like_interleaved_channels() {
        let mut downmix = Downmix::with_capacity(4);
        downmix.add(&[0.2, 0.4], 1);
        downmix.add(&[0.4, 0.0], 1);
        assert_eq!(downmix.finish(), &[0.3, 0.2]);
        downmix.clear();
        downmix.add(&[0.5], 1);
        assert_eq!(downmix.finish(), &[0.5]);
    }

    #[test]
    fn mixing_within_capacity_does_not_reallocate() {
        let mut out = Vec::with_capacity(8);
        let before = out.as_ptr();
        mix(&[0.1; 8], &[0.1; 8], &mut out);
        assert_eq!(out.as_ptr(), before);
    }

    #[test]
    fn equal_rates_are_an_identity() {
        let mut resampler = Resampler::new(48_000.0, 48_000.0);
        assert!(resampler.is_identity());
        let mut out = Vec::new();
        resampler.process(&[0.1, 0.2, 0.3], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn resampling_44_1_to_48_khz_keeps_the_duration() {
        let input: Vec<f32> = (0..441_000).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut resampler = Resampler::new(44_100.0, 48_000.0);
        let mut out = Vec::new();
        resampler.process(&input, &mut out);
        assert!(out.len().abs_diff(480_000) <= 1, "{} samples", out.len());
    }

    #[test]
    fn resampling_keeps_pitch() {
        // A ramp keeps its slope per second: interpolated values sit on the same line.
        let input: Vec<f32> = (0..4410).map(|i| i as f32 / 44_100.0).collect();
        let mut resampler = Resampler::new(44_100.0, 48_000.0);
        let mut out = Vec::new();
        resampler.process(&input, &mut out);
        for (n, &s) in out.iter().enumerate() {
            let expected = n as f32 / 48_000.0;
            assert!((s - expected).abs() < 1e-6, "sample {n}: {s} vs {expected}");
        }
    }

    #[test]
    fn chunked_input_gives_the_same_output_as_one_input() {
        let input: Vec<f32> = (0..10_000).map(|i| (i as f32 * 0.03).sin()).collect();
        let mut whole = Vec::new();
        Resampler::new(44_100.0, 48_000.0).process(&input, &mut whole);

        let mut resampler = Resampler::new(44_100.0, 48_000.0);
        let mut chunked = Vec::new();
        let mut out = Vec::new();
        for chunk in input.chunks(333) {
            resampler.process(chunk, &mut out);
            chunked.extend_from_slice(&out);
        }
        assert_eq!(whole.len(), chunked.len());
        for (a, b) in whole.iter().zip(&chunked) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn an_empty_chunk_changes_nothing() {
        let mut resampler = Resampler::new(44_100.0, 48_000.0);
        let mut out = vec![1.0];
        resampler.process(&[], &mut out);
        assert!(out.is_empty());
    }
}
