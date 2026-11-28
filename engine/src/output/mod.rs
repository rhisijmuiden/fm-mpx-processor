//! Outputs. One output is the *master clock* that paces the realtime thread
//! (the MicroMPX loopback or the sound card); without one a timer paces it.
//! All other outputs run in their own threads behind lock-free ring buffers,
//! so a slow stream server or SDR can never stall the MPX to the transmitter.

#[cfg(feature = "alsa-out")]
pub mod alsa_out;
pub mod sdr;
pub mod stream;

use crate::config::{OutputsConfig, SoundcardMode};
use crate::dsp::chain::{BLOCK, MPX_BLOCK};
use crate::dsp::dynamics::db_to_lin;
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, Serialize)]
pub struct OutputStatus {
    pub name: String,
    pub enabled: bool,
    pub running: bool,
    /// "master" (paces the engine) or "secondary".
    pub role: String,
    pub target: String,
    pub sample_rate: u32,
    pub format: String,
    pub underruns: u64,
    pub overruns: u64,
    pub error: String,
    /// Extra information, e.g. the MicroMPX encoder state.
    pub detail: String,
}

pub type SharedStatus = Arc<Mutex<OutputStatus>>;

fn status(name: &str, enabled: bool, target: &str) -> SharedStatus {
    Arc::new(Mutex::new(OutputStatus {
        name: name.into(),
        enabled,
        target: target.into(),
        role: "secondary".into(),
        ..Default::default()
    }))
}

/// What the master clock receives per block.
pub enum Feed<'a> {
    Mpx(&'a [f32]),
    Audio(&'a [f32], &'a [f32]),
}

/// A blocking sink that paces the engine.
pub trait MasterSink: Send {
    fn write(&mut self, mpx: &[f32], l: &[f32], r: &[f32]) -> anyhow::Result<()>;
}

struct TimerClock {
    next: Instant,
}

impl TimerClock {
    fn wait(&mut self) {
        self.next += Duration::from_micros(5_000);
        let now = Instant::now();
        if self.next > now {
            std::thread::sleep(self.next - now);
        } else if now - self.next > Duration::from_millis(200) {
            self.next = now; // we fell far behind (suspend, debugger): resync
        }
    }
}

/// A secondary output fed through a ring buffer.
struct Secondary {
    producer: rtrb::Producer<f32>,
    kind: SecondaryKind,
    status: SharedStatus,
    gain: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum SecondaryKind {
    Mpx,
    Audio,
}

pub struct Outputs {
    master: Option<Box<dyn MasterSink>>,
    master_status: Option<SharedStatus>,
    master_retry: Instant,
    master_spec: Option<MasterSpec>,
    timer: TimerClock,
    secondaries: Vec<Secondary>,
    stop: Arc<AtomicBool>,
    pub statuses: Vec<SharedStatus>,
    scratch: Vec<f32>,
}

#[derive(Clone)]
struct MasterSpec {
    device: String,
    mode: SoundcardMode,
    gain: f32,
}

impl Drop for Outputs {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Outputs {
    pub fn new(cfg: &OutputsConfig) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mut statuses = Vec::new();
        let mut secondaries = Vec::new();

        let mm = &cfg.micrompx;
        let sc = &cfg.soundcard;
        let mm_status = status("micrompx", mm.enabled, &mm.device);
        let sc_status = status("soundcard", sc.enabled, &sc.device);
        statuses.push(mm_status.clone());
        statuses.push(sc_status.clone());

        // Master: MicroMPX loopback first, then the sound card.
        let mut master_spec = None;
        let mut master_status = None;
        if mm.enabled {
            master_spec = Some(MasterSpec { device: mm.device.clone(), mode: SoundcardMode::Mpx, gain: db_to_lin(mm.level_db) });
            master_status = Some(mm_status.clone());
            if sc.enabled {
                secondaries.push(Self::alsa_secondary(sc, sc_status.clone(), stop.clone()));
            }
        } else if sc.enabled {
            master_spec = Some(MasterSpec { device: sc.device.clone(), mode: sc.mode, gain: db_to_lin(sc.level_db) });
            master_status = Some(sc_status.clone());
        }
        if mm.enabled && !mm.encoder_status_url.trim().is_empty() {
            spawn_encoder_poll(mm.encoder_status_url.clone(), mm_status.clone(), stop.clone());
        }

        let sdr_status = status("sdr", cfg.sdr.enabled, &cfg.sdr.command);
        statuses.push(sdr_status.clone());
        if cfg.sdr.enabled {
            let (p, c) = rtrb::RingBuffer::<f32>::new(MPX_BLOCK * 200);
            sdr::spawn(cfg.sdr.clone(), c, sdr_status.clone(), stop.clone());
            secondaries.push(Secondary { producer: p, kind: SecondaryKind::Mpx, status: sdr_status, gain: 1.0 });
        }

