//! Wires input, DSP chain and outputs together in one realtime thread.

use crate::config::Config;
use crate::dsp::chain::{Chain, BLOCK, MPX_BLOCK};
use crate::dsp::meters::Meters;
use crate::input::{self, InputHandle, InputStatus, StreamMeta, RING_SECONDS};
use crate::output::{OutputStatus, Outputs, SharedStatus};
use crossbeam_channel::{bounded, Receiver, Sender};
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::Arc;
use std::time::Instant;

pub const SPECTRUM_SAMPLES: usize = 16_384;

pub enum Command {
    Config(Box<Config>),
    RadioText(String),
    ResetLoudness,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EngineStatus {
    pub running: bool,
    /// True when an audio device paces the engine; false = system timer.
    pub hardware_clock: bool,
    pub input_underruns: u64,
    pub buffering: bool,
    pub uptime_s: u64,
    pub realtime_priority: bool,
}

#[derive(Clone)]
pub struct EngineHandle {
    cmd: Sender<Command>,
    pub meters: Arc<Mutex<Meters>>,
    pub status: Arc<Mutex<EngineStatus>>,
    pub input: Arc<Mutex<InputStatus>>,
    pub outputs: Arc<Mutex<Vec<SharedStatus>>>,
    pub spectrum: Arc<Mutex<Vec<f32>>>,
    input_handle: Arc<InputHandle>,
}

impl EngineHandle {
    pub fn apply(&self, cfg: &Config) {
        self.input_handle.set_config(&cfg.input);
        let _ = self.cmd.send(Command::Config(Box::new(cfg.clone())));
    }

    pub fn set_radiotext(&self, text: &str) {
        let _ = self.cmd.send(Command::RadioText(text.to_string()));
    }

    pub fn reset_loudness(&self) {
        let _ = self.cmd.send(Command::ResetLoudness);
    }

    pub fn output_statuses(&self) -> Vec<OutputStatus> {
        self.outputs.lock().iter().map(|s| s.lock().clone()).collect()
    }
}

fn try_realtime() -> bool {
    unsafe {
        let param = libc::sched_param { sched_priority: 70 };
        libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) == 0
    }
}

