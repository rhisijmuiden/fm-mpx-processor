//! SDR output: FM-modulates the MPX into I/Q and pipes it into a transmit tool
//! (hackrf_transfer, a SoapySDR streamer …). Peak deviation follows the MPX scale:
//! 1.0 = 75 kHz.
//!
//! Transmitting on air requires a licence. Use a dummy load or a shielded setup for tests.

use super::SharedStatus;
use crate::config::{IqFormat, SdrOutput, MAX_DEVIATION_KHZ};
use crate::dsp::fir::{design_lowpass, kaiser_taps, Interpolator};
use crate::dsp::mpx::MPX_RATE;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct FmModulator {
    interp: Interpolator,
    phase: f64,
    k: f64,
    tmp: Vec<f32>,
}

impl FmModulator {
    pub fn new(upsample: usize) -> Self {
        let fs = MPX_RATE * upsample as f64;
        let taps = kaiser_taps(fs, 30_000.0, 70.0);
        let h = design_lowpass(fs, 96_000.0, taps, 70.0);
        Self {
            interp: Interpolator::new(upsample, &h),
            phase: 0.0,
            k: 2.0 * std::f64::consts::PI * MAX_DEVIATION_KHZ as f64 * 1000.0 / fs,
            tmp: vec![0.0; upsample],
        }
    }

    /// Modulate MPX samples into interleaved I/Q floats.
    pub fn process(&mut self, mpx: &[f32], out: &mut Vec<f32>) {
        for &x in mpx {
            self.interp.process(x, &mut self.tmp);
            for &v in &self.tmp {
                self.phase += self.k * v as f64;
                if self.phase > std::f64::consts::PI {
                    self.phase -= 2.0 * std::f64::consts::PI;
                } else if self.phase < -std::f64::consts::PI {
                    self.phase += 2.0 * std::f64::consts::PI;
                }
                let (s, c) = self.phase.sin_cos();
                out.push(c as f32);
                out.push(s as f32);
            }
        }
    }
}

pub fn encode(iq: &[f32], fmt: IqFormat, bytes: &mut Vec<u8>) {
    bytes.clear();
    match fmt {
        IqFormat::Cs8 => bytes.extend(iq.iter().map(|&v| ((v * 127.0) as i8) as u8)),
        IqFormat::Cs16 => {
            for &v in iq {
                bytes.extend_from_slice(&((v * 32_767.0) as i16).to_le_bytes());
            }
        }
        IqFormat::Cf32 => {
            for &v in iq {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
}

pub fn spawn(cfg: SdrOutput, mut consumer: rtrb::Consumer<f32>, st: SharedStatus, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new().name("pilot-sdr".into()).spawn(move || {
        let rate = (MPX_RATE as u32) * cfg.upsample;
        let cmdline = cfg.command.replace("{rate}", &rate.to_string());
        {
            let mut s = st.lock();
            s.sample_rate = rate;
            s.format = format!("{:?}", cfg.format).to_lowercase();
        }
        while !stop.load(Ordering::SeqCst) {
            let mut child = match Command::new("/bin/sh")
                .arg("-c")
                .arg(&cmdline)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    st.lock().error = e.to_string();
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
            };
            let mut stdin = child.stdin.take().unwrap();
            {
                let mut s = st.lock();
                s.running = true;
                s.error.clear();
            }
            let mut fm = FmModulator::new(cfg.upsample as usize);
            let mut iq = Vec::new();
            let mut bytes = Vec::new();
            let mut buf = vec![0.0f32; 960];
            while !stop.load(Ordering::SeqCst) {
                if consumer.slots() < 960 {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                if let Ok(chunk) = consumer.read_chunk(960) {
                    let (a, b) = chunk.as_slices();
                    buf[..a.len()].copy_from_slice(a);
                    buf[a.len()..a.len() + b.len()].copy_from_slice(b);
                    chunk.commit_all();
                }
                iq.clear();
                fm.process(&buf, &mut iq);
                encode(&iq, cfg.format, &mut bytes);
                if let Err(e) = stdin.write_all(&bytes) {
                    st.lock().error = format!("SDR tool stopped: {e}");
                    break;
                }
            }
            let _ = child.kill();
            let _ = child.wait();
            st.lock().running = false;
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deviation_matches_scale() {
        // A DC MPX of 1.0 must shift the carrier by exactly 75 kHz.
        let mut fm = FmModulator::new(10);
        let mut iq = Vec::new();
        fm.process(&vec![1.0f32; 9600], &mut iq);
        let fs = 1_920_000.0;
        let n = iq.len() / 2;
        let ph = |i: usize| (iq[2 * i + 1] as f64).atan2(iq[2 * i] as f64);
        let mut total = 0.0;
        for i in (n / 2)..(n - 1) {
            let mut d = ph(i + 1) - ph(i);
            if d > std::f64::consts::PI { d -= 2.0 * std::f64::consts::PI; }
            if d < -std::f64::consts::PI { d += 2.0 * std::f64::consts::PI; }
            total += d;
        }
        let f = total / ((n - 1 - n / 2) as f64) * fs / (2.0 * std::f64::consts::PI);
        assert!((f - 75_000.0).abs() < 50.0, "{f}");
    }
}
