//! Variable-ratio windowed-sinc resampler (stereo).
//!
//! Converts the decoder output (any rate) to the 48 kHz processing rate and
//! absorbs clock drift between the stream server and our output clock: the
//! ratio is nudged by a slow controller that keeps the input buffer at its
//! target fill.

use super::fir::kaiser_beta;
use std::f64::consts::PI;

const ZERO_CROSSINGS: usize = 24;
const PHASES: usize = 512;

pub struct Resampler {
    /// table[phase][tap], phase 0..=PHASES so we can interpolate between phases.
    table: Vec<Vec<f32>>,
    taps: usize,
    hist_l: Vec<f32>,
    hist_r: Vec<f32>,
    /// Fractional read position into the history (in input samples).
    pos: f64,
    /// Input samples per output sample.
    step: f64,
    nominal_step: f64,
}

impl Resampler {
    pub fn new(in_rate: f64, out_rate: f64) -> Self {
        let taps = 2 * ZERO_CROSSINGS;
        // Cut-off just below the lower Nyquist; never above 20 kHz (FM is 15 kHz anyway).
        let cutoff = (0.5 * in_rate.min(out_rate) * 0.92).min(20_000.0) / (in_rate * 0.5);
        let beta = kaiser_beta(90.0);
        let i0b = bessel_i0(beta);
        let mut table = Vec::with_capacity(PHASES + 1);
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let mut row = Vec::with_capacity(taps);
            let mut sum = 0.0;
            for k in 0..taps {
                // Tap k sits at input offset (k - ZERO_CROSSINGS + 1) relative to floor(pos).
                let t = (k as f64 - (ZERO_CROSSINGS as f64 - 1.0)) - frac;
                let x = t * cutoff;
                let sinc = if x.abs() < 1e-12 { 1.0 } else { (PI * x).sin() / (PI * x) };
                let r = t / ZERO_CROSSINGS as f64;
                let w = if r.abs() >= 1.0 { 0.0 } else { bessel_i0(beta * (1.0 - r * r).sqrt()) / i0b };
                let v = sinc * w;
                sum += v;
                row.push(v);
            }
            table.push(row.into_iter().map(|v| (v / sum) as f32).collect());
        }
        let step = in_rate / out_rate;
        Self {
            table,
            taps,
            hist_l: vec![0.0; taps],
            hist_r: vec![0.0; taps],
            pos: (ZERO_CROSSINGS - 1) as f64,
            step,
            nominal_step: step,
        }
    }

    /// Relative speed correction, e.g. 1.0001 consumes input 100 ppm faster.
    pub fn set_ratio_relative(&mut self, rel: f64) {
        self.step = self.nominal_step * rel.clamp(0.99, 1.01);
    }

    /// Resample a block of interleaved-free stereo input, appending to `out_l/out_r`.
    pub fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut Vec<f32>, out_r: &mut Vec<f32>) {
        self.hist_l.extend_from_slice(in_l);
        self.hist_r.extend_from_slice(in_r);
        let need = ZERO_CROSSINGS; // samples needed after floor(pos)
        while (self.pos as usize) + need < self.hist_l.len() {
            let i = self.pos as usize;
            let frac = self.pos - i as f64;
            let fp = frac * PHASES as f64;
            let p = fp as usize;
            let a = (fp - p as f64) as f32;
            let start = i + 1 - ZERO_CROSSINGS;
            let (t0, t1) = (&self.table[p], &self.table[p + 1]);
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for k in 0..self.taps {
                let c = t0[k] + a * (t1[k] - t0[k]);
                l += c * self.hist_l[start + k];
                r += c * self.hist_r[start + k];
            }
            out_l.push(l);
            out_r.push(r);
            self.pos += self.step;
        }
        // Drop history we no longer need.
        let keep_from = (self.pos as usize + 1).saturating_sub(ZERO_CROSSINGS);
        if keep_from > 4096 {
            self.hist_l.drain(..keep_from);
            self.hist_r.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..60 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < 1e-14 * sum {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_44k1_to_48k_cleanly() {
        let mut rs = Resampler::new(44_100.0, 48_000.0);
        let n = 44_100;
        let x: Vec<f32> = (0..n).map(|i| (2.0 * PI * 1000.0 * i as f64 / 44_100.0).sin() as f32 * 0.5).collect();
        let (mut l, mut r) = (vec![], vec![]);
        rs.process(&x, &x, &mut l, &mut r);
        assert!((l.len() as i64 - 48_000).abs() < 100, "got {}", l.len());
        // 48 zero samples of history minus the 23-sample filter centre: 25 input samples of delay.
        let mut err = 0.0;
        for i in 2000..10_000 {
            let t = i as f64 / 48_000.0 - 25.0 / 44_100.0;
            let ideal = (2.0 * PI * 1000.0 * t).sin() * 0.5;
            err += (l[i] as f64 - ideal).powi(2);
        }
        let best = err / 8000.0;
        // Best-fit residual below -60 dB relative to the 0.5 amplitude sine.
        assert!(10.0 * best.log10() < -60.0, "residual {} dB", 10.0 * best.log10());
    }
}
