//! 帧组装与音频后处理。

/// 把大小不定的 GATT notification 攒成定长 ADPCM 帧。
#[derive(Default)]
pub struct FrameAccumulator {
    pending: Vec<u8>,
}

impl FrameAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一段数据，对每个攒满的帧调用 `emit`。
    pub fn push(&mut self, data: &[u8], frame_size: usize, mut emit: impl FnMut(&[u8])) {
        if frame_size == 0 {
            return;
        }
        self.pending.extend_from_slice(data);
        let complete = self.pending.len() / frame_size;
        for i in 0..complete {
            emit(&self.pending[i * frame_size..(i + 1) * frame_size]);
        }
        self.pending.drain(..complete * frame_size);
    }

    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

/// 3 抽头平滑（1,2,1)/4 + dB 增益，与原 macOS 实现保持一致的听感。
/// 首尾样本不参与平滑；增益限制在 ±24 dB。
pub fn postprocess(samples: &mut [i16], gain_db: f64) {
    if samples.len() >= 3 {
        let orig: Vec<i32> = samples.iter().map(|&s| s as i32).collect();
        for i in 1..orig.len() - 1 {
            samples[i] = ((orig[i - 1] + 2 * orig[i] + orig[i + 1]) >> 2) as i16;
        }
    }
    let gain_db = if gain_db.is_finite() { gain_db.clamp(-24.0, 24.0) } else { 0.0 };
    if gain_db != 0.0 {
        let gain = 10f64.powf(gain_db / 20.0);
        for s in samples.iter_mut() {
            *s = ((*s as f64) * gain).round().clamp(-32_768.0, 32_767.0) as i16;
        }
    }
}

/// 线性插值重采样：16 kHz 单声道 i16 → 目标采样率 f32（[-1,1]）。
///
/// 语音场景线性插值足够；跨 `process` 调用保留上一个样本以保证连续性。
pub struct LinearResampler {
    step: f64,
    pos: f64,
    prev: f32,
}

impl LinearResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Self { step: in_rate as f64 / out_rate as f64, pos: 0.0, prev: 0.0 }
    }

    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.prev = 0.0;
    }

    pub fn process(&mut self, input: &[i16], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        const SCALE: f32 = 1.0 / 32768.0;
        // 虚拟序列 v[0] = prev，v[k] = input[k-1]；pos 是其上的小数下标。
        let len = input.len() as f64;
        while self.pos < len {
            let i0 = self.pos.floor();
            let frac = (self.pos - i0) as f32;
            let i0 = i0 as usize;
            let a = if i0 == 0 { self.prev } else { input[i0 - 1] as f32 * SCALE };
            let b = input[i0] as f32 * SCALE;
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= len;
        self.prev = *input.last().unwrap() as f32 * SCALE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulator_reassembles_across_notifications() {
        let mut acc = FrameAccumulator::new();
        let mut frames: Vec<Vec<u8>> = Vec::new();
        acc.push(&[1, 2, 3], 4, |f| frames.push(f.to_vec()));
        assert!(frames.is_empty());
        acc.push(&[4, 5, 6, 7, 8], 4, |f| frames.push(f.to_vec()));
        assert_eq!(frames, vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
        acc.reset();
        acc.push(&[9, 9, 9, 9], 4, |f| frames.push(f.to_vec()));
        assert_eq!(frames.len(), 3);
    }

    #[test]
    fn postprocess_smooths_interior_samples() {
        let mut samples = vec![100i16, 200, 100];
        postprocess(&mut samples, 0.0);
        assert_eq!(samples, vec![100, 150, 100]);
    }

    #[test]
    fn postprocess_applies_gain_with_clamp() {
        let mut samples = vec![1000i16, 1000];
        postprocess(&mut samples, 6.0);
        assert_eq!(samples, vec![1995, 1995]);
        let mut loud = vec![30_000i16, 30_000];
        postprocess(&mut loud, 24.0);
        assert_eq!(loud, vec![32_767, 32_767]);
    }

    #[test]
    fn resampler_triples_16k_to_48k() {
        let mut rs = LinearResampler::new(16_000, 48_000);
        let mut out = Vec::new();
        rs.process(&[16_384; 4], &mut out);
        assert_eq!(out.len(), 12);
        // 从 prev=0 渐入，稳定后应贴近 0.5。
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[11] - 0.5).abs() < 1e-3);
        // 连续性：第二块继续输出 12 个稳定样本。
        out.clear();
        rs.process(&[16_384; 4], &mut out);
        assert_eq!(out.len(), 12);
        assert!(out.iter().all(|v| (v - 0.5).abs() < 1e-3));
    }
}
