//! Audio input: Icecast/Shoutcast (MP3, AAC, HE-AAC via ffmpeg) with ICY metadata,
//! a backup stream and a local fallback file. Decoded audio is resampled to 48 kHz
//! with drift compensation and written to a lock-free ring buffer.

mod decode;
mod icy;

use crate::config::{Decoder, InputConfig};
use crate::dsp::resampler::Resampler;
use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender};
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use icy::StreamMeta;

pub const RING_SECONDS: usize = 12;

#[derive(Debug, Clone, Default, Serialize)]
pub struct InputStatus {
    /// "primary", "backup", "file" or "none".
    pub source: String,
    pub url: String,
    pub connected: bool,
    pub codec: String,
    pub decoder: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub station: String,
    pub now_playing: String,
    pub buffer_ms: u32,
    pub target_ms: u32,
    /// Current drift correction in ppm.
    pub drift_ppm: f32,
    pub reconnects: u64,
    pub last_error: String,
}

/// Decoded audio from a pipeline: interleaved stereo f32 at `rate`.
pub(crate) enum Chunk {
    Audio { rate: u32, data: Vec<f32> },
    Info { codec: String, rate: u32, channels: u16, decoder: String },
    Meta(StreamMeta),
}

pub(crate) struct Pipeline {
    pub rx: Receiver<Chunk>,
    pub stop: Arc<AtomicBool>,
    pub child: Option<std::process::Child>,
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

pub struct InputHandle {
    pub status: Arc<Mutex<InputStatus>>,
    cfg_tx: Sender<InputConfig>,
}

impl InputHandle {
    pub fn set_config(&self, cfg: &InputConfig) {
        let _ = self.cfg_tx.try_send(cfg.clone());
    }
}

pub fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Start the input thread. `meta_tx` receives stream titles for dynamic RadioText.
pub fn start(
    cfg: InputConfig,
    mut producer: rtrb::Producer<f32>,
    meta_tx: Sender<StreamMeta>,
) -> InputHandle {
    let status = Arc::new(Mutex::new(InputStatus { source: "none".into(), ..Default::default() }));
    let (cfg_tx, cfg_rx) = bounded::<InputConfig>(4);
    let st = status.clone();
    std::thread::Builder::new()
        .name("pilot-input".into())
        .spawn(move || {
            let mut cfg = cfg;
            let has_ffmpeg = ffmpeg_available();
            let mut idx = 0usize;
            let mut primary_retry = Instant::now();
            loop {
                while let Ok(c) = cfg_rx.try_recv() {
                    if c.url != cfg.url || c.backup_url != cfg.backup_url || c.fallback_file != cfg.fallback_file || c.decoder != cfg.decoder {
                        idx = 0;
                    }
                    cfg = c;
                }
                let sources: Vec<(&str, String)> = [
                    ("primary", cfg.url.clone()),
                    ("backup", cfg.backup_url.clone()),
                    ("file", cfg.fallback_file.clone()),
                ]
                .into_iter()
                .filter(|(_, u)| !u.trim().is_empty())
                .collect();
                if sources.is_empty() {
                    {
                        let mut s = st.lock();
                        s.source = "none".into();
                        s.connected = false;
                    }
                    if let Ok(c) = cfg_rx.recv_timeout(Duration::from_millis(500)) {
                        cfg = c;
                        idx = 0;
                    }
                    continue;
                }
                if idx >= sources.len() {
                    idx = 0;
                }
                let (kind, url) = sources[idx].clone();
                let use_ffmpeg = match cfg.decoder {
                    Decoder::Ffmpeg => true,
                    Decoder::Native => false,
                    Decoder::Auto => has_ffmpeg,
                };
                {
                    let mut s = st.lock();
                    s.source = kind.into();
                    s.url = redact(&url);
                    s.connected = false;
                    s.target_ms = cfg.buffer_ms;
                }
                tracing::info!(source = kind, url = %redact(&url), ffmpeg = use_ffmpeg, "opening input");
                let pipe = if kind == "file" {
                    decode::open_file(&url, use_ffmpeg)
                } else {
                    decode::open_stream(&url, use_ffmpeg)
                };
                let result = match pipe {
                    Ok(p) => run_pipeline(p, kind, &cfg, &mut producer, &meta_tx, &st, &cfg_rx, kind != "primary", &mut primary_retry),
                    Err(e) => Err(e),
                };
                match result {
                    Ok(Next::Reconfigure(c)) => {
                        cfg = c;
                        idx = 0;
                    }
                    Ok(Next::RetryPrimary) => idx = 0,
                    Err(e) => {
                        tracing::warn!(source = kind, error = %e, "input failed");
                        let mut s = st.lock();
                        s.connected = false;
                        s.last_error = e.to_string();
                        s.reconnects += 1;
                        drop(s);
                        idx += 1;
                        if idx >= sources.len() {
                            idx = 0;
                            std::thread::sleep(Duration::from_secs(2));
                        }
                        if idx > 0 {
                            primary_retry = Instant::now() + Duration::from_secs(30);
                        }
                    }
                }
            }
        })
        .expect("spawn input thread");
    InputHandle { status, cfg_tx }
}

enum Next {
    Reconfigure(InputConfig),
    RetryPrimary,
}

#[allow(clippy::too_many_arguments)]
fn run_pipeline(
    pipe: Pipeline,
    kind: &str,
    cfg: &InputConfig,
    producer: &mut rtrb::Producer<f32>,
    meta_tx: &Sender<StreamMeta>,
    st: &Arc<Mutex<InputStatus>>,
    cfg_rx: &Receiver<InputConfig>,
    on_fallback: bool,
    primary_retry: &mut Instant,
) -> anyhow::Result<Next> {
    let timeout = Duration::from_secs_f32(cfg.failover_s);
    let capacity = producer.buffer().capacity();
    let target = (cfg.buffer_ms as usize * 48 * 2).min(capacity * 3 / 4);
    let is_file = kind == "file";
    let mut rs: Option<(u32, Resampler)> = None;
    let mut rel = 1.0f64;
    let (mut l, mut r, mut ol, mut or) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut last_audio = Instant::now();
    let mut got_audio = false;
    loop {
        if let Ok(c) = cfg_rx.try_recv() {
            return Ok(Next::Reconfigure(c));
        }
        if on_fallback && got_audio && Instant::now() > *primary_retry && kind != "primary" {
            // Periodically try to get back to the primary stream.
            *primary_retry = Instant::now() + Duration::from_secs(60);
            if decode::probe_url(&cfg.url) {
                tracing::info!("primary input is back, switching");
                return Ok(Next::RetryPrimary);
            }
        }
        let chunk = match pipe.rx.recv_timeout(Duration::from_millis(250)) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => {
                if last_audio.elapsed() > timeout {
                    anyhow::bail!("no audio for {:.0} s", timeout.as_secs_f32());
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("stream ended"),
        };
        match chunk {
            Chunk::Info { codec, rate, channels, decoder } => {
                let mut s = st.lock();
                s.codec = codec;
                s.sample_rate = rate;
                s.channels = channels;
                s.decoder = decoder;
            }
            Chunk::Meta(m) => {
                {
                    let mut s = st.lock();
                    if !m.station.is_empty() {
                        s.station = m.station.clone();
                    }
                    if !m.title.is_empty() {
                        s.now_playing = m.title.clone();
                    }
                }
                let _ = meta_tx.try_send(m);
            }
            Chunk::Audio { rate, data } => {
                if data.is_empty() {
                    continue;
                }
                let peak = data.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
                if peak > 1e-5 || is_file {
                    last_audio = Instant::now();
                }
                if !got_audio {
                    got_audio = true;
                    let mut s = st.lock();
                    s.connected = true;
                    s.last_error.clear();
                }
                if rs.as_ref().map(|(r0, _)| *r0 != rate).unwrap_or(true) {
                    rs = Some((rate, Resampler::new(rate as f64, 48_000.0)));
                }
                let resampler = &mut rs.as_mut().unwrap().1;
                // Drift control: keep the ring at the target fill.
                let fill = capacity - producer.slots();
                if !is_file {
                    let err = (fill as f64 - target as f64) / target as f64;
                    let want = 1.0 + (err * 0.002).clamp(-0.0005, 0.0005);
                    rel += (want - rel) * 0.01;
                    resampler.set_ratio_relative(rel);
                }
                l.clear();
                r.clear();
                for f in data.chunks_exact(2) {
                    l.push(f[0]);
                    r.push(f[1]);
                }
                ol.clear();
                or.clear();
                resampler.process(&l, &r, &mut ol, &mut or);
                // Files are paced by the buffer; live streams only block when the ring is full.
                let ceiling = if is_file { target } else { capacity };
                let mut i = 0;
                while i < ol.len() {
                    let fill = capacity - producer.slots();
                    if fill + 2 > ceiling {
                        std::thread::sleep(Duration::from_millis(5));
                        if let Ok(c) = cfg_rx.try_recv() {
                            return Ok(Next::Reconfigure(c));
                        }
                        continue;
                    }
                    let room = (ceiling - fill) / 2;
                    let n = room.min(ol.len() - i);
                    if let Ok(mut chunk) = producer.write_chunk_uninit(n * 2) {
                        let (a, b) = chunk.as_mut_slices();
                        let mut k = 0;
                        for slot in a.iter_mut().chain(b.iter_mut()) {
                            let j = i + k / 2;
                            slot.write(if k % 2 == 0 { ol[j] } else { or[j] });
                            k += 1;
                        }
                        unsafe { chunk.commit_all() };
                    }
                    i += n;
                }
                let fill = capacity - producer.slots();
                let mut s = st.lock();
                s.buffer_ms = (fill / 96) as u32;
                s.drift_ppm = ((rel - 1.0) * 1e6) as f32;
            }
        }
    }
}

/// Hide passwords in URLs for status output and logs.
pub fn redact(url: &str) -> String {
    if let Some(scheme_end) = url.find("://") {
        let rest = &url[scheme_end + 3..];
        if let Some(at) = rest.find('@') {
            let host = &rest[at + 1..];
            let user = rest[..at].split(':').next().unwrap_or("");
            return format!("{}://{}:***@{}", &url[..scheme_end], user, host);
        }
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn redacts_passwords() {
        assert_eq!(super::redact("icecast://source:geheim@host:8000/live"), "icecast://source:***@host:8000/live");
        assert_eq!(super::redact("http://host/stream"), "http://host/stream");
    }
}
