//! Dynamics: AGC, multiband compressor, look-ahead peak limiter and soft clipper.

use super::biquad::{lr4_allpass, Biquad, Lr4};
use crate::config::{AgcConfig, BandConfig, ClipperConfig, MultibandConfig};
use std::collections::VecDeque;

#[inline(always)]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline(always)]
pub fn lin_to_db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

fn coef_ms(ms: f32, fs: f32) -> f32 {
    1.0 - (-1.0 / (ms.max(0.01) * 0.001 * fs)).exp()
}

// ------------------------------------------------------------------ AGC

/// Slow wideband gain rider, stereo linked. Freezes below the gate.
pub struct Agc {
    fs: f32,
    cfg: AgcConfig,
    rms: f32,
    rms_coef: f32,
    gain_db: f32,
    gain_lin: f32,
    pub gated: bool,
}

impl Agc {
    pub fn new(fs: f32, cfg: &AgcConfig) -> Self {
        Self {
            fs,
            cfg: cfg.clone(),
            rms: 0.0,
            rms_coef: 1.0 - (-1.0 / (0.4 * fs)).exp(),
            gain_db: 0.0,
            gain_lin: 1.0,
            gated: true,
        }
    }

    pub fn set_config(&mut self, cfg: &AgcConfig) {
        self.cfg = cfg.clone();
    }

    pub fn gain_db(&self) -> f32 {
        if self.cfg.enabled { self.gain_db } else { 0.0 }
    }

    /// Process one block in place.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.cfg.enabled {
            return;
        }
        for i in 0..l.len() {
            let p = 0.5 * (l[i] * l[i] + r[i] * r[i]);
            self.rms += (p - self.rms) * self.rms_coef;
        }
        let level = 10.0 * self.rms.max(1e-12).log10() + 3.0; // sine-referenced dBFS
        let n = l.len() as f32;
        self.gated = level < self.cfg.gate_db;
        if !self.gated {
            let want = (self.cfg.target_db - level).clamp(self.cfg.min_gain_db, self.cfg.max_gain_db);
            let rate = if want < self.gain_db { self.cfg.attack_db_s } else { self.cfg.release_db_s };
            let step = rate * n / self.fs;
            self.gain_db += (want - self.gain_db).clamp(-step, step);
        }
        let target = db_to_lin(self.gain_db);
        let start = self.gain_lin;
        for i in 0..l.len() {
            let g = start + (target - start) * (i as f32 + 1.0) / n;
            l[i] *= g;
            r[i] *= g;
        }
        self.gain_lin = target;
    }
}

// ------------------------------------------------------------------ multiband

struct Band {
    cfg: BandConfig,
    att: f32,
    rel: f32,
    env_db: f32,
    gr_db: f32,
}

impl Band {
    fn new(fs: f32, cfg: &BandConfig) -> Self {
        let mut b = Self { cfg: cfg.clone(), att: 0.0, rel: 0.0, env_db: -120.0, gr_db: 0.0 };
        b.set(fs, cfg);
        b
    }

    fn set(&mut self, fs: f32, cfg: &BandConfig) {
        self.cfg = cfg.clone();
        self.att = coef_ms(cfg.attack_ms, fs);
        self.rel = coef_ms(cfg.release_ms, fs);
    }

    #[inline]
    fn gain(&mut self, l: f32, r: f32) -> f32 {
        let c = &self.cfg;
        let x = lin_to_db(l.abs().max(r.abs()));
        let k = if x > self.env_db { self.att } else { self.rel };
        self.env_db += (x - self.env_db) * k;
        let over = self.env_db - c.threshold_db;
        let slope = 1.0 - 1.0 / c.ratio;
        let half = c.knee_db * 0.5;
        let gr = if !c.enabled || over <= -half {
            0.0
        } else if over >= half || c.knee_db <= 0.0 {
            over * slope
        } else {
            (over + half) * (over + half) / (2.0 * c.knee_db) * slope
        };
        self.gr_db = gr;
        let makeup = if c.enabled { c.makeup_db } else { 0.0 };
        db_to_lin(makeup + c.gain_db - gr)
    }
}

/// Five-band compressor with Linkwitz-Riley crossovers and allpass phase compensation,
/// so that with all bands at 0 dB the sum has a flat magnitude response.
pub struct Multiband {
    fs: f32,
    enabled: bool,
    lp: [[Lr4; 2]; 4],
    hp: [[Lr4; 2]; 4],
    /// comp[b] = allpasses for crossovers above b+1 applied to band b.
    comp: Vec<Vec<[Biquad; 2]>>,
    bands: Vec<Band>,
    xo: [f32; 4],
}

