//! ALSA playback: the MicroMPX loopback (192 kHz MPX) and sound cards.

use super::{MasterSink, SharedStatus};
use crate::config::SoundcardMode;
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct AlsaMaster {
    pcm: PCM,
    mode: SoundcardMode,
    gain: f32,
    pub rate: u32,
    pub format: String,
    s32: bool,
    buf32: Vec<i32>,
    buf16: Vec<i16>,
}

fn open_pcm(device: &str, rate: u32, period: usize) -> anyhow::Result<(PCM, bool)> {
    let pcm = PCM::new(device, Direction::Playback, false)?;
    let s32;
    {
        let hwp = HwParams::any(&pcm)?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_channels(2)?;
        s32 = hwp.set_format(Format::S32LE).is_ok();
        if !s32 {
            hwp.set_format(Format::S16LE)?;
        }
        hwp.set_rate(rate, ValueOr::Nearest)?;
        hwp.set_period_size_near(period as alsa::pcm::Frames, ValueOr::Nearest)?;
        hwp.set_buffer_size_near((period * 4) as alsa::pcm::Frames)?;
        pcm.hw_params(&hwp)?;
        let got = hwp.get_rate()?;
        if got != rate {
            anyhow::bail!("device runs at {got} Hz, {rate} Hz required");
        }
    }
    pcm.prepare()?;
    Ok((pcm, s32))
}

fn rate_for(mode: SoundcardMode) -> (u32, usize) {
    match mode {
        SoundcardMode::Mpx => (192_000, 960),
        SoundcardMode::Audio => (48_000, 240),
    }
}

impl AlsaMaster {
    pub fn open(device: &str, mode: SoundcardMode, gain: f32) -> anyhow::Result<Self> {
        let (rate, period) = rate_for(mode);
        let (pcm, s32) = open_pcm(device, rate, period)?;
        Ok(Self {
            pcm,
            mode,
            gain,
            rate,
            format: if s32 { "S32_LE".into() } else { "S16_LE".into() },
            s32,
            buf32: Vec::with_capacity(period * 2),
            buf16: Vec::with_capacity(period * 2),
        })
    }

    fn write_frames(&mut self, frames: usize) -> anyhow::Result<()> {
        let mut done = 0;
        while done < frames {
            let res = if self.s32 {
                self.pcm.io_i32()?.writei(&self.buf32[done * 2..])
            } else {
                self.pcm.io_i16()?.writei(&self.buf16[done * 2..])
            };
            match res {
                Ok(n) => done += n,
                Err(e) => {
                    // Underrun or suspend: recover and continue.
                    self.pcm.try_recover(e, true)?;
                }
            }
        }
        Ok(())
    }
}

#[inline]
fn to_i32(v: f32) -> i32 {
    (v.clamp(-1.0, 1.0) as f64 * 2_147_483_647.0) as i32
}

#[inline]
fn to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32_767.0) as i16
}

impl MasterSink for AlsaMaster {
    fn write(&mut self, mpx: &[f32], l: &[f32], r: &[f32]) -> anyhow::Result<()> {
        self.buf32.clear();
        self.buf16.clear();
        let g = self.gain;
        let frames = match self.mode {
            SoundcardMode::Mpx => {
                for &v in mpx {
                    let v = v * g;
                    if self.s32 {
                        self.buf32.extend_from_slice(&[to_i32(v), to_i32(v)]);
                    } else {
                        self.buf16.extend_from_slice(&[to_i16(v), to_i16(v)]);
                    }
                }
                mpx.len()
            }
            SoundcardMode::Audio => {
                for i in 0..l.len() {
                    if self.s32 {
                        self.buf32.extend_from_slice(&[to_i32(l[i] * g), to_i32(r[i] * g)]);
                    } else {
                        self.buf16.extend_from_slice(&[to_i16(l[i] * g), to_i16(r[i] * g)]);
                    }
                }
                l.len()
            }
        };
        self.write_frames(frames)
    }
}

/// A sound card that is not the master: own thread, fed from a ring buffer.
pub fn spawn_secondary(
    device: String,
    mode: SoundcardMode,
    mut consumer: rtrb::Consumer<f32>,
    st: SharedStatus,
    stop: Arc<AtomicBool>,
) {
    let _ = std::thread::Builder::new().name("pilot-alsa2".into()).spawn(move || {
        let (_, period) = rate_for(mode);
        let block = period * if mode == SoundcardMode::Mpx { 1 } else { 2 };
        let zeros = vec![0.0f32; period];
        while !stop.load(Ordering::SeqCst) {
            let mut sink = match AlsaMaster::open(&device, mode, 1.0) {
                Ok(s) => {
                    let mut g = st.lock();
                    g.running = true;
                    g.error.clear();
                    g.sample_rate = s.rate;
                    g.format = s.format.clone();
                    s
                }
                Err(e) => {
                    let mut g = st.lock();
                    g.running = false;
                    g.error = e.to_string();
                    drop(g);
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
            };
            // Pre-fill half a second so clock differences have room.
            while consumer.slots() < block * 100 && !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let mut buf = vec![0.0f32; block];
            while !stop.load(Ordering::SeqCst) {
                let res = if consumer.slots() >= block {
                    if let Ok(chunk) = consumer.read_chunk(block) {
                        let (a, b) = chunk.as_slices();
                        buf[..a.len()].copy_from_slice(a);
                        buf[a.len()..a.len() + b.len()].copy_from_slice(b);
                        chunk.commit_all();
                    }
                    match mode {
                        SoundcardMode::Mpx => sink.write(&buf, &[], &[]),
                        SoundcardMode::Audio => {
                            let l: Vec<f32> = buf.iter().step_by(2).copied().collect();
                            let r: Vec<f32> = buf.iter().skip(1).step_by(2).copied().collect();
                            sink.write(&[], &l, &r)
                        }
                    }
                } else {
                    st.lock().underruns += 1;
                    match mode {
                        SoundcardMode::Mpx => sink.write(&zeros, &[], &[]),
                        SoundcardMode::Audio => sink.write(&[], &zeros, &zeros),
                    }
                };
                if let Err(e) = res {
                    st.lock().error = e.to_string();
                    break;
                }
            }
            st.lock().running = false;
        }
    });
}
