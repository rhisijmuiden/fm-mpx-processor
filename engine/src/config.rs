//! Engine configuration. Everything the web interface can change lives here.
//!
//! The configuration is stored as JSON in SQLite and sent to the realtime
//! thread as a whole. `Config::sanitize` enforces the hard broadcast limits:
//! whatever the API sends, peak deviation can never exceed 75 kHz and the
//! BS.412 MPX power limit can never be set above +3 dBr.

use serde::{Deserialize, Serialize};

/// Absolute peak deviation that 100 % modulation represents.
pub const MAX_DEVIATION_KHZ: f32 = 75.0;
/// ITU-R BS.412 maximum MPX power.
pub const MAX_MPX_POWER_DBR: f32 = 3.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub input: InputConfig,
    pub processing: ProcessingConfig,
    pub stereo: StereoConfig,
    pub rds: RdsConfig,
    pub limits: LimitsConfig,
    pub outputs: OutputsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            input: InputConfig::default(),
            processing: ProcessingConfig::default(),
            stereo: StereoConfig::default(),
            rds: RdsConfig::default(),
            limits: LimitsConfig::default(),
            outputs: OutputsConfig::default(),
        }
    }
}

// ---------------------------------------------------------------- input

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Decoder {
    /// ffmpeg when installed (MP3, AAC-LC, HE-AAC v1/v2, Ogg, Opus), otherwise native.
    Auto,
    /// ffmpeg subprocess.
    Ffmpeg,
    /// Built-in Symphonia decoder (MP3, AAC-LC).
    Native,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct InputConfig {
    /// Primary Icecast/Shoutcast URL.
    pub url: String,
    /// Backup URL, used when the primary fails.
    pub backup_url: String,
    /// Local audio file played in a loop when both streams fail.
    pub fallback_file: String,
    pub decoder: Decoder,
    /// Target buffer between network and DSP, milliseconds.
    pub buffer_ms: u32,
    /// Input trim in dB.
    pub gain_db: f32,
    /// Seconds without audio before switching to the next source.
    pub failover_s: f32,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            backup_url: String::new(),
            fallback_file: String::new(),
            decoder: Decoder::Auto,
            buffer_ms: 1500,
            gain_db: 0.0,
            failover_s: 5.0,
        }
    }
}

// ---------------------------------------------------------------- processing

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProcessingConfig {
    /// Bypass the whole audio processing (input → stereo encoder). Limits stay active.
    pub bypass: bool,
    pub agc: AgcConfig,
    pub multiband: MultibandConfig,
    pub emphasis: Emphasis,
    pub limiter: LimiterConfig,
    pub clipper: ClipperConfig,
}