impl Multiband {
    pub fn new(fs: f32, cfg: &MultibandConfig) -> Self {
        let xo = cfg.crossovers_hz;
        let mut mb = Self {
            fs,
            enabled: cfg.enabled,
            lp: [[Lr4::lowpass(fs, 1000.0); 2]; 4],
            hp: [[Lr4::highpass(fs, 1000.0); 2]; 4],
            comp: Vec::new(),
            bands: cfg.bands.iter().map(|b| Band::new(fs, b)).collect(),
            xo: [0.0; 4],
        };
        mb.set_crossovers(xo);
        mb
    }

    fn set_crossovers(&mut self, xo: [f32; 4]) {
        if xo == self.xo {
            return;
        }
        self.xo = xo;
        let fs = self.fs;
        for i in 0..4 {
            self.lp[i] = [Lr4::lowpass(fs, xo[i]); 2];
            self.hp[i] = [Lr4::highpass(fs, xo[i]); 2];
        }
        self.comp = (0..5)
            .map(|b| ((b + 1)..4).map(|k| [lr4_allpass(fs, xo[k]); 2]).collect())
            .collect();
    }

    pub fn set_config(&mut self, cfg: &MultibandConfig) {
        self.enabled = cfg.enabled;
        self.set_crossovers(cfg.crossovers_hz);
        for (b, c) in self.bands.iter_mut().zip(cfg.bands.iter()) {
            b.set(self.fs, c);
        }
    }

    pub fn gain_reduction_db(&self) -> [f32; 5] {
        let mut out = [0.0; 5];
        if self.enabled {
            for (o, b) in out.iter_mut().zip(self.bands.iter()) {
                *o = b.gr_db;
            }
        }
        out
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.enabled {
            return;
        }
        for i in 0..l.len() {
            let mut rest = [l[i], r[i]];
            let mut band = [[0.0f32; 2]; 5];
            for k in 0..4 {
                for c in 0..2 {
                    band[k][c] = self.lp[k][c].process(rest[c]);
                    rest[c] = self.hp[k][c].process(rest[c]);
                }
            }
            band[4] = rest;
            let (mut ol, mut or) = (0.0, 0.0);
            for b in 0..5 {
                for ap in self.comp[b].iter_mut() {
                    band[b][0] = ap[0].process(band[b][0]);
                    band[b][1] = ap[1].process(band[b][1]);
                }
                let g = self.bands[b].gain(band[b][0], band[b][1]);
                ol += band[b][0] * g;
                or += band[b][1] * g;
            }
            l[i] = ol;
            r[i] = or;
        }
    }
}

// ------------------------------------------------------------------ peak limiter

/// Look-ahead peak limiter for N linked channels. The output never exceeds the
/// ceiling: the gain is the box-filtered sliding minimum of the required gain,
/// which is always ≤ the required gain for the delayed sample.
pub struct PeakLimiter {
    channels: usize,
    la: usize,
    delay: Vec<VecDeque<f32>>,
    mins: VecDeque<(u64, f32)>,
    boxq: VecDeque<f32>,
    box_sum: f64,
    n: u64,
    gain: f32,
    rel: f32,
    pub ceiling: f32,
    /// Lowest gain since the last `take_min_gain`.
    min_gain: f32,
}

impl PeakLimiter {
    pub fn new(channels: usize, lookahead: usize, release_ms: f32, fs: f32, ceiling: f32) -> Self {
        let w = lookahead + 1;
        let mut delay = Vec::new();
        for _ in 0..channels {
            let mut d = VecDeque::with_capacity(w + 1);
            d.extend(std::iter::repeat(0.0).take(lookahead));
            delay.push(d);
        }
        let mut boxq = VecDeque::with_capacity(w + 1);
        boxq.extend(std::iter::repeat(1.0f32).take(w));
        Self {
            channels,
            la: lookahead,
            delay,
            mins: VecDeque::with_capacity(w + 1),
            boxq,
            box_sum: w as f64,
            n: 0,
            gain: 1.0,
            rel: coef_ms(release_ms, fs),
            ceiling,
            min_gain: 1.0,
        }
    }

    pub fn latency(&self) -> usize {
        self.la
    }

    pub fn set_release(&mut self, release_ms: f32, fs: f32) {
        self.rel = coef_ms(release_ms, fs);
    }

    pub fn take_min_gain(&mut self) -> f32 {
        let g = self.min_gain;
        self.min_gain = 1.0;
        g
    }

