//! Offline rendering: audio file → MPX WAV (192 kHz, 32-bit float, mono) plus a report.
//! Handy to check processing and limits without any hardware.

use crate::config::Config;
use crate::dsp::chain::{Chain, BLOCK, MPX_BLOCK};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn write_wav_header(w: &mut impl Write, rate: u32, channels: u16, frames: u32) -> std::io::Result<()> {
    let data = frames * channels as u32 * 4;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&3u16.to_le_bytes())?; // IEEE float
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&(rate * channels as u32 * 4).to_le_bytes())?;
    w.write_all(&(channels * 4).to_le_bytes())?;
    w.write_all(&32u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data.to_le_bytes())
}

pub fn render(input: &Path, output: &Path, config: Option<&Path>, seconds: f32) -> anyhow::Result<()> {
    let mut cfg: Config = match config {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(p)?)?,
        None => Config::default(),
    };
    cfg.sanitize();
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(input)
        .args(["-t", &seconds.to_string(), "-vn", "-f", "f32le", "-ac", "2", "-ar", "48000", "pipe:1"])
        .stdout(Stdio::piped())
        .spawn()?;
    let mut pcm = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut pcm)?;
    child.wait()?;
    let samples: Vec<f32> = pcm.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    let frames = samples.len() / 2;
    let blocks = frames / BLOCK;
    let mut chain = Chain::new(&cfg);
    let mut out = std::io::BufWriter::new(std::fs::File::create(output)?);
    write_wav_header(&mut out, 192_000, 1, (blocks * MPX_BLOCK) as u32)?;
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let (mut tl, mut tr) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut mpx = vec![0.0f32; MPX_BLOCK];
    let mut peak = 0.0f32;
    for b in 0..blocks {
        for i in 0..BLOCK {
            l[i] = samples[(b * BLOCK + i) * 2];
            r[i] = samples[(b * BLOCK + i) * 2 + 1];
        }
        chain.process(&l, &r, &mut mpx, &mut tl, &mut tr);
        for &v in &mpx {
            peak = peak.max(v.abs());
            out.write_all(&v.to_le_bytes())?;
        }
    }
    out.flush()?;
    let m = &chain.meters;
    println!("rendered {:.1} s → {}", blocks as f32 * 0.005, output.display());
    println!("peak deviation   {:6.2} kHz (limit {:.1})", peak * 75.0, cfg.limits.max_deviation_khz);
    println!("MPX power        {:+6.2} dBr (limit {:+.1})", m.mpx_power_dbr, cfg.limits.mpx_power_dbr);
    println!("BS.412 gain      {:6.2} dB", m.bs412_gain_db);
    println!("input loudness   {:6.1} LUFS (integrated)", m.input_loudness.integrated);
    println!("output loudness  {:6.1} LUFS (integrated)", m.output_loudness.integrated);
    println!("hard clips       {}", m.hard_clips);
    Ok(())
}
