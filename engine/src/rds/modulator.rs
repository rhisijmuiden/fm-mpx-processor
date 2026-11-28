//! RDS baseband modulator: differential encoding, biphase symbols shaped with the
//! IEC 62106 cosine filter, clocked from the 19 kHz pilot (1 bit = 16 pilot cycles,
//! 57 kHz = 3 × pilot). Output is normalised so the absolute peak is ≤ 1.

use super::encoder::RdsEncoder;
use std::f64::consts::PI;

const SPAN: i64 = 3; // symbols on each side that contribute
const RES: usize = 512; // table points per bit

pub struct RdsModulator {
    table: Vec<f32>,
    norm: f32,
    symbols: [f32; 16],
    generated: i64, // next bit index to generate
    diff: u8,
    pub encoder: RdsEncoder,
    /// Carrier multiple of the pilot (3.0 for 57 kHz; RDS2: 3.5, 3.75, 4.0).
    pub carrier_mult: f64,
}

fn h(tau: f64) -> f64 {
    // ∫0^2 cos(π f / 4) cos(2π f τ) df  (f in units of the bit rate)
    let a = PI / 4.0;
    let b = 2.0 * PI * tau;
    let f = 2.0;
    let term = |x: f64| if x.abs() < 1e-9 { f } else { (x * f).sin() / x };
    0.5 * (term(a - b) + term(a + b))
}

impl RdsModulator {
    pub fn new(encoder: RdsEncoder, carrier_mult: f64) -> Self {
        let n = (2 * SPAN as usize + 1) * RES;
        let mut table = vec![0.0f32; n + 1];
        for (i, v) in table.iter_mut().enumerate() {
            let tau = i as f64 / RES as f64 - (SPAN as f64 + 0.5);
            *v = (h(tau + 0.25) - h(tau - 0.25)) as f32;
        }
        // Worst-case peak over every symbol pattern = max over phase of Σ|s|.
        let mut worst = 0.0f32;
        for p in 0..RES {
            let mut s = 0.0;
            for j in -SPAN..=SPAN {
                let idx = (p as i64 + (j + SPAN) * RES as i64) as usize;
                s += table[idx].abs();
            }
            worst = worst.max(s);
        }
        Self {
            table,
            norm: 1.0 / worst,
            symbols: [0.0; 16],
            generated: 0,
            diff: 0,
            encoder,
            carrier_mult,
        }
    }

    fn ensure(&mut self, upto: i64) {
        while self.generated <= upto {
            let d = self.encoder.next_bit();
            self.diff ^= d;
            self.symbols[(self.generated & 15) as usize] = if self.diff == 1 { 1.0 } else { -1.0 };
            self.generated += 1;
        }
    }

    /// Baseband value at a pilot-cycle time.
    #[inline]
    pub fn baseband(&mut self, pilot_cycles: f64) -> f32 {
        let t = pilot_cycles / 16.0; // bits
        let k = t.floor() as i64;
        let frac = t - k as f64;
        self.ensure(k + SPAN);
        let mut acc = 0.0f32;
        for j in -SPAN..=SPAN {
            let bit = k + j;
            if bit < 0 {
                continue;
            }
            // Position relative to the window start of symbol `bit`.
            let pos = (frac - j as f64 + SPAN as f64) * RES as f64;
            let i = pos as usize;
            if i >= self.table.len() - 1 {
                continue;
            }
            let a = (pos - i as f64) as f32;
            let v = self.table[i] + a * (self.table[i + 1] - self.table[i]);
            acc += self.symbols[(bit & 15) as usize] * v;
        }
        acc * self.norm
    }

    /// Modulated subcarrier (peak ≤ 1).
    #[inline]
    pub fn sample(&mut self, pilot_cycles: f64) -> f32 {
        let bb = self.baseband(pilot_cycles);
        let ph = (pilot_cycles * self.carrier_mult).fract() * 2.0 * PI;
        bb * ph.sin() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RdsConfig;
    use crate::rds::encoder::{OFFSET_A, OFFSET_B, OFFSET_C, OFFSET_D};

    fn remainder26(word: u32) -> u16 {
        let mut reg = word;
        for bit in (10..26).rev() {
            if reg & (1 << bit) != 0 {
                reg ^= 0x5B9 << (bit - 10);
            }
        }
        (reg & 0x3FF) as u16
    }

    /// Demodulate our own 57 kHz output and recover the PS name.
    #[test]
    fn modulated_rds_decodes_back() {
        let cfg = RdsConfig { ps: "PILOT FM".into(), rt: "Hello".into(), ct: false, ..Default::default() };
        let mut m = RdsModulator::new(RdsEncoder::new(&cfg, true), 3.0);
        let fs = 192_000.0;
        let seconds = 3.0;
        let n = (fs * seconds) as usize;
        // Coherent demodulation: multiply by the carrier, then integrate each half bit.
        let samples_per_bit = fs / 1187.5;
        let mut half = vec![0.0f64; (n as f64 / samples_per_bit * 2.0) as usize + 2];
        for i in 0..n {
            let cycles = i as f64 * 19_000.0 / fs;
            let y = m.sample(cycles) as f64;
            let carrier = ((cycles * 3.0).fract() * 2.0 * PI).sin();
            let t_bits = cycles / 16.0;
            let idx = (t_bits * 2.0) as usize;
            half[idx] += y * carrier;
        }
        // Biphase: first half minus second half gives the symbol sign.
        let nbits = half.len() / 2 - 1;
        let syms: Vec<u8> = (0..nbits).map(|k| (half[2 * k] - half[2 * k + 1] > 0.0) as u8).collect();
        // Differential decode.
        let mut bits = vec![0u8; nbits];
        for k in 1..nbits {
            bits[k] = syms[k] ^ syms[k - 1];
        }
        // Find block sync and collect PS from 0A groups.
        let mut ps = [b'?'; 8];
        let mut found = 0;
        let word = |s: usize| bits[s..s + 26].iter().fold(0u32, |a, &b| (a << 1) | b as u32);
        let mut i = 1;
        while i + 104 < nbits {
            if remainder26(word(i)) == OFFSET_A
                && remainder26(word(i + 26)) == OFFSET_B
                && remainder26(word(i + 52)) == OFFSET_C
                && remainder26(word(i + 78)) == OFFSET_D
            {
                let b2 = (word(i + 26) >> 10) as u16;
                let b4 = (word(i + 78) >> 10) as u16;
                assert_eq!(word(i) >> 10, 0x8201);
                if b2 >> 12 == 0 {
                    let seg = (b2 & 3) as usize;
                    ps[seg * 2] = (b4 >> 8) as u8;
                    ps[seg * 2 + 1] = b4 as u8;
                }
                found += 1;
                i += 104;
            } else {
                i += 1;
            }
        }
        assert!(found >= 20, "only {found} valid groups decoded");
        assert_eq!(&ps, b"PILOT FM");
    }

    #[test]
    fn peak_is_normalised() {
        let mut m = RdsModulator::new(RdsEncoder::new(&RdsConfig::default(), true), 3.0);
        let mut peak = 0.0f32;
        for i in 0..192_000 {
            peak = peak.max(m.sample(i as f64 * 19_000.0 / 192_000.0).abs());
        }
        assert!(peak <= 1.0 && peak > 0.3, "peak {peak}");
    }
}
