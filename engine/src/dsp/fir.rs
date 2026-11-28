//! FIR design (Kaiser-windowed sinc), a plain FIR filter and a polyphase interpolator.

use std::f64::consts::PI;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..50 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

/// Kaiser beta for a stop-band attenuation in dB.
pub fn kaiser_beta(atten_db: f64) -> f64 {
    if atten_db > 50.0 {
        0.1102 * (atten_db - 8.7)
    } else if atten_db >= 21.0 {
        0.5842 * (atten_db - 21.0).powf(0.4) + 0.07886 * (atten_db - 21.0)
    } else {
        0.0
    }
}

/// Number of taps for a transition width (Hz) at sample rate fs and attenuation.
pub fn kaiser_taps(fs: f64, transition_hz: f64, atten_db: f64) -> usize {
    let dw = 2.0 * PI * transition_hz / fs;
    let n = ((atten_db - 8.0) / (2.285 * dw)).ceil() as usize + 1;
    n | 1 // odd length, integer group delay
}

/// Windowed-sinc low-pass with cut-off `fc` (Hz, -6 dB point), unity DC gain.
pub fn design_lowpass(fs: f64, fc: f64, taps: usize, atten_db: f64) -> Vec<f32> {
    let beta = kaiser_beta(atten_db);
    let m = (taps - 1) as f64 / 2.0;
    let wc = 2.0 * fc / fs;
    let i0b = bessel_i0(beta);
    let mut h: Vec<f64> = (0..taps)
        .map(|n| {
            let t = n as f64 - m;
            let sinc = if t == 0.0 { wc } else { (PI * wc * t).sin() / (PI * t) };
            let r = t / m;
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b;
            sinc * w
        })
        .collect();
    let sum: f64 = h.iter().sum();
    for v in h.iter_mut() {
        *v /= sum;
    }
    h.into_iter().map(|v| v as f32).collect()
}

/// Real FIR filter on a single channel with a doubled delay line (no modulo in the inner loop).
#[derive(Clone)]
pub struct Fir {
    h: Vec<f32>,
    buf: Vec<f32>,
    pos: usize,
}

impl Fir {
    pub fn new(h: Vec<f32>) -> Self {
        let n = h.len();
        // Reverse so the dot product walks forward in time.
        let mut h = h;
        h.reverse();
        Self { h, buf: vec![0.0; 2 * n], pos: 0 }
    }

    pub fn len(&self) -> usize {
        self.h.len()
    }

    pub fn delay(&self) -> usize {
        (self.h.len() - 1) / 2
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let n = self.h.len();
        self.buf[self.pos] = x;
        self.buf[self.pos + n] = x;
        self.pos += 1;
        if self.pos == n {
            self.pos = 0;
        }
        dot(&self.h, &self.buf[self.pos..self.pos + n])
    }
}

#[inline(always)]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    // Eight accumulators so LLVM vectorises.
    let mut acc = [0.0f32; 8];
    let chunks = a.len() / 8;
    for i in 0..chunks {
        for k in 0..8 {
            acc[k] += a[i * 8 + k] * b[i * 8 + k];
        }
    }
    let mut s: f32 = acc.iter().sum();
    for i in chunks * 8..a.len() {
        s += a[i] * b[i];
    }
    s
}

/// Integer-factor polyphase interpolator (one channel).
#[derive(Clone)]
pub struct Interpolator {
    factor: usize,
    /// phases[p] holds the sub-filter for output phase p, reversed for forward dot products.
    phases: Vec<Vec<f32>>,
    taps_per_phase: usize,
    buf: Vec<f32>,
    pos: usize,
}

impl Interpolator {
    /// `h` is a low-pass designed at the output rate with unity DC gain.
    pub fn new(factor: usize, h: &[f32]) -> Self {
        let tpp = h.len().div_ceil(factor);
        let mut phases = vec![vec![0.0f32; tpp]; factor];
        for (i, &v) in h.iter().enumerate() {
            phases[i % factor][i / factor] = v * factor as f32;
        }
        for p in phases.iter_mut() {
            p.reverse();
        }
        Self { factor, phases, taps_per_phase: tpp, buf: vec![0.0; 2 * tpp], pos: 0 }
    }

    pub fn factor(&self) -> usize {
        self.factor
    }

    /// Push one input sample and write `factor` output samples.
    #[inline]
    pub fn process(&mut self, x: f32, out: &mut [f32]) {
        let n = self.taps_per_phase;
        self.buf[self.pos] = x;
        self.buf[self.pos + n] = x;
        self.pos += 1;
        if self.pos == n {
            self.pos = 0;
        }
        let window = &self.buf[self.pos..self.pos + n];
        for (p, o) in out.iter_mut().take(self.factor).enumerate() {
            *o = dot(&self.phases[p], window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_gain(h: &[f32], fs: f64, f: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &v) in h.iter().enumerate() {
            let w = 2.0 * PI * f / fs * n as f64;
            re += v as f64 * w.cos();
            im -= v as f64 * w.sin();
        }
        (re * re + im * im).sqrt()
    }

    #[test]
    fn lowpass_15k_rejects_pilot() {
        let fs = 192_000.0;
        let taps = kaiser_taps(fs, 3_000.0, 75.0);
        let h = design_lowpass(fs, 16_750.0, taps, 75.0);
        let pass = 20.0 * tone_gain(&h, fs, 15_000.0).log10();
        let stop = 20.0 * tone_gain(&h, fs, 19_000.0).log10();
        assert!(pass > -1.0, "passband at 15 kHz: {pass} dB");
        assert!(stop < -60.0, "19 kHz rejection: {stop} dB");
    }
}
