# PILOT engine API (v0.2)

Base URL: `http://<host>:8080`. All bodies are JSON. User-facing strings (stage names,
errors, notices) follow `Accept-Language` or `?lang=en|nl`; English is the default.

When `PILOT_API_TOKEN` is set in `/etc/pilot/pilot.env`, every non-GET call needs
`Authorization: Bearer <token>`.

## Overview

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/health` | Liveness check |
| GET | `/api/info` | Name, version, git commit, channel (`production`/`test`), languages |
| GET | `/api/status` | Engine (clock, buffering, uptime), input, outputs |
| GET | `/api/config` | Full active configuration |
| PUT | `/api/config` | Replace the configuration |
| PATCH | `/api/config` | JSON merge patch (RFC 7386), e.g. `{"rds":{"ps":"RADIO 1"}}` |
| GET | `/api/config/defaults` | Factory configuration |
| GET | `/api/chain` | Signal chain overview: every stage with name, on/off and live value |
| GET | `/api/meters` | Latest meter snapshot |
| WS | `/api/ws` | Meter stream (~20/s); every second also engine and input status |
| POST | `/api/meters/reset-loudness` | Restart the integrated LUFS measurement |
| GET | `/api/spectrum?bins=512` | MPX spectrum 0–96 kHz in dB (0 dB = 100 % modulation sine) |
| POST | `/api/rds/radiotext` | `{"text":"..."}` set RadioText now (overrides until the next title) |
| GET | `/api/presets` | Processing presets |
| POST | `/api/presets` | `{"name","description","processing"?}` save (current processing if omitted) |
| GET / DELETE | `/api/presets/{name}` | Read or remove a preset |
| POST | `/api/presets/{name}/apply` | Load the preset's processing section |
| GET | `/api/i18n/{en\|nl}` | String table |
| GET | `/api/log?limit=100` | Audit log (config changes, presets, RadioText) |

`PUT` and `PATCH` return `{"config": …, "adjusted": [fields], "notice"?: "..."}`. Values
outside their range are clamped and listed in `adjusted`. **Hard limits** cannot be raised:
`limits.max_deviation_khz` ≤ 75 and `limits.mpx_power_dbr` ≤ +3.

## Configuration sections

- `input`: `url`, `backup_url`, `fallback_file`, `decoder` (`auto`/`ffmpeg`/`native`),
  `buffer_ms`, `gain_db`, `failover_s`.
- `processing`: `bypass`, `agc`, `multiband` (4 crossovers, 5 bands with threshold, ratio,
  knee, attack, release, makeup, gain), `emphasis` (`50us`/`75us`/`off`), `limiter`, `clipper`.
- `stereo`: `stereo`, `pilot_pct`, `pilot_phase_deg`.
- `rds`: `enabled`, `injection_pct`, `pi`, `ps`, `rt`, `dynamic_rt`, `rt_template`
  (`{song}`, `{artist}`, `{title}`, `{station}`), `pty`, `tp`, `ta`, `ms`, `af_mhz`, `ct`,
  `rds2` (`enabled`, `streams`, `injection_pct`).
- `limits`: `max_deviation_khz`, `mpx_power_enabled`, `mpx_power_dbr`.
- `outputs`: `micrompx` (ALSA loopback device, level, encoder status URL), `soundcard`
  (device, `mpx`/`audio` mode, level), `sdr` (upsample, I/Q format, command), `stream`
  (Icecast URL, `mp3`/`aac`, bitrate).

See `engine/src/config.rs` for every field, default and range.

## Meter snapshot

```json
{
  "seq": 1234,
  "input_peak_db": [-6.1, -6.4], "input_loudness": {"momentary": -18.2, "short_term": -18.9, "integrated": -19.4},
  "output_loudness": {...}, "output_peak_db": [...],
  "agc_gain_db": 2.0, "agc_gated": false, "band_gr_db": [1.2, 2.0, 2.4, 1.1, 0.4],
  "limiter_gr_db": 0.8, "clipper_activity": 0.02,
  "deviation_khz": 71.8, "deviation_hold_khz": 74.6, "max_deviation_khz": 75,
  "mpx_power_dbr": 2.7, "mpx_power_limit_dbr": 3, "bs412_gain_db": -0.4,
  "composite_limiter_db": -0.1, "hard_clips": 0,
  "pilot_khz": 6.75, "rds_khz": 3.0, "rds_ps": "PILOT   ", "rds_rt": "Artist - Title",
  "dsp_load": 0.12
}
```
