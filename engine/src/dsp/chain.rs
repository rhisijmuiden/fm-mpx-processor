//! The complete signal chain, from 48 kHz stereo input to 192 kHz MPX.
//!
//! ```text
//! 48 kHz:  trim → DC block → AGC → 5-band compressor
//!          → 4× interpolation (20 kHz)
//! 192 kHz: pre-emphasis → look-ahead limiter → soft clipper → 15 kHz low-pass (≥ 60 dB at 19 kHz)
//!          → M/S → stereo encoder → BS.412 → composite safety limiter → + pilot + RDS/RDS2
//! ```
//! A de-emphasised 48 kHz copy of the processed audio is tapped after the
//! 15 kHz low-pass for the stream output, monitoring and the output loudness meter.

use super::biquad::{Biquad, Coefs, DcBlock};
use super::dynamics::{db_to_lin, lin_to_db, Agc, Clipper, Multiband, PeakLimiter};
use super::fir::{design_lowpass, kaiser_taps, Fir, Interpolator};
use super::meters::{Loudness, Meters};
use super::mpx::{MpxEncoder, MPX_RATE};
use crate::config::{Config, MAX_DEVIATION_KHZ};
use crate::rds::encoder::RdsEncoder;
use crate::rds::modulator::RdsModulator;

pub const AUDIO_RATE: f32 = 48_000.0;
pub const UP: usize = 4;
/// Frames per block at 48 kHz (5 ms).
pub const BLOCK: usize = 240;
pub const MPX_BLOCK: usize = BLOCK * UP;

const RDS2_MULTS: [f64; 3] = [3.5, 3.75, 4.0];

pub struct Chain {
    cfg: Config,
    trim: f32,
    dc: [DcBlock; 2],
    agc: Agc,
    mb: Multiband,
    up: [Interpolator; 2],
    pre: [Biquad; 2],
    emphasis: bool,
    limiter: PeakLimiter,
    limiter_drive: f32,
    clipper: Clipper,
    lpf: [Fir; 2],
    de: [Biquad; 2],
    enc: MpxEncoder,
    rds: RdsModulator,
    rds2: Vec<RdsModulator>,
    rds_level: f32,
    rds2_level: f32,
    audio_scale: f32,
    // Scratch buffers (no allocation in the realtime path).
    l48: Vec<f32>,
    r48: Vec<f32>,
    l192: Vec<f32>,
    r192: Vec<f32>,
    m: Vec<f32>,
    s: Vec<f32>,
    tmp: [f32; UP],
    // Metering.
    ebu_in: Option<ebur128::EbuR128>,
    ebu_out: Option<ebur128::EbuR128>,
    interleave: Vec<f32>,
    in_peak: [f32; 2],
    out_peak: [f32; 2],
    dev_hold: f32,
    dev_hold_age: u32,
    blocks: u32,
    pub meters: Meters,
}

fn emphasis_filters(cfg: &Config) -> (bool, [Biquad; 2]) {
    match cfg.processing.emphasis.tau() {
        Some(tau) => {
            // Zero at 1/(2πτ); pole far above the audio band (keeps the curve within
            // 0.3 dB of the ideal up to 15 kHz while staying realisable).
            let tp = 1.0 / (2.0 * std::f64::consts::PI * 70_000.0);
            let c = Coefs::first_order_shelf(MPX_RATE as f32, tau as f64, tp);
            (true, [Biquad::new(c); 2])
        }
        None => (false, [Biquad::default(); 2]),
    }
}

fn deemphasis_filters(cfg: &Config) -> [Biquad; 2] {
    let tau = cfg.processing.emphasis.tau().unwrap_or(0.0) as f64;
    let tp = 1.0 / (2.0 * std::f64::consts::PI * 70_000.0);
    let c = Coefs::first_order_shelf(MPX_RATE as f32, tp, tau.max(tp));
    [Biquad::new(c); 2]
}

