//! Meter snapshot shared between the realtime thread and the API.

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Loudness {
    /// Momentary loudness (400 ms), LUFS.
    pub momentary: f32,
    /// Short-term loudness (3 s), LUFS.
    pub short_term: f32,
    /// Integrated loudness since start or reset, LUFS.
    pub integrated: f32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Meters {
    /// Monotonic counter, increments with every snapshot.
    pub seq: u64,
    /// Input peak level, dBFS (L, R).
    pub input_peak_db: [f32; 2],
    pub input_loudness: Loudness,
    /// Processed audio (de-emphasised) loudness.
    pub output_loudness: Loudness,
    pub output_peak_db: [f32; 2],
    pub agc_gain_db: f32,
    pub agc_gated: bool,
    /// Gain reduction per band, dB (positive = reduction).
    pub band_gr_db: [f32; 5],
    pub limiter_gr_db: f32,
    /// Fraction of samples in the clipper's soft region (0–1).
    pub clipper_activity: f32,
    /// Peak deviation over the last meter interval, kHz.
    pub deviation_khz: f32,
    /// Peak deviation hold over 2 seconds, kHz.
    pub deviation_hold_khz: f32,
    /// Integrated 60 s MPX power (ITU-R BS.412), dBr.
    pub mpx_power_dbr: f32,
    /// Gain the BS.412 controller applies, dB (≤ 0).
    pub bs412_gain_db: f32,
    /// Composite safety limiter gain, dB (≤ 0).
    pub composite_limiter_db: f32,
    /// Samples that reached the last-resort hard clip (should stay 0).
    pub hard_clips: u64,
    pub pilot_khz: f32,
    pub rds_khz: f32,
    pub max_deviation_khz: f32,
    pub mpx_power_limit_dbr: f32,
    pub rds_ps: String,
    pub rds_rt: String,
    /// Processing load of the realtime thread (0–1).
    pub dsp_load: f32,
}
