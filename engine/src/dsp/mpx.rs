//! Stereo encoder and the composite safety stage.
//!
//! Scale: an MPX sample of 1.0 is 100 % modulation = the configured maximum
//! deviation (never more than 75 kHz). The audio part of the composite is
//! limited to `max - pilot - rds`, then pilot and RDS are added, so peak
//! deviation can never exceed the limit. A BS.412 controller reduces the
//! audio gain when the 60-second MPX power would exceed the limit.

use super::dynamics::PeakLimiter;
use crate::config::MAX_DEVIATION_KHZ;
use std::f64::consts::TAU;

pub const MPX_RATE: f64 = 192_000.0;
pub const PILOT_HZ: f64 = 19_000.0;

/// Reference power for 0 dBr: a sine giving ±19 kHz deviation, relative to 75 kHz = 1.0.
pub fn bs412_ref_power() -> f64 {
    let a = 19.0 / MAX_DEVIATION_KHZ as f64;
    a * a / 2.0
}

/// Phase accumulator locked to the 19 kHz pilot. Exact: counts samples and
/// derives phase as n·19000/192000 cycles (reduced fraction 19/192).
#[derive(Clone, Copy)]
pub struct PilotClock {
    n: u64,
}

impl PilotClock {
    pub fn starting_at(n: u64) -> Self {
        Self { n }
    }
    /// Pilot phase in cycles (0..1).
    #[inline]
    pub fn cycles(&self) -> f64 {
        ((self.n % 192) as f64 * 19.0 / 192.0).fract()
    }
    /// Total pilot cycles elapsed (for the RDS bit clock).
    #[inline]
    pub fn total_cycles(&self) -> f64 {
        (self.n / 192) as f64 * 19.0 + (self.n % 192) as f64 * 19.0 / 192.0
    }
    #[inline]
    pub fn tick(&mut self) {
        self.n += 1;
    }
}

/// BS.412 MPX power controller.
pub struct Bs412 {
    pub enabled: bool,
    pub limit_dbr: f32,
    samples_per_bin: usize,
    bins: [f64; 60],
    bin_idx: usize,
    cur_energy: f64,
    cur_audio: f64,
    cur_count: usize,
    filled: usize,
    /// Slow gain (dB, ≤ 0) applied to the audio.
    slow_db: f64,
    /// Fast emergency gain (linear, ≤ 1).
    fast: f64,
    total_energy: f64,
}

impl Bs412 {
    pub fn new(enabled: bool, limit_dbr: f32) -> Self {
        Self {
            enabled,
            limit_dbr,
            samples_per_bin: MPX_RATE as usize,
            bins: [0.0; 60],
            bin_idx: 0,
            cur_energy: 0.0,
            cur_audio: 0.0,
            cur_count: 0,
            filled: 0,
            slow_db: 0.0,
            fast: 1.0,
            total_energy: 0.0,
        }
    }

    pub fn gain(&self) -> f32 {
        if !self.enabled {
            return 1.0;
        }
        (10f64.powf(self.slow_db / 20.0) * self.fast) as f32
    }

    pub fn gain_db(&self) -> f32 {
        20.0 * self.gain().max(1e-6).log10()
    }

    /// Integrated MPX power over the last 60 s (or what we have), dBr.
    pub fn power_dbr(&self) -> f32 {
        let bins = self.filled.min(59);
        let mut energy: f64 = self.bins.iter().take(60).sum::<f64>();
        let mut n = (bins * self.samples_per_bin) as f64;
        energy += self.cur_energy;
        n += self.cur_count as f64;
        if n <= 0.0 {
            return -60.0;
        }
        (10.0 * (energy / n / bs412_ref_power()).max(1e-9).log10()) as f32
    }

    /// Feed one block: `audio_energy` is Σ audio² before the BS.412 gain,
    /// `fixed_energy` the pilot/RDS part, `out_energy` Σ mpx² of the final output.
    /// Updates the gain for the next block.
    pub fn update(&mut self, audio_energy: f64, fixed_energy: f64, out_energy: f64, n: usize) {
        self.cur_energy += out_energy;
        self.cur_audio += audio_energy;
        self.cur_count += n;
        self.total_energy += out_energy;
        let limit = bs412_ref_power() * 10f64.powf(self.limit_dbr as f64 / 10.0);
        if self.cur_count >= self.samples_per_bin {
            self.bins[self.bin_idx] = self.cur_energy;
            self.bin_idx = (self.bin_idx + 1) % 60;
            self.filled += 1;
            // Slow loop, once per second: steer to 0.3 dB below the limit.
            let p = self.power_dbr() as f64;
            let err = p - (self.limit_dbr as f64 - 0.3);
            if err > 0.0 {
                self.slow_db -= (0.5 * err).min(3.0);
            } else {
                self.slow_db += (-0.5 * err).min(0.2);
            }
            self.slow_db = self.slow_db.clamp(-15.0, 0.0);
            self.bins[self.bin_idx] = 0.0;
            self.cur_energy = 0.0;
            self.cur_audio = 0.0;
            self.cur_count = 0;
        }
        if !self.enabled {
            self.fast = 1.0;
            return;
        }
        // Fast loop: the energy still allowed in this one-second bin so that the
        // 60-second window stays ≤ the limit. Spread over the rest of the bin.
        let window_samples = (60 * self.samples_per_bin) as f64;
        let older: f64 = (0..59).map(|k| self.bins[(self.bin_idx + 60 - 1 - k) % 60]).sum();
        let older_n = (self.filled.min(59) * self.samples_per_bin) as f64;
        let allowed_total = limit * (older_n + self.samples_per_bin as f64).min(window_samples);
        let remaining_energy = allowed_total - older - self.cur_energy;
        let remaining_samples = (self.samples_per_bin - self.cur_count).max(1) as f64;
        let budget_rate = remaining_energy / remaining_samples;
        let nf = n.max(1) as f64;
        let audio_rate = audio_energy / nf;
        let fixed_rate = fixed_energy / nf;
        let g_slow2 = 10f64.powf(self.slow_db / 10.0);
        let projected = g_slow2 * audio_rate + fixed_rate;
        let target = if projected > budget_rate && audio_rate > 0.0 {
            (((budget_rate - fixed_rate).max(0.0)) / (g_slow2 * audio_rate)).sqrt().min(1.0)
        } else {
            1.0
        };
        // Attack immediately, release gently.
        self.fast = if target < self.fast { target } else { self.fast + (target - self.fast) * 0.02 };
    }
}