impl Default for ProcessingConfig {
    fn default() -> Self {
        Self {
            bypass: false,
            agc: AgcConfig::default(),
            multiband: MultibandConfig::default(),
            emphasis: Emphasis::Us50,
            limiter: LimiterConfig::default(),
            clipper: ClipperConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgcConfig {
    pub enabled: bool,
    /// Target RMS level, dBFS.
    pub target_db: f32,
    pub max_gain_db: f32,
    pub min_gain_db: f32,
    /// Below this RMS level the gain is frozen (silence / fades), dBFS.
    pub gate_db: f32,
    /// Speed of gain reduction, dB per second.
    pub attack_db_s: f32,
    /// Speed of gain increase, dB per second.
    pub release_db_s: f32,
}

impl Default for AgcConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target_db: -18.0,
            max_gain_db: 12.0,
            min_gain_db: -12.0,
            gate_db: -45.0,
            attack_db_s: 3.0,
            release_db_s: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MultibandConfig {
    pub enabled: bool,
    /// Four crossover frequencies (Hz) giving five bands.
    pub crossovers_hz: [f32; 4],
    pub bands: [BandConfig; 5],
}

impl Default for MultibandConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            crossovers_hz: [120.0, 500.0, 2000.0, 6000.0],
            bands: [
                BandConfig { threshold_db: -22.0, ratio: 3.0, attack_ms: 30.0, release_ms: 300.0, makeup_db: 3.0, ..Default::default() },
                BandConfig { threshold_db: -22.0, ratio: 3.0, attack_ms: 20.0, release_ms: 250.0, makeup_db: 3.0, ..Default::default() },
                BandConfig { threshold_db: -22.0, ratio: 3.0, attack_ms: 10.0, release_ms: 200.0, makeup_db: 3.0, ..Default::default() },
                BandConfig { threshold_db: -24.0, ratio: 3.0, attack_ms: 5.0, release_ms: 150.0, makeup_db: 3.0, ..Default::default() },
                BandConfig { threshold_db: -26.0, ratio: 3.0, attack_ms: 3.0, release_ms: 120.0, makeup_db: 3.0, ..Default::default() },
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BandConfig {
    pub enabled: bool,
    pub threshold_db: f32,
    pub ratio: f32,
    pub knee_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
    /// Band output level (mix), dB.
    pub gain_db: f32,
}

impl Default for BandConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_db: -22.0,
            ratio: 3.0,
            knee_db: 6.0,
            attack_ms: 10.0,
            release_ms: 200.0,
            makeup_db: 3.0,
            gain_db: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Emphasis {
    /// 50 µs (Europe).
    #[serde(rename = "50us")]
    Us50,
    /// 75 µs (Americas, Korea).
    #[serde(rename = "75us")]
    Us75,
    #[serde(rename = "off")]
    Off,
}

impl Emphasis {
    pub fn tau(self) -> Option<f32> {
        match self {
            Emphasis::Us50 => Some(50e-6),
            Emphasis::Us75 => Some(75e-6),
            Emphasis::Off => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LimiterConfig {
    pub enabled: bool,
    /// Output ceiling in dBFS before the clipper.
    pub ceiling_db: f32,
    /// Drive into the limiter, dB.
    pub drive_db: f32,
    pub release_ms: f32,
}

impl Default for LimiterConfig {
    fn default() -> Self {
        Self { enabled: true, ceiling_db: -1.0, drive_db: 2.0, release_ms: 80.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClipperConfig {
    pub enabled: bool,
    /// Drive into the clipper, dB. More drive = louder and more dense.
    pub drive_db: f32,
    /// Start of the soft knee as a fraction of the ceiling (0.5 - 1.0).
    pub knee: f32,
}

impl Default for ClipperConfig {
    fn default() -> Self {
        Self { enabled: true, drive_db: 1.0, knee: 0.85 }
    }
}

// ---------------------------------------------------------------- stereo

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StereoConfig {
    /// Stereo (pilot + L−R) or mono.
    pub stereo: bool,
    /// Pilot injection in percent of 75 kHz (8–10 % is usual).
    pub pilot_pct: f32,
    /// Pilot phase trim in degrees.
    pub pilot_phase_deg: f32,
}

impl Default for StereoConfig {
    fn default() -> Self {
        Self { stereo: true, pilot_pct: 9.0, pilot_phase_deg: 0.0 }
    }
}

// ---------------------------------------------------------------- RDS

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RdsConfig {
    pub enabled: bool,
    /// Injection of the 57 kHz subcarrier in percent of 75 kHz (3–5 % is usual).
    pub injection_pct: f32,
    /// Programme Identification, hex, e.g. "8201".
    pub pi: String,
    /// Programme Service name, 8 characters.
    pub ps: String,
    /// Static RadioText, up to 64 characters.
    pub rt: String,
    /// Dynamic RadioText from stream metadata.
    pub dynamic_rt: bool,
    /// Template for dynamic RadioText. `{title}`, `{artist}`, `{song}`, `{station}`.
    pub rt_template: String,
    /// Programme Type 0–31.
    pub pty: u8,
    pub tp: bool,
    pub ta: bool,
    /// Music (true) or speech (false).
    pub ms: bool,
    /// Alternative frequencies in MHz (max 25).
    pub af_mhz: Vec<f32>,
    /// Send clock time (group 4A) once per minute.
    pub ct: bool,
    /// Additional RDS2 subcarriers (66.5 / 71.25 / 76 kHz). Experimental.
    pub rds2: Rds2Config,
}

impl Default for RdsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            injection_pct: 4.0,
            pi: "8201".into(),
            ps: "PILOT".into(),
            rt: "PILOT FM processor".into(),
            dynamic_rt: true,
            rt_template: "{song}".into(),
            pty: 10,
            tp: false,
            ta: false,
            ms: true,
            af_mhz: vec![],
            ct: true,
            rds2: Rds2Config::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Rds2Config {
    pub enabled: bool,
    /// Number of extra streams (1–3).
    pub streams: u8,
    /// Injection per extra subcarrier, percent.
    pub injection_pct: f32,
}

impl Default for Rds2Config {
    fn default() -> Self {
        Self { enabled: false, streams: 3, injection_pct: 2.0 }
    }
}

// ---------------------------------------------------------------- limits

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LimitsConfig {
    /// Peak deviation ceiling, kHz. Clamped to ≤ 75.
    pub max_deviation_khz: f32,
    /// BS.412 MPX power limiter.
    pub mpx_power_enabled: bool,
    /// MPX power limit, dBr. Clamped to ≤ +3.
    pub mpx_power_dbr: f32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self { max_deviation_khz: 75.0, mpx_power_enabled: true, mpx_power_dbr: 3.0 }
    }
}

// ---------------------------------------------------------------- outputs

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OutputsConfig {
    pub micrompx: MicroMpxOutput,
    pub soundcard: SoundcardOutput,
    pub sdr: SdrOutput,
    pub stream: StreamOutput,
}

impl Default for OutputsConfig {
    fn default() -> Self {
        Self {
            micrompx: MicroMpxOutput::default(),
            soundcard: SoundcardOutput::default(),
            sdr: SdrOutput::default(),
            stream: StreamOutput::default(),
        }
    }
}

/// MPX to the official Thimeo MicroMPX encoder via an ALSA loopback at 192 kHz.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MicroMpxOutput {
    pub enabled: bool,
    /// ALSA device the engine writes to (playback side of snd-aloop).
    pub device: String,
    /// Level trim, dB (≤ 0). 0 dB = 100 % modulation at 0 dBFS.
    pub level_db: f32,
    /// URL of the encoder's web interface for status polling (optional).
    pub encoder_status_url: String,
}

impl Default for MicroMpxOutput {
    fn default() -> Self {
        Self {
            enabled: false,
            device: "hw:Loopback,0,0".into(),
            level_db: 0.0,
            encoder_status_url: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SoundcardMode {
    /// Full composite on one channel at 192 kHz (for a DC-coupled MPX sound card).
    Mpx,
    /// Processed, de-emphasised L/R audio at 48 kHz (monitoring or an external stereo coder).
    Audio,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SoundcardOutput {
    pub enabled: bool,
    pub device: String,
    pub mode: SoundcardMode,
    pub level_db: f32,
}

impl Default for SoundcardOutput {
    fn default() -> Self {
        Self { enabled: false, device: "default".into(), mode: SoundcardMode::Mpx, level_db: 0.0 }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IqFormat {
    /// Signed 8-bit interleaved I/Q (HackRF).
    Cs8,
    /// Signed 16-bit interleaved I/Q (LimeSDR, Pluto, SoapySDR).
    Cs16,
    /// 32-bit float interleaved I/Q.
    Cf32,
}

/// FM-modulated I/Q piped into an SDR transmit tool (hackrf_transfer, SoapySDR tools …).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SdrOutput {
    pub enabled: bool,
    /// I/Q sample rate = 192 kHz × this factor.
    pub upsample: u32,
    pub format: IqFormat,
    /// Command that reads I/Q from stdin. `{rate}` is replaced by the I/Q sample rate.
    pub command: String,
}

impl Default for SdrOutput {
    fn default() -> Self {
        Self {
            enabled: false,
            upsample: 10,
            format: IqFormat::Cs8,
            command: "hackrf_transfer -t /dev/stdin -f 87600000 -s {rate} -x 0".into(),
        }
    }
}

/// Processed audio encoded by ffmpeg and sent to an Icecast server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StreamOutput {
    pub enabled: bool,
    /// icecast://source:password@host:port/mount
    pub url: String,
    /// mp3 or aac
    pub codec: String,
    pub bitrate_kbps: u32,
}

impl Default for StreamOutput {
    fn default() -> Self {
        Self { enabled: false, url: String::new(), codec: "mp3".into(), bitrate_kbps: 192 }
    }
}

// ---------------------------------------------------------------- validation

fn clampf(v: &mut f32, lo: f32, hi: f32) {
    if !v.is_finite() {
        *v = lo;
    }
    *v = v.clamp(lo, hi);
}

impl Config {
    /// Bring every value into a safe range. Returns the list of fields that were changed.
    pub fn sanitize(&mut self) -> Vec<String> {
        let before = self.clone();
        let i = &mut self.input;
        i.buffer_ms = i.buffer_ms.clamp(300, 10_000);
        clampf(&mut i.gain_db, -24.0, 24.0);
        clampf(&mut i.failover_s, 1.0, 60.0);

        let p = &mut self.processing;
        clampf(&mut p.agc.target_db, -40.0, -6.0);
        clampf(&mut p.agc.max_gain_db, 0.0, 30.0);
        clampf(&mut p.agc.min_gain_db, -30.0, 0.0);
        clampf(&mut p.agc.gate_db, -80.0, -20.0);
        clampf(&mut p.agc.attack_db_s, 0.1, 50.0);
        clampf(&mut p.agc.release_db_s, 0.1, 50.0);
        let mut last = 40.0f32;
        for f in p.multiband.crossovers_hz.iter_mut() {
            clampf(f, last * 1.25, 15000.0);
            last = *f;
        }
        for b in p.multiband.bands.iter_mut() {
            clampf(&mut b.threshold_db, -60.0, 0.0);
            clampf(&mut b.ratio, 1.0, 20.0);
            clampf(&mut b.knee_db, 0.0, 24.0);
            clampf(&mut b.attack_ms, 0.1, 500.0);
            clampf(&mut b.release_ms, 5.0, 5000.0);
            clampf(&mut b.makeup_db, 0.0, 24.0);
            clampf(&mut b.gain_db, -24.0, 12.0);
        }
        clampf(&mut p.limiter.ceiling_db, -12.0, 0.0);
        clampf(&mut p.limiter.drive_db, -12.0, 12.0);
        clampf(&mut p.limiter.release_ms, 5.0, 2000.0);
        clampf(&mut p.clipper.drive_db, -6.0, 9.0);
        clampf(&mut p.clipper.knee, 0.5, 1.0);

        clampf(&mut self.stereo.pilot_pct, 6.0, 12.0);
        clampf(&mut self.stereo.pilot_phase_deg, -180.0, 180.0);

        let r = &mut self.rds;
        clampf(&mut r.injection_pct, 1.0, 10.0);
        r.pty = r.pty.min(31);
        if u16::from_str_radix(r.pi.trim(), 16).is_err() {
            r.pi = "8201".into();
        }
        r.pi = r.pi.trim().to_uppercase();
        r.ps = r.ps.chars().take(8).collect();
        r.rt = r.rt.chars().take(64).collect();
        r.af_mhz.retain(|f| (87.6..=107.9).contains(f));
        r.af_mhz.truncate(25);
        r.rds2.streams = r.rds2.streams.clamp(1, 3);
        clampf(&mut r.rds2.injection_pct, 0.5, 5.0);

        // Hard limits: never above 75 kHz and +3 dBr.
        clampf(&mut self.limits.max_deviation_khz, 40.0, MAX_DEVIATION_KHZ);
        clampf(&mut self.limits.mpx_power_dbr, -6.0, MAX_MPX_POWER_DBR);

        let o = &mut self.outputs;
        clampf(&mut o.micrompx.level_db, -40.0, 0.0);
        clampf(&mut o.soundcard.level_db, -40.0, 0.0);
        o.sdr.upsample = o.sdr.upsample.clamp(2, 40);
        o.stream.bitrate_kbps = o.stream.bitrate_kbps.clamp(32, 320);
        if o.stream.codec != "aac" {
            o.stream.codec = "mp3".into();
        }

        diff_fields(&serde_json::to_value(&before).unwrap(), &serde_json::to_value(&*self).unwrap(), "")
    }
}

fn diff_fields(a: &serde_json::Value, b: &serde_json::Value, path: &str) -> Vec<String> {
    use serde_json::Value;
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => x
            .iter()
            .flat_map(|(k, v)| {
                let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                y.get(k).map(|w| diff_fields(v, w, &p)).unwrap_or_default()
            })
            .collect(),
        _ if a != b => vec![path.to_string()],
        _ => vec![],
    }
}

/// Apply a JSON merge patch (RFC 7386) to a configuration.
pub fn merge_patch(target: &mut serde_json::Value, patch: &serde_json::Value) {
    use serde_json::Value;
    if let Value::Object(p) = patch {
        if !target.is_object() {
            *target = Value::Object(Default::default());
        }
        let t = target.as_object_mut().unwrap();
        for (k, v) in p {
            if v.is_null() {
                t.remove(k);
            } else {
                merge_patch(t.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
    } else {
        *target = patch.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_limits_cannot_be_exceeded() {
        let mut c = Config::default();
        c.limits.max_deviation_khz = 100.0;
        c.limits.mpx_power_dbr = 9.0;
        let changed = c.sanitize();
        assert_eq!(c.limits.max_deviation_khz, 75.0);
        assert_eq!(c.limits.mpx_power_dbr, 3.0);
        assert!(changed.contains(&"limits.max_deviation_khz".to_string()));
    }

    #[test]
    fn defaults_are_sane() {
        let mut c = Config::default();
        assert!(c.sanitize().is_empty(), "defaults must already be within range");
    }
}