impl Chain {
    pub fn new(cfg: &Config) -> Self {
        let fs = MPX_RATE;
        let up_taps = kaiser_taps(fs, 8_000.0, 80.0);
        let up_h = design_lowpass(fs, 24_000.0, up_taps, 80.0);
        let lpf_taps = kaiser_taps(fs, 3_000.0, 75.0);
        let lpf_h = design_lowpass(fs, 16_750.0, lpf_taps, 75.0);
        let (emphasis, pre) = emphasis_filters(cfg);
        let stereo = cfg.stereo.stereo;
        let mut rds2 = Vec::new();
        for (i, &mult) in RDS2_MULTS.iter().enumerate() {
            let mut e = RdsEncoder::new(&cfg.rds, stereo);
            // Offset the group sequence so the streams do not carry identical data.
            for _ in 0..(i + 1) * 3 {
                e.next_group();
            }
            rds2.push(RdsModulator::new(e, mult));
        }
        let mut c = Self {
            cfg: cfg.clone(),
            trim: 1.0,
            dc: [DcBlock::default(); 2],
            agc: Agc::new(AUDIO_RATE, &cfg.processing.agc),
            mb: Multiband::new(AUDIO_RATE, &cfg.processing.multiband),
            up: [Interpolator::new(UP, &up_h), Interpolator::new(UP, &up_h)],
            pre,
            emphasis,
            limiter: PeakLimiter::new(2, 192, cfg.processing.limiter.release_ms, fs as f32, 1.0),
            limiter_drive: 1.0,
            clipper: Clipper::new(&cfg.processing.clipper),
            lpf: [Fir::new(lpf_h.clone()), Fir::new(lpf_h)],
            de: deemphasis_filters(cfg),
            enc: MpxEncoder::new(cfg.limits.mpx_power_enabled, cfg.limits.mpx_power_dbr),
            rds: RdsModulator::new(RdsEncoder::new(&cfg.rds, stereo), 3.0),
            rds2,
            rds_level: 0.0,
            rds2_level: 0.0,
            audio_scale: 0.87,
            l48: vec![0.0; BLOCK],
            r48: vec![0.0; BLOCK],
            l192: vec![0.0; MPX_BLOCK],
            r192: vec![0.0; MPX_BLOCK],
            m: vec![0.0; MPX_BLOCK],
            s: vec![0.0; MPX_BLOCK],
            tmp: [0.0; UP],
            ebu_in: ebur128::EbuR128::new(2, AUDIO_RATE as u32, ebur128::Mode::I | ebur128::Mode::S | ebur128::Mode::M).ok(),
            ebu_out: ebur128::EbuR128::new(2, AUDIO_RATE as u32, ebur128::Mode::I | ebur128::Mode::S | ebur128::Mode::M).ok(),
            interleave: vec![0.0; BLOCK * 2],
            in_peak: [0.0; 2],
            out_peak: [0.0; 2],
            dev_hold: 0.0,
            dev_hold_age: 0,
            blocks: 0,
            meters: Meters::default(),
        };
        c.set_config(cfg);
        c
    }

    /// Apply a new configuration. Filter states are kept where possible.
    pub fn set_config(&mut self, cfg: &Config) {
        let old = std::mem::replace(&mut self.cfg, cfg.clone());
        let p = &cfg.processing;
        self.trim = db_to_lin(cfg.input.gain_db);
        self.agc.set_config(&p.agc);
        self.mb.set_config(&p.multiband);
        if old.processing.emphasis != p.emphasis || !self.emphasis && p.emphasis.tau().is_some() {
            let (e, pre) = emphasis_filters(cfg);
            self.emphasis = e;
            self.pre = pre;
            self.de = deemphasis_filters(cfg);
        }
        self.limiter.ceiling = db_to_lin(p.limiter.ceiling_db);
        self.limiter.set_release(p.limiter.release_ms, MPX_RATE as f32);
        self.limiter_drive = if p.limiter.enabled { db_to_lin(p.limiter.drive_db) } else { 1.0 };
        self.clipper.set_config(&p.clipper);

        let stereo = cfg.stereo.stereo;
        let max = cfg.limits.max_deviation_khz / MAX_DEVIATION_KHZ;
        self.enc.max = max;
        self.enc.stereo = stereo;
        self.enc.pilot = cfg.stereo.pilot_pct / 100.0;
        self.enc.pilot_phase = cfg.stereo.pilot_phase_deg as f64;
        self.enc.bs412.enabled = cfg.limits.mpx_power_enabled;
        self.enc.bs412.limit_dbr = cfg.limits.mpx_power_dbr;

        self.rds_level = if cfg.rds.enabled { cfg.rds.injection_pct / 100.0 } else { 0.0 };
        self.rds2_level = if cfg.rds.enabled && cfg.rds.rds2.enabled { cfg.rds.rds2.injection_pct / 100.0 } else { 0.0 };
        self.rds.encoder.set_config(&cfg.rds, stereo);
        for m in self.rds2.iter_mut() {
            m.encoder.set_config(&cfg.rds, stereo);
        }
        let pilot = if stereo { self.enc.pilot } else { 0.0 };
        self.audio_scale = (max - pilot - self.rds_peak()).max(0.1);
    }