/// Stereo encoder + MPX safety + pilot. RDS samples are supplied by the caller.
pub struct MpxEncoder {
    pub stereo: bool,
    pub pilot: f32,
    pub pilot_phase: f64,
    pub max: f32,
    limiter: PeakLimiter,
    audio_clock: PilotClock,
    /// Lags the audio clock by the limiter latency so pilot, 38 kHz and 57 kHz stay coherent.
    out_clock: PilotClock,
    pub bs412: Bs412,
    pub peak: f32,
    pub hard_clips: u64,
}

impl MpxEncoder {
    pub fn new(bs412_enabled: bool, bs412_dbr: f32) -> Self {
        let la = 96; // 0.5 ms
        Self {
            stereo: true,
            pilot: 0.09,
            pilot_phase: 0.0,
            max: 1.0,
            limiter: PeakLimiter::new(1, la, 20.0, MPX_RATE as f32, 0.87),
            audio_clock: PilotClock::starting_at(la as u64),
            out_clock: PilotClock::starting_at(0),
            bs412: Bs412::new(bs412_enabled, bs412_dbr),
            peak: 0.0,
            hard_clips: 0,
        }
    }

    pub fn limiter_gain_db(&mut self) -> f32 {
        20.0 * self.limiter.take_min_gain().max(1e-6).log10()
    }

    /// Encode a block. `m`/`s` are (L+R)/2 and (L−R)/2 at 192 kHz.
    /// `rds` supplies the subcarrier sum for a given output clock (already scaled),
    /// with its peak amplitude in `rds_peak`. Output written to `out`.
    pub fn process<F: FnMut(&PilotClock) -> f32>(
        &mut self,
        m: &[f32],
        s: &[f32],
        rds_peak: f32,
        mut rds: F,
        out: &mut [f32],
    ) {
        let pilot = if self.stereo { self.pilot } else { 0.0 };
        self.limiter.ceiling = (self.max - pilot - rds_peak).max(0.1);
        let g = self.bs412.gain();
        let phase0 = self.pilot_phase / 360.0;
        let (mut audio_e, mut fixed_e, mut out_e) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..m.len() {
            let ph = (self.audio_clock.cycles() + phase0) * TAU;
            let sub = if self.stereo { s[i] * (2.0 * ph).sin() as f32 } else { 0.0 };
            let raw = m[i] + sub;
            audio_e += (raw as f64) * (raw as f64);
            let mut frame = [raw * g];
            self.limiter.process(&mut frame);
            self.audio_clock.tick();

            let oph = (self.out_clock.cycles() + phase0) * TAU;
            let p = pilot * oph.sin() as f32;
            let r = rds(&self.out_clock);
            fixed_e += ((p + r) as f64).powi(2);
            let mut y = frame[0] + p + r;
            if y.abs() > self.max {
                self.hard_clips += 1;
                y = y.clamp(-self.max, self.max);
            }
            self.peak = self.peak.max(y.abs());
            out_e += (y as f64) * (y as f64);
            out[i] = y;
            self.out_clock.tick();
        }
        self.bs412.update(audio_e, fixed_e, out_e, m.len());
    }

    pub fn take_peak(&mut self) -> f32 {
        let p = self.peak;
        self.peak = 0.0;
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deviation_and_power_stay_within_limits() {
        let mut enc = MpxEncoder::new(true, 3.0);
        let n = 960;
        let mut max_peak = 0.0f32;
        let mut seed = 7u32;
        let mut out = vec![0.0f32; n];
        // 90 seconds of hot, dense, wide stereo noise (worst case for BS.412).
        for _ in 0..(90 * 200) {
            let mut m = vec![0.0; n];
            let mut s = vec![0.0; n];
            for i in 0..n {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let a = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let b = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
                m[i] = a * 1.2;
                s[i] = b * 1.2;
            }
            enc.process(&m, &s, 0.04, |c| 0.04 * ((c.total_cycles() * 3.0).fract() * TAU).sin() as f32, &mut out);
            max_peak = max_peak.max(enc.take_peak());
        }
        assert!(max_peak <= 1.0, "peak {max_peak}");
        assert_eq!(enc.hard_clips, 0, "safety limiter let peaks through to the hard clip");
        let p = enc.bs412.power_dbr();
        assert!(p <= 3.0 + 0.05, "MPX power {p} dBr");
    }
}
