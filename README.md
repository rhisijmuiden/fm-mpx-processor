# PILOT — FM MPX / RDS audio processor

Broadcast audio processor for FM: Icecast input (MP3/AAC), full processing chain,
stereo MPX with 19 kHz pilot, RDS/RDS2, hard MPX limits (75 kHz deviation, BS.412 MPX power)
and output to MicroMPX, sound card, SDR or stream. Configured through a web interface (EN/NL).

- Brand style: [`branding/`](branding/README.md)
- Design (phase 1): [`docs/fase-1-ontwerp.md`](docs/fase-1-ontwerp.md)
- Audio engine (phase 2): [`docs/fase-2-engine.md`](docs/fase-2-engine.md) · API: [`docs/api.md`](docs/api.md)

## Engine

```
cd engine
cargo test --release          # DSP, RDS, limits, storage
cargo run --release -- serve --listen 0.0.0.0:8080 --db ./pilot.db
cargo run --release -- render input.mp3 mpx.wav --seconds 30
```

Install on Debian 13 (native, systemd): `sudo deploy/install.sh --channel test`.

Branches: `main` = production, `test` = test.