    /// Process one frame (one sample per channel) in place.
    #[inline]
    pub fn process(&mut self, frame: &mut [f32]) {
        let w = (self.la + 1) as u64;
        let mut peak = 0.0f32;
        for &x in frame.iter().take(self.channels) {
            peak = peak.max(x.abs());
        }
        let need = if peak > self.ceiling { self.ceiling / peak } else { 1.0 };
        // Sliding minimum over the last w required gains.
        while let Some(&(_, v)) = self.mins.back() {
            if v >= need {
                self.mins.pop_back();
            } else {
                break;
            }
        }
        self.mins.push_back((self.n, need));
        while let Some(&(i, _)) = self.mins.front() {
            if i + w <= self.n {
                self.mins.pop_front();
            } else {
                break;
            }
        }
        let m = self.mins.front().map(|x| x.1).unwrap_or(1.0);
        // Box filter of length w.
        self.boxq.push_back(m);
        self.box_sum += m as f64;
        self.box_sum -= self.boxq.pop_front().unwrap_or(1.0) as f64;
        if self.n % 65_536 == 0 {
            self.box_sum = self.boxq.iter().map(|&v| v as f64).sum();
        }
        let b = (self.box_sum / w as f64) as f32;
        // Release towards 1, never above the box-filtered target.
        let up = self.gain + (1.0 - self.gain) * self.rel;
        self.gain = up.min(b);
        self.min_gain = self.min_gain.min(self.gain);
        self.n += 1;
        let g = self.gain * 0.999_99;
        for (c, x) in frame.iter_mut().take(self.channels).enumerate() {
            self.delay[c].push_back(*x);
            *x = self.delay[c].pop_front().unwrap_or(0.0) * g;
        }
    }
}

// ------------------------------------------------------------------ clipper

pub struct Clipper {
    enabled: bool,
    drive: f32,
    knee: f32,
    /// Fraction of samples that hit the soft region since the last read.
    hits: u32,
    count: u32,
}

impl Clipper {
    pub fn new(cfg: &ClipperConfig) -> Self {
        let mut c = Self { enabled: true, drive: 1.0, knee: 0.85, hits: 0, count: 0 };
        c.set_config(cfg);
        c
    }

    pub fn set_config(&mut self, cfg: &ClipperConfig) {
        self.enabled = cfg.enabled;
        self.drive = db_to_lin(cfg.drive_db);
        self.knee = cfg.knee;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        if !self.enabled {
            return x.clamp(-1.0, 1.0);
        }
        let v = x * self.drive;
        let a = v.abs();
        self.count += 1;
        let k = self.knee;
        if a <= k {
            return v;
        }
        self.hits += 1;
        let span = 1.0 - k;
        let y = if span <= 1e-6 { 1.0 } else { k + span * ((a - k) / span).tanh() };
        y.min(1.0).copysign(v)
    }

    pub fn take_activity(&mut self) -> f32 {
        let a = if self.count == 0 { 0.0 } else { self.hits as f32 / self.count as f32 };
        self.hits = 0;
        self.count = 0;
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_never_overshoots() {
        let mut lim = PeakLimiter::new(2, 96, 50.0, 192_000.0, 0.8);
        let mut max_out = 0.0f32;
        let mut seed = 1u32;
        for i in 0..200_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            let burst = if (i / 5000) % 2 == 0 { 3.0 } else { 0.3 };
            let mut f = [noise * burst, -noise * burst * 0.7];
            lim.process(&mut f);
            max_out = max_out.max(f[0].abs()).max(f[1].abs());
        }
        assert!(max_out <= 0.8, "limiter overshoot: {max_out}");
    }

    #[test]
    fn multiband_sums_flat() {
        let mut cfg = MultibandConfig::default();
        for b in cfg.bands.iter_mut() {
            b.enabled = false; // no compression, no makeup
        }
        let fs = 48_000.0;
        for &f in &[60.0f32, 300.0, 1000.0, 4000.0, 10_000.0] {
            let mut mb = Multiband::new(fs, &cfg);
            let n = 48_000;
            let mut l: Vec<f32> = (0..n).map(|i| (2.0 * std::f32::consts::PI * f * i as f32 / fs).sin()).collect();
            let mut r = l.clone();
            mb.process(&mut l, &mut r);
            let peak = l[24_000..].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
            assert!((lin_to_db(peak)).abs() < 0.1, "{f} Hz: {} dB", lin_to_db(peak));
        }
    }
}