        let st_status = status("stream", cfg.stream.enabled, &crate::input::redact(&cfg.stream.url));
        statuses.push(st_status.clone());
        if cfg.stream.enabled && !cfg.stream.url.is_empty() {
            let (p, c) = rtrb::RingBuffer::<f32>::new(BLOCK * 2 * 400);
            stream::spawn(cfg.stream.clone(), c, st_status.clone(), stop.clone());
            secondaries.push(Secondary { producer: p, kind: SecondaryKind::Audio, status: st_status, gain: 1.0 });
        }

        let mut o = Self {
            master: None,
            master_status,
            master_retry: Instant::now(),
            master_spec,
            timer: TimerClock { next: Instant::now() },
            secondaries,
            stop,
            statuses,
            scratch: Vec::with_capacity(MPX_BLOCK * 2),
        };
        o.open_master();
        o
    }

    #[allow(unused_variables)]
    fn alsa_secondary(sc: &crate::config::SoundcardOutput, st: SharedStatus, stop: Arc<AtomicBool>) -> Secondary {
        let kind = if sc.mode == SoundcardMode::Mpx { SecondaryKind::Mpx } else { SecondaryKind::Audio };
        let (p, c) = rtrb::RingBuffer::<f32>::new(MPX_BLOCK * 2 * 40);
        #[cfg(feature = "alsa-out")]
        alsa_out::spawn_secondary(sc.device.clone(), sc.mode, c, st.clone(), stop);
        #[cfg(not(feature = "alsa-out"))]
        {
            st.lock().error = "built without ALSA support".into();
            drop(c);
        }
        Secondary { producer: p, kind, status: st, gain: db_to_lin(sc.level_db) }
    }

    fn open_master(&mut self) {
        let (Some(spec), Some(st)) = (self.master_spec.clone(), self.master_status.clone()) else {
            return;
        };
        self.master_retry = Instant::now() + Duration::from_secs(5);
        #[cfg(feature = "alsa-out")]
        {
            match alsa_out::AlsaMaster::open(&spec.device, spec.mode, spec.gain) {
                Ok(m) => {
                    let mut s = st.lock();
                    s.role = "master".into();
                    s.running = true;
                    s.error.clear();
                    s.sample_rate = m.rate;
                    s.format = m.format.clone();
                    drop(s);
                    self.master = Some(Box::new(m));
                }
                Err(e) => {
                    let mut s = st.lock();
                    s.role = "master".into();
                    s.running = false;
                    s.error = e.to_string();
                    tracing::warn!(device = %spec.device, error = %e, "cannot open master output, using timer");
                }
            }
        }
        #[cfg(not(feature = "alsa-out"))]
        {
            let _ = spec;
            st.lock().error = "built without ALSA support".into();
        }
    }

    /// Hand one block to all outputs. Blocks on the master clock (or the timer).
    pub fn write(&mut self, mpx: &[f32], l: &[f32], r: &[f32]) {
        for sec in self.secondaries.iter_mut() {
            let n = match sec.kind {
                SecondaryKind::Mpx => mpx.len(),
                SecondaryKind::Audio => l.len() * 2,
            };
            match sec.producer.write_chunk_uninit(n) {
                Ok(mut chunk) => {
                    let (a, b) = chunk.as_mut_slices();
                    let g = sec.gain;
                    for (k, slot) in a.iter_mut().chain(b.iter_mut()).enumerate() {
                        let v = match sec.kind {
                            SecondaryKind::Mpx => mpx[k],
                            SecondaryKind::Audio => if k % 2 == 0 { l[k / 2] } else { r[k / 2] },
                        };
                        slot.write(v * g);
                    }
                    unsafe { chunk.commit_all() };
                }
                Err(_) => sec.status.lock().overruns += 1,
            }
        }
        if let Some(m) = self.master.as_mut() {
            if let Err(e) = m.write(mpx, l, r) {
                tracing::warn!(error = %e, "master output failed, switching to timer");
                if let Some(st) = &self.master_status {
                    let mut s = st.lock();
                    s.running = false;
                    s.error = e.to_string();
                }
                self.master = None;
                self.timer.next = Instant::now();
            } else {
                return;
            }
        }
        if self.master_spec.is_some() && Instant::now() > self.master_retry {
            self.open_master();
            if self.master.is_some() {
                return;
            }
        }
        self.timer.wait();
    }

    pub fn has_master(&self) -> bool {
        self.master.is_some()
    }

    #[allow(dead_code)]
    fn scratch(&mut self) -> &mut Vec<f32> {
        &mut self.scratch
    }
}

/// Poll the MicroMPX encoder's web interface so its state shows up in PILOT.
fn spawn_encoder_poll(url: String, st: SharedStatus, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new().name("pilot-umpx-poll".into()).spawn(move || {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(3)).build();
        while !stop.load(Ordering::SeqCst) {
            let detail = match agent.get(&url).call() {
                Ok(r) => {
                    let body = r.into_string().unwrap_or_default();
                    format!("encoder reachable: {}", body.chars().take(200).collect::<String>())
                }
                Err(e) => format!("encoder unreachable: {e}"),
            };
            st.lock().detail = detail;
            for _ in 0..50 {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    });
}