pub fn start(cfg: Config) -> EngineHandle {
    let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(48_000 * 2 * RING_SECONDS);
    let (meta_tx, meta_rx) = bounded::<StreamMeta>(16);
    let input_handle = Arc::new(input::start(cfg.input.clone(), producer, meta_tx));
    let (cmd_tx, cmd_rx) = bounded::<Command>(32);
    let meters = Arc::new(Mutex::new(Meters::default()));
    let status = Arc::new(Mutex::new(EngineStatus::default()));
    let outputs_status = Arc::new(Mutex::new(Vec::new()));
    let spectrum = Arc::new(Mutex::new(vec![0.0f32; SPECTRUM_SAMPLES]));

    let handle = EngineHandle {
        cmd: cmd_tx,
        meters: meters.clone(),
        status: status.clone(),
        input: input_handle.status.clone(),
        outputs: outputs_status.clone(),
        spectrum: spectrum.clone(),
        input_handle,
    };

    std::thread::Builder::new()
        .name("pilot-dsp".into())
        .spawn(move || {
            let rt = try_realtime();
            if !rt {
                tracing::warn!("no realtime priority (set LimitRTPRIO in the service)");
            }
            run(cfg, &mut consumer, cmd_rx, meta_rx, meters, status, outputs_status, spectrum, rt);
        })
        .expect("spawn dsp thread");
    handle
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut cfg: Config,
    consumer: &mut rtrb::Consumer<f32>,
    cmd_rx: Receiver<Command>,
    meta_rx: Receiver<StreamMeta>,
    meters: Arc<Mutex<Meters>>,
    status: Arc<Mutex<EngineStatus>>,
    outputs_status: Arc<Mutex<Vec<SharedStatus>>>,
    spectrum: Arc<Mutex<Vec<f32>>>,
    rt: bool,
) {
    let mut chain = Chain::new(&cfg);
    let mut outputs = Outputs::new(&cfg.outputs);
    *outputs_status.lock() = outputs.statuses.clone();
    let started = Instant::now();
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let (mut tl, mut tr) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let mut mpx = vec![0.0f32; MPX_BLOCK];
    let mut spec_buf = vec![0.0f32; SPECTRUM_SAMPLES];
    let mut spec_pos = 0usize;
    let mut primed = false;
    let mut underruns = 0u64;
    let mut last_meta = StreamMeta::default();
    let mut busy = 0.0f64;
    let mut blocks = 0u64;
    {
        let mut s = status.lock();
        s.running = true;
        s.realtime_priority = rt;
    }
    loop {
        // ---- control (non-blocking)
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Config(c) => {
                    let outputs_changed = c.outputs != cfg.outputs;
                    chain.set_config(&c);
                    cfg = *c;
                    if cfg.rds.dynamic_rt && !last_meta.title.is_empty() {
                        chain.set_radiotext(&last_meta.render(&cfg.rds.rt_template));
                    } else if !cfg.rds.dynamic_rt {
                        chain.set_radiotext(&cfg.rds.rt);
                    }
                    if outputs_changed {
                        drop(outputs);
                        outputs = Outputs::new(&cfg.outputs);
                        *outputs_status.lock() = outputs.statuses.clone();
                    }
                }
                Command::RadioText(t) => chain.set_radiotext(&t),
                Command::ResetLoudness => chain.reset_loudness(),
            }
        }
        while let Ok(m) = meta_rx.try_recv() {
            if !m.title.is_empty() && cfg.rds.dynamic_rt {
                let text = m.render(&cfg.rds.rt_template);
                if !text.is_empty() {
                    chain.set_radiotext(&text);
                }
            }
            if !m.title.is_empty() {
                last_meta = m;
            }
        }

        // ---- input
        let target = cfg.input.buffer_ms as usize * 96;
        let avail = consumer.slots();
        if !primed && avail >= target.min(consumer.buffer().capacity() / 2) {
            primed = true;
        }
        if primed && avail >= BLOCK * 2 {
            if let Ok(chunk) = consumer.read_chunk(BLOCK * 2) {
                let (a, b) = chunk.as_slices();
                for (k, v) in a.iter().chain(b.iter()).enumerate() {
                    if k % 2 == 0 { l[k / 2] = *v } else { r[k / 2] = *v }
                }
                chunk.commit_all();
            }
        } else {
            if primed {
                primed = false;
                underruns += 1;
            }
            l.fill(0.0);
            r.fill(0.0);
        }

        // ---- DSP
        let t0 = Instant::now();
        chain.process(&l, &r, &mut mpx, &mut tl, &mut tr);
        busy += t0.elapsed().as_secs_f64();
        blocks += 1;

        // ---- spectrum snapshot (MPX, 192 kHz)
        let n = mpx.len().min(SPECTRUM_SAMPLES - spec_pos);
        spec_buf[spec_pos..spec_pos + n].copy_from_slice(&mpx[..n]);
        spec_pos += n;
        if spec_pos >= SPECTRUM_SAMPLES {
            spec_pos = 0;
            if let Some(mut s) = spectrum.try_lock() {
                s.copy_from_slice(&spec_buf);
            }
        }

        // ---- meters
        if blocks % 7 == 0 {
            if let Some(mut m) = meters.try_lock() {
                *m = chain.meters.clone();
                m.dsp_load = (busy / (7.0 * 0.005)) as f32;
            }
            busy = 0.0;
            if let Some(mut s) = status.try_lock() {
                s.hardware_clock = outputs.has_master();
                s.input_underruns = underruns;
                s.buffering = !primed;
                s.uptime_s = started.elapsed().as_secs();
            }
        }

        // ---- outputs (blocks on the master clock)
        outputs.write(&mpx, &tl, &tr);
    }
}
