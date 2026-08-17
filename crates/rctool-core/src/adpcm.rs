//! IMA/DVI ADPCM 解码器。
//!
//! ATVV 音频每字节两个 4-bit 样本（高 nibble 在前），解出 16-bit PCM。
//! 16 kHz × 4 bit = 64 kbps，这正是语音能塞进 BLE GATT notify 的原因。

const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408,
    449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630,
    9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];
const INDEX_TABLE: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

pub struct ImaAdpcmDecoder {
    predictor: i32,
    step_index: i32,
}

impl Default for ImaAdpcmDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ImaAdpcmDecoder {
    pub fn new() -> Self {
        Self { predictor: 0, step_index: 0 }
    }

    /// 重置解码状态。AUDIO_SYNC 用它把解码器对齐到遥控器声明的状态。
    pub fn reset(&mut self, predictor: i32, step_index: i32) {
        self.predictor = predictor.clamp(-32_768, 32_767);
        self.step_index = step_index.clamp(0, 88);
    }

    pub fn decode_into(&mut self, data: &[u8], out: &mut Vec<i16>) {
        out.reserve(data.len() * 2);
        for byte in data {
            out.push(self.nibble((byte >> 4) as i32));
            out.push(self.nibble((byte & 0x0F) as i32));
        }
    }

    fn nibble(&mut self, n: i32) -> i16 {
        let step = STEP_TABLE[self.step_index as usize];
        let mut diff = step >> 3;
        if n & 1 != 0 {
            diff += step >> 2;
        }
        if n & 2 != 0 {
            diff += step >> 1;
        }
        if n & 4 != 0 {
            diff += step;
        }
        self.predictor += if n & 8 != 0 { -diff } else { diff };
        self.predictor = self.predictor.clamp(-32_768, 32_767);
        self.step_index = (self.step_index + INDEX_TABLE[(n & 7) as usize]).clamp(0, 88);
        self.predictor as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_known_nibbles() {
        // 0x77：n=7 时 diff = step>>3 + step>>2 + step>>1 + step。
        // 第一样本 step=7 → diff=0+1+3+7=11；第二样本 step=STEP[8]=16 → diff=2+4+8+16=30。
        let mut dec = ImaAdpcmDecoder::new();
        let mut out = Vec::new();
        dec.decode_into(&[0x77], &mut out);
        assert_eq!(out, vec![11, 41]);
    }

    #[test]
    fn zero_nibbles_hold_predictor() {
        let mut dec = ImaAdpcmDecoder::new();
        dec.reset(100, 5);
        let mut out = Vec::new();
        // n=0：diff = step>>3（STEP[5]=12 → 1，STEP[4]=11 → 1），predictor 缓慢爬升。
        dec.decode_into(&[0x00], &mut out);
        assert_eq!(out, vec![101, 102]);
    }

    #[test]
    fn reset_clamps_ranges() {
        let mut dec = ImaAdpcmDecoder::new();
        dec.reset(99_999, 999);
        let mut out = Vec::new();
        dec.decode_into(&[0x08], &mut out);
        // 高 nibble 0：predictor 从 32767 被夹住后 +diff 再夹住；
        // 低 nibble 8（符号位）：往回走。只验证不越界不 panic。
        assert_eq!(out.len(), 2);
    }
}
