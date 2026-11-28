//! Source pipelines: HTTP stream → ICY strip → decoder (ffmpeg or Symphonia) → f32 chunks.

use super::icy::{IcyReader, StreamMeta};
use super::{Chunk, Pipeline};
use crossbeam_channel::{bounded, Sender};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const UA: &str = concat!("PILOT/", env!("CARGO_PKG_VERSION"));

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .user_agent(UA)
        .build()
}

pub fn probe_url(url: &str) -> bool {
    if url.trim().is_empty() {
        return false;
    }
    agent().get(url).call().map(|r| r.status() == 200).unwrap_or(false)
}

fn codec_from_content_type(ct: &str) -> &'static str {
    let ct = ct.to_ascii_lowercase();
    if ct.contains("mpeg") || ct.contains("mp3") {
        "mp3"
    } else if ct.contains("aac") || ct.contains("mp4") {
        "aac"
    } else if ct.contains("ogg") {
        "ogg"
    } else {
        "unknown"
    }
}

pub fn open_stream(url: &str, use_ffmpeg: bool) -> anyhow::Result<Pipeline> {
    let resp = agent().get(url).set("Icy-MetaData", "1").call()?;
    let metaint: usize = resp.header("icy-metaint").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    let station = resp.header("icy-name").unwrap_or("").to_string();
    let ct = resp.header("content-type").unwrap_or("").to_string();
    let codec = codec_from_content_type(&ct).to_string();
    let (tx, rx) = bounded::<Chunk>(256);
    let stop = Arc::new(AtomicBool::new(false));
    let _ = tx.send(Chunk::Meta(StreamMeta { station: station.clone(), title: String::new() }));
    let meta_tx = tx.clone();
    let reader = IcyReader::new(resp.into_reader(), metaint, move |title| {
        let _ = meta_tx.try_send(Chunk::Meta(StreamMeta { station: station.clone(), title }));
    });
    if use_ffmpeg {
        let child = spawn_ffmpeg(Box::new(reader), &["-i", "pipe:0"], codec, tx, stop.clone())?;
        Ok(Pipeline { rx, stop, child: Some(child) })
    } else {
        spawn_native(Box::new(reader), codec, tx, stop.clone())?;
        Ok(Pipeline { rx, stop, child: None })
    }
}

pub fn open_file(path: &str, use_ffmpeg: bool) -> anyhow::Result<Pipeline> {
    if !std::path::Path::new(path).exists() {
        anyhow::bail!("fallback file not found: {path}");
    }
    let (tx, rx) = bounded::<Chunk>(256);
    let stop = Arc::new(AtomicBool::new(false));
    let codec = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("file")
        .to_lowercase();
    if use_ffmpeg {
        let child = spawn_ffmpeg_file(path, codec, tx, stop.clone())?;
        return Ok(Pipeline { rx, stop, child: Some(child) });
    }
    let p = path.to_string();
    let st = stop.clone();
    std::thread::Builder::new().name("pilot-file".into()).spawn(move || {
        while !st.load(Ordering::SeqCst) {
            match std::fs::File::open(&p) {
                Ok(f) => {
                    if let Err(e) = decode_native(Box::new(f), &codec, &tx, &st) {
                        tracing::warn!(error = %e, "fallback file decode");
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
                Err(_) => break,
            }
        }
    })?;
    Ok(Pipeline { rx, stop, child: None })
}

fn ffmpeg_cmd(input_args: &[&str]) -> Command {
    let mut c = Command::new("ffmpeg");
    c.args(["-hide_banner", "-loglevel", "error"]);
    if !input_args.contains(&"pipe:0") {
        c.arg("-nostdin");
    }
    c.args(input_args);
    c.args(["-vn", "-f", "f32le", "-ac", "2", "-ar", "48000", "pipe:1"]);
    c
}

fn spawn_ffmpeg(
    mut reader: Box<dyn Read + Send>,
    input_args: &[&str],
    codec: String,
    tx: Sender<Chunk>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<std::process::Child> {
    let mut child = ffmpeg_cmd(input_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let st = stop.clone();
    std::thread::Builder::new().name("pilot-net".into()).spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        while !st.load(Ordering::SeqCst) {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdin.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    })?;
    let _ = tx.send(Chunk::Info { codec, rate: 48_000, channels: 2, decoder: "ffmpeg".into() });
    spawn_pcm_reader(stdout, tx, stop)?;
    Ok(child)
}

fn spawn_ffmpeg_file(path: &str, codec: String, tx: Sender<Chunk>, stop: Arc<AtomicBool>) -> anyhow::Result<std::process::Child> {
    let mut child = ffmpeg_cmd(&["-re", "-stream_loop", "-1", "-i", path])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().unwrap();
    let _ = tx.send(Chunk::Info { codec, rate: 48_000, channels: 2, decoder: "ffmpeg".into() });
    spawn_pcm_reader(stdout, tx, stop)?;
    Ok(child)
}

fn spawn_pcm_reader(mut stdout: std::process::ChildStdout, tx: Sender<Chunk>, stop: Arc<AtomicBool>) -> anyhow::Result<()> {
    std::thread::Builder::new().name("pilot-decode".into()).spawn(move || {
        let mut buf = vec![0u8; 4800 * 8];
        let mut carry: Vec<u8> = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    carry.extend_from_slice(&buf[..n]);
                    let whole = carry.len() / 8 * 8;
                    let data: Vec<f32> = carry[..whole]
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .collect();
                    carry.drain(..whole);
                    if tx.send(Chunk::Audio { rate: 48_000, data }).is_err() {
                        break;
                    }
                }
            }
        }
    })?;
    Ok(())
}