    fn rds_streams(&self) -> usize {
        if self.rds2_level > 0.0 { self.cfg.rds.rds2.streams as usize } else { 0 }
    }

    fn rds_peak(&self) -> f32 {
        self.rds_level + self.rds2_level * self.rds_streams() as f32
    }

    pub fn set_radiotext(&mut self, text: &str) {
        self.rds.encoder.set_rt(text);
        for m in self.rds2.iter_mut() {
            m.encoder.set_rt(text);
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn reset_loudness(&mut self) {
        self.ebu_in = ebur128::EbuR128::new(2, AUDIO_RATE as u32, ebur128::Mode::I | ebur128::Mode::S | ebur128::Mode::M).ok();
        self.ebu_out = ebur128::EbuR128::new(2, AUDIO_RATE as u32, ebur128::Mode::I | ebur128::Mode::S | ebur128::Mode::M).ok();
    }

    /// Process one block: `in_l/in_r` are BLOCK frames at 48 kHz. Writes MPX_BLOCK MPX samples
    /// and BLOCK de-emphasised processed frames (`tap_l/tap_r`).
    pub fn process(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        mpx: &mut [f32],
        tap_l: &mut [f32],
        tap_r: &mut [f32],
    ) {
        let bypass = self.cfg.processing.bypass;
        // ---- 48 kHz
        for i in 0..BLOCK {
            let l = self.dc[0].process(in_l[i] * self.trim);
            let r = self.dc[1].process(in_r[i] * self.trim);
            self.in_peak[0] = self.in_peak[0].max(l.abs());
            self.in_peak[1] = self.in_peak[1].max(r.abs());
            self.l48[i] = l;
            self.r48[i] = r;
        }
        if let Some(e) = self.ebu_in.as_mut() {
            for i in 0..BLOCK {
                self.interleave[2 * i] = self.l48[i];
                self.interleave[2 * i + 1] = self.r48[i];
            }
            let _ = e.add_frames_f32(&self.interleave);
        }
        if !bypass {
            self.agc.process(&mut self.l48, &mut self.r48);
            self.mb.process(&mut self.l48, &mut self.r48);
        }
        // ---- 4× up
        for i in 0..BLOCK {
            self.up[0].process(self.l48[i], &mut self.tmp);
            self.l192[i * UP..(i + 1) * UP].copy_from_slice(&self.tmp);
            self.up[1].process(self.r48[i], &mut self.tmp);
            self.r192[i * UP..(i + 1) * UP].copy_from_slice(&self.tmp);
        }
        // ---- 192 kHz audio
        let lim_on = self.cfg.processing.limiter.enabled && !bypass;
        let clip_on = !bypass;
        for i in 0..MPX_BLOCK {
            let mut f = [self.l192[i], self.r192[i]];
            if self.emphasis {
                f[0] = self.pre[0].process(f[0]);
                f[1] = self.pre[1].process(f[1]);
            }
            if lim_on {
                f[0] *= self.limiter_drive;
                f[1] *= self.limiter_drive;
                self.limiter.process(&mut f);
            }
            if clip_on {
                f[0] = self.clipper.process(f[0]);
                f[1] = self.clipper.process(f[1]);
            }
            let l = self.lpf[0].process(f[0]);
            let r = self.lpf[1].process(f[1]);
            self.m[i] = 0.5 * (l + r) * self.audio_scale;
            self.s[i] = 0.5 * (l - r) * self.audio_scale;
            // De-emphasised tap, decimated by 4 (already band-limited to 15 kHz).
            let dl = self.de[0].process(l);
            let dr = self.de[1].process(r);
            if i % UP == UP - 1 {
                tap_l[i / UP] = dl;
                tap_r[i / UP] = dr;
            }
        }
        // ---- composite
        let rds_level = self.rds_level;
        let rds2_level = self.rds2_level;
        let streams = self.rds_streams();
        let rds_peak = self.rds_peak();
        let rds = &mut self.rds;
        let rds2 = &mut self.rds2;
        self.enc.process(
            &self.m,
            &self.s,
            rds_peak,
            |clock| {
                if rds_level <= 0.0 {
                    return 0.0;
                }
                let t = clock.total_cycles();
                let mut v = rds.sample(t) * rds_level;
                for m in rds2.iter_mut().take(streams) {
                    v += m.sample(t) * rds2_level;
                }
                v
            },
            mpx,
        );
        // ---- metering
        for i in 0..BLOCK {
            self.out_peak[0] = self.out_peak[0].max(tap_l[i].abs());
            self.out_peak[1] = self.out_peak[1].max(tap_r[i].abs());
        }
        if let Some(e) = self.ebu_out.as_mut() {
            for i in 0..BLOCK {
                self.interleave[2 * i] = tap_l[i];
                self.interleave[2 * i + 1] = tap_r[i];
            }
            let _ = e.add_frames_f32(&self.interleave);
        }
        self.blocks += 1;
        if self.blocks % 7 == 0 {
            self.publish_meters();
        }
    }

    fn publish_meters(&mut self) {
        let loud = |e: &Option<ebur128::EbuR128>| {
            let f = |r: Result<f64, ebur128::Error>| r.ok().filter(|v| v.is_finite()).unwrap_or(-70.0).max(-70.0) as f32;
            match e {
                Some(e) => Loudness {
                    momentary: f(e.loudness_momentary()),
                    short_term: f(e.loudness_shortterm()),
                    integrated: f(e.loudness_global()),
                },
                None => Loudness::default(),
            }
        };
        let dev = self.enc.take_peak() * MAX_DEVIATION_KHZ;
        self.dev_hold_age += 1;
        if dev >= self.dev_hold || self.dev_hold_age > 57 {
            self.dev_hold = dev;
            self.dev_hold_age = 0;
        }
        let rds_peak = self.rds_peak();
        let m = &mut self.meters;
        m.seq += 1;
        m.input_peak_db = [lin_to_db(self.in_peak[0]), lin_to_db(self.in_peak[1])];
        m.output_peak_db = [lin_to_db(self.out_peak[0]), lin_to_db(self.out_peak[1])];
        self.in_peak = [0.0; 2];
        self.out_peak = [0.0; 2];
        m.input_loudness = loud(&self.ebu_in);
        m.output_loudness = loud(&self.ebu_out);
        m.agc_gain_db = self.agc.gain_db();
        m.agc_gated = self.agc.gated;
        m.band_gr_db = self.mb.gain_reduction_db();
        m.limiter_gr_db = -lin_to_db(self.limiter.take_min_gain());
        m.clipper_activity = self.clipper.take_activity();
        m.deviation_khz = dev;
        m.deviation_hold_khz = self.dev_hold;
        m.mpx_power_dbr = self.enc.bs412.power_dbr();
        m.bs412_gain_db = self.enc.bs412.gain_db();
        m.composite_limiter_db = self.enc.limiter_gain_db();
        m.hard_clips = self.enc.hard_clips;
        m.pilot_khz = if self.cfg.stereo.stereo { self.enc.pilot * MAX_DEVIATION_KHZ } else { 0.0 };
        m.rds_khz = rds_peak * MAX_DEVIATION_KHZ;
        m.max_deviation_khz = self.cfg.limits.max_deviation_khz;
        m.mpx_power_limit_dbr = self.cfg.limits.mpx_power_dbr;
        if m.rds_ps.is_empty() || self.blocks % 70 == 0 {
            m.rds_ps = self.rds.encoder.ps();
            m.rds_rt = self.rds.encoder.rt().to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn tone_power(x: &[f32], fs: f64, f: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &v) in x.iter().enumerate() {
            let w = 2.0 * PI * f / fs * n as f64;
            re += v as f64 * w.cos();
            im += v as f64 * w.sin();
        }
        2.0 * (re * re + im * im).sqrt() / x.len() as f64
    }

    #[test]
    fn full_chain_respects_limits_and_has_pilot() {
        let cfg = Config::default();
        let mut chain = Chain::new(&cfg);
        let mut mpx = vec![0.0f32; MPX_BLOCK];
        let (mut tl, mut tr) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        let mut all = Vec::new();
        let mut seed = 3u32;
        let mut max_peak = 0.0f32;
        // 20 s of loud stereo noise + tones.
        for b in 0..(20 * 200) {
            let mut l = vec![0.0; BLOCK];
            let mut r = vec![0.0; BLOCK];
            for i in 0..BLOCK {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let n = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
                let t = (b * BLOCK + i) as f32 / 48_000.0;
                l[i] = 0.9 * n + 0.5 * (2.0 * std::f32::consts::PI * 8_000.0 * t).sin();
                r[i] = 0.9 * n * -0.5 + 0.5 * (2.0 * std::f32::consts::PI * 120.0 * t).sin();
            }
            chain.process(&l, &r, &mut mpx, &mut tl, &mut tr);
            max_peak = max_peak.max(mpx.iter().fold(0.0f32, |a, &v| a.max(v.abs())));
            if b >= 19 * 200 {
                all.extend_from_slice(&mpx);
            }
        }
        assert!(max_peak <= 1.0, "peak deviation {} kHz", max_peak * 75.0);
        assert_eq!(chain.enc.hard_clips, 0);
        let pilot = tone_power(&all, MPX_RATE, 19_000.0);
        assert!((pilot - 0.09).abs() < 0.004, "pilot level {pilot}");
        // The 15 kHz low-pass must keep audio out of the pilot region (18.5–19.5 kHz).
        let guard = tone_power(&all, MPX_RATE, 18_600.0);
        assert!(guard < 0.002, "audio leaking near the pilot: {guard}");
    }

    #[test]
    fn mono_has_no_pilot() {
        let mut cfg = Config::default();
        cfg.stereo.stereo = false;
        cfg.rds.enabled = false;
        let mut chain = Chain::new(&cfg);
        let mut mpx = vec![0.0f32; MPX_BLOCK];
        let (mut tl, mut tr) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        let z = vec![0.0; BLOCK];
        let mut all = Vec::new();
        for _ in 0..400 {
            chain.process(&z, &z, &mut mpx, &mut tl, &mut tr);
            all.extend_from_slice(&mpx);
        }
        assert!(tone_power(&all, MPX_RATE, 19_000.0) < 1e-5);
    }

    #[test]
    fn emphasis_curve_matches_ideal() {
        let tau = 50e-6;
        let tp = 1.0 / (2.0 * PI * 70_000.0);
        let c = Coefs::first_order_shelf(MPX_RATE as f32, tau, tp);
        for &f in &[1000.0f64, 5000.0, 10_000.0, 15_000.0] {
            let ideal = (1.0 + (2.0 * PI * f * tau).powi(2)).sqrt();
            let got = c.magnitude(MPX_RATE as f32, f as f32) as f64;
            let err = 20.0 * (got / ideal).log10();
            assert!(err.abs() < 0.5, "{f} Hz: {err} dB");
        }
    }
}
