//! RDS group generation (IEC 62106): 0A (PS, AF, DI, TA/TP, PTY, M/S), 2A (RadioText),
//! 4A (clock time). Produces a stream of bits at 1187.5 bit/s.

use super::charset::to_rds_bytes;
use crate::config::RdsConfig;

const POLY: u32 = 0x5B9; // x^10 + x^8 + x^7 + x^5 + x^4 + x^3 + 1
pub const OFFSET_A: u16 = 0x0FC;
pub const OFFSET_B: u16 = 0x198;
pub const OFFSET_C: u16 = 0x168;
pub const OFFSET_CP: u16 = 0x350;
pub const OFFSET_D: u16 = 0x1B4;

/// 10-bit CRC of a 16-bit information word.
pub fn checkword(info: u16) -> u16 {
    let mut reg: u32 = (info as u32) << 10;
    for bit in (10..26).rev() {
        if reg & (1 << bit) != 0 {
            reg ^= POLY << (bit - 10);
        }
    }
    (reg & 0x3FF) as u16
}

/// A 26-bit block: info word + (checkword XOR offset).
pub fn block(info: u16, offset: u16) -> u32 {
    ((info as u32) << 10) | (checkword(info) ^ offset) as u32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Group(pub [u16; 4]);

impl Group {
    pub fn to_bits(self) -> [u32; 4] {
        [
            block(self.0[0], OFFSET_A),
            block(self.0[1], OFFSET_B),
            block(self.0[2], OFFSET_C),
            block(self.0[3], OFFSET_D),
        ]
    }
}

/// Mutable RDS state (the realtime thread owns one).
pub struct RdsEncoder {
    pi: u16,
    pty: u8,
    tp: bool,
    ta: bool,
    ms: bool,
    stereo: bool,
    ps: [u8; 8],
    rt: [u8; 64],
    rt_len: usize,
    rt_ab: bool,
    rt_text: String,
    af: Vec<u8>,
    ct: bool,
    ps_seg: usize,
    rt_seg: usize,
    af_idx: usize,
    seq: usize,
    last_ct_minute: i64,
    /// Bits of the current group, MSB first.
    bits: [u32; 4],
    bit_pos: usize,
}

impl RdsEncoder {
    pub fn new(cfg: &RdsConfig, stereo: bool) -> Self {
        let mut e = Self {
            pi: 0,
            pty: 0,
            tp: false,
            ta: false,
            ms: true,
            stereo,
            ps: [b' '; 8],
            rt: [b' '; 64],
            rt_len: 0,
            rt_ab: false,
            rt_text: String::new(),
            af: vec![],
            ct: true,
            ps_seg: 0,
            rt_seg: 0,
            af_idx: 0,
            seq: 0,
            last_ct_minute: -1,
            bits: [0; 4],
            bit_pos: 104,
        };
        e.set_config(cfg, stereo);
        e
    }

    pub fn set_config(&mut self, cfg: &RdsConfig, stereo: bool) {
        self.pi = u16::from_str_radix(cfg.pi.trim(), 16).unwrap_or(0x8201);
        self.pty = cfg.pty & 31;
        self.tp = cfg.tp;
        self.ta = cfg.ta;
        self.ms = cfg.ms;
        self.stereo = stereo;
        self.ct = cfg.ct;
        let ps = to_rds_bytes(&cfg.ps);
        self.ps = [b' '; 8];
        for (d, s) in self.ps.iter_mut().zip(ps.iter()) {
            *d = *s;
        }
        // AF method A: count code, then the frequencies. Filler 205 pads odd lists.
        self.af.clear();
        if !cfg.af_mhz.is_empty() {
            self.af.push(224 + cfg.af_mhz.len() as u8);
            for f in &cfg.af_mhz {
                self.af.push(((f - 87.5) * 10.0).round().clamp(1.0, 204.0) as u8);
            }
            if self.af.len() % 2 == 1 {
                self.af.push(205);
            }
        }
        if !cfg.dynamic_rt || self.rt_text.is_empty() {
            self.set_rt(&cfg.rt);
        }
    }

    pub fn ps(&self) -> String {
        String::from_utf8_lossy(&self.ps).to_string()
    }

    pub fn rt(&self) -> &str {
        &self.rt_text
    }

    /// Change RadioText. Toggles the A/B flag so receivers clear their display.
    pub fn set_rt(&mut self, text: &str) {
        let text: String = text.chars().take(64).collect();
        if text == self.rt_text {
            return;
        }
        self.rt_text = text.clone();
        let bytes = to_rds_bytes(&text);
        self.rt = [b' '; 64];
        let n = bytes.len().min(64);
        self.rt[..n].copy_from_slice(&bytes[..n]);
        self.rt_len = n;
        if n < 64 {
            self.rt[n] = 0x0D;
            self.rt_len = n + 1;
        }
        self.rt_ab = !self.rt_ab;
        self.rt_seg = 0;
    }

    fn block2_common(&self, group_type: u16, version_b: bool) -> u16 {
        (group_type << 12)
            | ((version_b as u16) << 11)
            | ((self.tp as u16) << 10)
            | ((self.pty as u16) << 5)
    }

    pub fn group_0a(&mut self) -> Group {
        let seg = self.ps_seg;
        // DI bits: segment 0 → d3 (dynamic PTY), 1 → d2 (compressed), 2 → d1 (artificial head), 3 → d0 (stereo).
        let di = if seg == 3 { self.stereo as u16 } else { 0 };
        let b2 = self.block2_common(0, false)
            | ((self.ta as u16) << 4)
            | ((self.ms as u16) << 3)
            | (di << 2)
            | seg as u16;
        let b3 = if self.af.is_empty() {
            (224 << 8) | 205 // "no AF" + filler
        } else {
            let a = self.af[self.af_idx] as u16;
            let b = self.af[self.af_idx + 1] as u16;
            if seg == 3 {
                self.af_idx = (self.af_idx + 2) % self.af.len();
            }
            (a << 8) | b
        };
        let b4 = ((self.ps[seg * 2] as u16) << 8) | self.ps[seg * 2 + 1] as u16;
        self.ps_seg = (seg + 1) % 4;
        Group([self.pi, b2, b3, b4])
    }

    pub fn group_2a(&mut self) -> Group {
        let seg = self.rt_seg;
        let b2 = self.block2_common(2, false) | ((self.rt_ab as u16) << 4) | seg as u16;
        let c = |i: usize| self.rt[seg * 4 + i] as u16;
        let b3 = (c(0) << 8) | c(1);
        let b4 = (c(2) << 8) | c(3);
        let segments = self.rt_len.div_ceil(4).max(1);
        self.rt_seg = (seg + 1) % segments;
        Group([self.pi, b2, b3, b4])
    }

    /// Clock time and date (4A) for a UTC time with a local offset in half hours.
    pub fn group_4a(&self, unix_utc: i64, offset_half_hours: i32) -> Group {
        let days = unix_utc.div_euclid(86_400);
        let mjd = (days + 40_587) as u32;
        let secs = unix_utc.rem_euclid(86_400);
        let hour = (secs / 3600) as u32;
        let minute = ((secs % 3600) / 60) as u32;
        let b2 = self.block2_common(4, false) | ((mjd >> 15) & 0x3) as u16;
        let b3 = (((mjd & 0x7FFF) << 1) | (hour >> 4)) as u16;
        let sign = if offset_half_hours < 0 { 1u32 } else { 0 };
        let off = offset_half_hours.unsigned_abs() & 0x1F;
        let b4 = (((hour & 0xF) << 12) | (minute << 6) | (sign << 5) | off) as u16;
        Group([self.pi, b2, b3, b4])
    }

    /// Group sequence: 0A 0A 2A repeating; 4A at the start of every minute.
    pub fn next_group(&mut self) -> Group {
        if self.ct {
            let now = chrono::Utc::now();
            let minute = now.timestamp() / 60;
            if minute != self.last_ct_minute && now.timestamp() % 60 < 2 {
                self.last_ct_minute = minute;
                let off = chrono::Local::now().offset().local_minus_utc() / 1800;
                return self.group_4a(minute * 60, off);
            }
        }
        self.seq = (self.seq + 1) % 3;
        if self.seq == 2 { self.group_2a() } else { self.group_0a() }
    }

    /// Next data bit (before differential encoding).
    pub fn next_bit(&mut self) -> u8 {
        if self.bit_pos >= 104 {
            self.bits = self.next_group().to_bits();
            self.bit_pos = 0;
        }
        let blk = self.bits[self.bit_pos / 26];
        let bit = (blk >> (25 - self.bit_pos % 26)) & 1;
        self.bit_pos += 1;
        bit as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkword_matches_spec_example() {
        // The code is linear and the remainder of a valid block equals its offset word.
        assert_eq!(checkword(0), 0);
        let a = checkword(0x1234);
        let b = checkword(0x00FF);
        assert_eq!(checkword(0x1234 ^ 0x00FF), a ^ b);
        let blk = block(0x8201, OFFSET_A);
        assert_eq!(blk >> 10, 0x8201);
        assert_eq!(remainder26(blk), OFFSET_A);
        // Generator polynomial itself must leave no remainder.
        assert_eq!(remainder26(POLY), 0);
    }

    fn remainder26(word: u32) -> u16 {
        let mut reg = word;
        for bit in (10..26).rev() {
            if reg & (1 << bit) != 0 {
                reg ^= POLY << (bit - 10);
            }
        }
        (reg & 0x3FF) as u16
    }

    #[test]
    fn ps_cycles_through_four_segments() {
        let cfg = RdsConfig { ps: "RADIO 1".into(), ..Default::default() };
        let mut e = RdsEncoder::new(&cfg, true);
        let mut ps = [0u8; 8];
        for _ in 0..4 {
            let g = e.group_0a();
            let seg = (g.0[1] & 3) as usize;
            ps[seg * 2] = (g.0[3] >> 8) as u8;
            ps[seg * 2 + 1] = g.0[3] as u8;
        }
        assert_eq!(&ps, b"RADIO 1 ");
    }

    #[test]
    fn ct_encodes_mjd() {
        let e = RdsEncoder::new(&RdsConfig::default(), true);
        // 2026-11-28 00:00 UTC
        let t = 1_795_824_000i64;
        let g = e.group_4a(t, 2);
        let mjd = (((g.0[1] & 3) as u32) << 15) | ((g.0[2] as u32) >> 1);
        assert_eq!(mjd as i64, t / 86_400 + 40_587);
    }
}