#[cfg(feature = "native-decoder")]
fn spawn_native(reader: Box<dyn Read + Send + Sync>, codec: String, tx: Sender<Chunk>, stop: Arc<AtomicBool>) -> anyhow::Result<()> {
    std::thread::Builder::new().name("pilot-decode".into()).spawn(move || {
        if let Err(e) = decode_native(reader, &codec, &tx, &stop) {
            tracing::warn!(error = %e, "native decoder stopped");
        }
    })?;
    Ok(())
}

#[cfg(not(feature = "native-decoder"))]
fn spawn_native(_: Box<dyn Read + Send + Sync>, _: String, _: Sender<Chunk>, _: Arc<AtomicBool>) -> anyhow::Result<()> {
    anyhow::bail!("built without the native decoder; install ffmpeg")
}

#[cfg(feature = "native-decoder")]
fn decode_native(reader: Box<dyn Read + Send + Sync>, codec: &str, tx: &Sender<Chunk>, stop: &AtomicBool) -> anyhow::Result<()> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let mss = MediaSourceStream::new(Box::new(ReadOnlySource::new(reader)), Default::default());
    let mut hint = Hint::new();
    match codec {
        "mp3" => {
            hint.with_extension("mp3");
        }
        "aac" => {
            hint.with_extension("aac");
        }
        "m4a" | "mp4" => {
            hint.with_extension("m4a");
        }
        _ => {}
    }
    let probed = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())?;
    let mut format = probed.format;
    let track = format.default_track().ok_or_else(|| anyhow::anyhow!("no audio track"))?.clone();
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let channels = track.codec_params.channels.map(|c| c.count() as u16).unwrap_or(2);
    let _ = tx.send(Chunk::Info { codec: codec.to_string(), rate, channels, decoder: "native".into() });
    let mut sbuf: Option<SampleBuffer<f32>> = None;
    while !stop.load(Ordering::SeqCst) {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count();
        let buf = sbuf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64 * 2, spec));
        if buf.capacity() < decoded.capacity() * ch {
            *buf = SampleBuffer::new(decoded.capacity() as u64 * 2, spec);
        }
        buf.copy_interleaved_ref(decoded);
        let s = buf.samples();
        let data: Vec<f32> = match ch {
            1 => s.iter().flat_map(|&v| [v, v]).collect(),
            2 => s.to_vec(),
            _ => s.chunks_exact(ch).flat_map(|f| [f[0], f[1]]).collect(),
        };
        if tx.send(Chunk::Audio { rate: spec.rate, data }).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(not(feature = "native-decoder"))]
fn decode_native(_: Box<dyn Read + Send + Sync>, _: &str, _: &Sender<Chunk>, _: &AtomicBool) -> anyhow::Result<()> {
    anyhow::bail!("built without the native decoder; install ffmpeg")
}
