//! Stream output: processed (de-emphasised) audio encoded by ffmpeg and sent to Icecast.

use super::SharedStatus;
use crate::config::StreamOutput;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub fn ffmpeg_args(cfg: &StreamOutput) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-f", "f32le", "-ar", "48000", "-ac", "2", "-i", "pipe:0"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let br = format!("{}k", cfg.bitrate_kbps);
    if cfg.codec == "aac" {
        a.extend(["-c:a", "aac", "-b:a", &br, "-f", "adts", "-content_type", "audio/aac"].map(String::from));
    } else {
        a.extend(["-c:a", "libmp3lame", "-b:a", &br, "-f", "mp3", "-content_type", "audio/mpeg"].map(String::from));
    }
    a.extend(["-ice_name".to_string(), "PILOT".to_string()]);
    a.push(cfg.url.clone());
    a
}

pub fn spawn(cfg: StreamOutput, mut consumer: rtrb::Consumer<f32>, st: SharedStatus, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new().name("pilot-stream".into()).spawn(move || {
        {
            let mut s = st.lock();
            s.sample_rate = 48_000;
            s.format = format!("{} {} kbit/s", cfg.codec, cfg.bitrate_kbps);
        }
        while !stop.load(Ordering::SeqCst) {
            let mut child = match Command::new("ffmpeg")
                .args(ffmpeg_args(&cfg))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    st.lock().error = format!("ffmpeg: {e}");
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
            let mut bytes = Vec::with_capacity(480 * 4);
            while !stop.load(Ordering::SeqCst) {
                let n = consumer.slots().min(4800);
                if n < 480 {
                    std::thread::sleep(Duration::from_millis(5));
                    if let Ok(Some(_)) = child.try_wait() {
                        break;
                    }
                    continue;
                }
                bytes.clear();
                if let Ok(chunk) = consumer.read_chunk(n) {
                    let (a, b) = chunk.as_slices();
                    for v in a.iter().chain(b.iter()) {
                        bytes.extend_from_slice(&v.to_le_bytes());
                    }
                    chunk.commit_all();
                }
                if stdin.write_all(&bytes).is_err() {
                    break;
                }
            }
            let _ = child.kill();
            let _ = child.wait();
            {
                let mut s = st.lock();
                s.running = false;
                if !stop.load(Ordering::SeqCst) {
                    s.error = "stream connection lost, retrying".into();
                }
            }
            // Drop what piled up while disconnected.
            let pending = consumer.slots();
            if let Ok(c) = consumer.read_chunk(pending) {
                c.commit_all();
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}
