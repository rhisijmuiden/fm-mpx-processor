//! HTTP API (REST + WebSocket) on the management port. The phase 3 web interface
//! is built on top of this; see docs/api.md.

use crate::config::{merge_patch, Config};
use crate::db::Db;
use crate::engine::{EngineHandle, SPECTRUM_SAMPLES};
use crate::i18n;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub engine: EngineHandle,
    pub cfg: Arc<Mutex<Config>>,
    pub token: Option<String>,
    pub channel: String,
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn lang(h: &HeaderMap, q: Option<&str>) -> &'static str {
    i18n::pick(q.or_else(|| h.get("accept-language").and_then(|v| v.to_str().ok())))
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

#[derive(Deserialize, Default)]
pub struct LangQuery {
    lang: Option<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/health", get(|| async { Json(json!({ "ok": true })) }))
        .route("/api/info", get(info))
        .route("/api/status", get(status))
        .route("/api/config", get(get_config).put(put_config).patch(patch_config))
        .route("/api/config/defaults", get(|| async { Json(Config::default()) }))
        .route("/api/chain", get(chain))
        .route("/api/meters", get(meters))
        .route("/api/meters/reset-loudness", post(reset_loudness))
        .route("/api/ws", get(ws))
        .route("/api/spectrum", get(spectrum))
        .route("/api/rds/radiotext", post(set_radiotext))
        .route("/api/presets", get(list_presets).post(save_preset))
        .route("/api/presets/:name", get(get_preset).delete(delete_preset))
        .route("/api/presets/:name/apply", post(apply_preset))
        .route("/api/i18n/:lang", get(i18n_table))
        .route("/api/log", get(log))
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state)
}

/// Writes need the API token when one is configured (PILOT_API_TOKEN).
async fn auth(State(st): State<AppState>, req: axum::extract::Request, next: Next) -> Response {
    if let Some(token) = &st.token {
        if req.method() != Method::GET {
            let ok = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.strip_prefix("Bearer ").unwrap_or("") == token)
                .unwrap_or(false);
            if !ok {
                let l = lang(req.headers(), None);
                return ApiError(StatusCode::UNAUTHORIZED, i18n::t(l, "error.unauthorized")).into_response();
            }
        }
    }
    next.run(req).await
}

async fn info(State(st): State<AppState>) -> Json<Value> {
    Json(json!({
        "name": "PILOT",
        "component": "engine",
        "version": version(),
        "git": option_env!("PILOT_GIT_SHA").unwrap_or("dev"),
        "channel": st.channel,
        "languages": i18n::languages(),
    }))
}

async fn status(State(st): State<AppState>) -> Json<Value> {
    let e = &st.engine;
    Json(json!({
        "engine": *e.status.lock(),
        "input": *e.input.lock(),
        "outputs": e.output_statuses(),
    }))
}

async fn get_config(State(st): State<AppState>) -> Json<Config> {
    Json(st.cfg.lock().clone())
}

fn apply(st: &AppState, mut cfg: Config, l: &str, action: &str) -> Result<Json<Value>, ApiError> {
    let adjusted = cfg.sanitize();
    st.db
        .save_config(&cfg)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, i18n::tf(l, "error.internal", &e.to_string())))?;
    st.engine.apply(&cfg);
    *st.cfg.lock() = cfg.clone();
    st.db.log(action, &if adjusted.is_empty() { String::new() } else { format!("adjusted: {}", adjusted.join(", ")) });
    let mut body = json!({ "config": cfg, "adjusted": adjusted });
    if !body["adjusted"].as_array().unwrap().is_empty() {
        body["notice"] = json!(i18n::t(l, "notice.adjusted"));
    }
    Ok(Json(body))
}

async fn put_config(
    State(st): State<AppState>,
    h: HeaderMap,
    Query(q): Query<LangQuery>,
    body: String,
) -> Result<Json<Value>, ApiError> {
    let l = lang(&h, q.lang.as_deref());
    let cfg: Config = serde_json::from_str(&body)
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, i18n::tf(l, "error.invalid_config", &e.to_string())))?;
    apply(&st, cfg, l, "config.replace")
}

async fn patch_config(
    State(st): State<AppState>,
    h: HeaderMap,
    Query(q): Query<LangQuery>,
    body: String,
) -> Result<Json<Value>, ApiError> {
    let l = lang(&h, q.lang.as_deref());
    let patch: Value = serde_json::from_str(&body).map_err(|_| ApiError(StatusCode::BAD_REQUEST, i18n::t(l, "error.invalid_json")))?;
    let mut cur = serde_json::to_value(&*st.cfg.lock()).unwrap();
    merge_patch(&mut cur, &patch);
    let cfg: Config = serde_json::from_value(cur)
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, i18n::tf(l, "error.invalid_config", &e.to_string())))?;
    apply(&st, cfg, l, "config.patch")
}

async fn chain(State(st): State<AppState>, h: HeaderMap, Query(q): Query<LangQuery>) -> Json<Value> {
    let l = lang(&h, q.lang.as_deref());
    let c = st.cfg.lock().clone();
    let m = st.engine.meters.lock().clone();
    let input = st.engine.input.lock().clone();
    let outs = st.engine.output_statuses();
    let p = &c.processing;
    let stage = |id: &str, enabled: bool, value: Value| json!({ "id": id, "name": i18n::t(l, &format!("chain.{id}")), "enabled": enabled, "value": value });
    Json(json!({ "stages": [
        stage("input", input.connected, json!({ "source": input.source, "codec": input.codec, "buffer_ms": input.buffer_ms, "loudness": m.input_loudness })),
        stage("agc", p.agc.enabled && !p.bypass, json!({ "gain_db": m.agc_gain_db, "gated": m.agc_gated })),
        stage("multiband", p.multiband.enabled && !p.bypass, json!({ "gain_reduction_db": m.band_gr_db, "crossovers_hz": p.multiband.crossovers_hz })),
        stage("emphasis", p.emphasis.tau().is_some(), json!({ "emphasis": p.emphasis })),
        stage("limiter", p.limiter.enabled && !p.bypass, json!({ "gain_reduction_db": m.limiter_gr_db })),
        stage("clipper", p.clipper.enabled && !p.bypass, json!({ "activity": m.clipper_activity })),
        stage("lowpass", true, json!({ "cutoff_hz": 15000 })),
        stage("stereo", c.stereo.stereo, json!({ "pilot_khz": m.pilot_khz })),
        stage("bs412", c.limits.mpx_power_enabled, json!({ "power_dbr": m.mpx_power_dbr, "limit_dbr": c.limits.mpx_power_dbr, "gain_db": m.bs412_gain_db })),
        stage("safety", true, json!({ "deviation_khz": m.deviation_khz, "max_khz": c.limits.max_deviation_khz, "gain_db": m.composite_limiter_db })),
        stage("rds", c.rds.enabled, json!({ "ps": m.rds_ps, "rt": m.rds_rt, "rds2": c.rds.rds2.enabled, "injection_khz": m.rds_khz })),
        stage("outputs", outs.iter().any(|o| o.enabled), json!(outs.iter().map(|o| json!({
            "id": o.name, "name": i18n::t(l, &format!("output.{}", o.name)), "enabled": o.enabled,
            "running": o.running, "role": o.role, "error": o.error })).collect::<Vec<_>>())),
    ]}))
}

async fn meters(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.engine.meters.lock().clone())
}

async fn reset_loudness(State(st): State<AppState>) -> impl IntoResponse {
    st.engine.reset_loudness();
    Json(json!({ "ok": true }))
}

async fn ws(State(st): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| ws_loop(socket, st))
}

async fn ws_loop(mut socket: WebSocket, st: AppState) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let mut last = 0u64;
    let mut n = 0u64;
    loop {
        tick.tick().await;
        let m = st.engine.meters.lock().clone();
        n += 1;
        if m.seq == last && n % 20 != 0 {
            continue;
        }
        last = m.seq;
        let mut msg = json!({ "type": "meters", "meters": m });
        if n % 20 == 0 {
            msg["status"] = json!({ "engine": *st.engine.status.lock(), "input": *st.engine.input.lock() });
        }
        if socket.send(Message::Text(msg.to_string())).await.is_err() {
            break;
        }
    }
}

#[derive(Deserialize)]
struct SpectrumQuery {
    bins: Option<usize>,
}

/// Magnitude spectrum of the MPX (0–96 kHz), dB relative to 100 % modulation.
async fn spectrum(State(st): State<AppState>, Query(q): Query<SpectrumQuery>) -> Json<Value> {
    let bins = q.bins.unwrap_or(512).clamp(64, 4096);
    let samples = st.engine.spectrum.lock().clone();
    let out = tokio::task::spawn_blocking(move || compute_spectrum(&samples, bins)).await.unwrap_or_default();
    Json(json!({ "sample_rate": 192000, "max_hz": 96000, "bins": out.len(), "db": out }))
}

pub fn compute_spectrum(samples: &[f32], bins: usize) -> Vec<f32> {
    use rustfft::{num_complex::Complex, FftPlanner};
    let n = SPECTRUM_SAMPLES.min(samples.len());
    let mut buf: Vec<Complex<f32>> = (0..n)
        .map(|i| {
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos();
            Complex::new(samples[i] * w, 0.0)
        })
        .collect();
    FftPlanner::new().plan_fft_forward(n).process(&mut buf);
    let half = n / 2;
    let per = (half / bins).max(1);
    // Peak per display bin; a full-scale sine reads 0 dB (Hann gain 0.5, two-sided ×2).
    (0..bins)
        .map(|b| {
            let mut peak = 0.0f32;
            for k in b * per..((b + 1) * per).min(half) {
                peak = peak.max(buf[k].norm());
            }
            20.0 * (peak * 4.0 / n as f32).max(1e-7).log10()
        })
        .collect()
}

#[derive(Deserialize)]
struct RtBody {
    text: String,
}

async fn set_radiotext(State(st): State<AppState>, Json(b): Json<RtBody>) -> impl IntoResponse {
    let text: String = b.text.chars().take(64).collect();
    st.engine.set_radiotext(&text);
    st.db.log("rds.radiotext", &text);
    Json(json!({ "ok": true, "text": text }))
}

async fn list_presets(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let p = st.db.list_presets().map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "presets": p })))
}

#[derive(Deserialize)]
struct PresetBody {
    name: String,
    #[serde(default)]
    description: String,
    /// Processing section to store; the current one when omitted.
    processing: Option<Value>,
}

async fn save_preset(State(st): State<AppState>, Json(b): Json<PresetBody>) -> Result<Json<Value>, ApiError> {
    let name = b.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "name must be 1-64 characters".into()));
    }
    let p = b.processing.unwrap_or_else(|| serde_json::to_value(&st.cfg.lock().processing).unwrap());
    st.db.save_preset(name, &b.description, &p).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    st.db.log("preset.save", name);
    Ok(Json(json!({ "ok": true })))
}

async fn get_preset(State(st): State<AppState>, h: HeaderMap, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    match st.db.get_preset(&name) {
        Ok(Some(v)) => Ok(Json(json!({ "name": name, "processing": v }))),
        _ => Err(ApiError(StatusCode::NOT_FOUND, i18n::t(lang(&h, None), "error.not_found"))),
    }
}

async fn delete_preset(State(st): State<AppState>, h: HeaderMap, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    match st.db.delete_preset(&name) {
        Ok(true) => {
            st.db.log("preset.delete", &name);
            Ok(Json(json!({ "ok": true })))
        }
        _ => Err(ApiError(StatusCode::NOT_FOUND, i18n::t(lang(&h, None), "error.not_found"))),
    }
}

async fn apply_preset(State(st): State<AppState>, h: HeaderMap, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    let l = lang(&h, None);
    let p = match st.db.get_preset(&name) {
        Ok(Some(v)) => v,
        _ => return Err(ApiError(StatusCode::NOT_FOUND, i18n::t(l, "error.not_found"))),
    };
    let mut cur = serde_json::to_value(&*st.cfg.lock()).unwrap();
    merge_patch(&mut cur, &json!({ "processing": p }));
    let cfg: Config = serde_json::from_value(cur).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    apply(&st, cfg, l, &format!("preset.apply {name}"))
}

async fn i18n_table(Path(l): Path<String>) -> Json<Value> {
    Json(json!(i18n::table(&l)))
}

#[derive(Deserialize)]
struct LogQuery {
    limit: Option<u32>,
}

async fn log(State(st): State<AppState>, Query(q): Query<LogQuery>) -> Result<Json<Value>, ApiError> {
    let entries = st.db.recent_log(q.limit.unwrap_or(100).min(1000)).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "entries": entries })))
}

/// Minimal live status page until the phase 3 web interface replaces it.
async fn index(h: HeaderMap, Query(q): Query<LangQuery>) -> Html<String> {
    let l = lang(&h, q.lang.as_deref());
    let t = |k: &str| i18n::t(l, k);
    Html(format!(
        r#"<!doctype html><html lang="{l}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<style>
:root{{--void:#04070E;--deep:#070C18;--text:#E8F1FF;--text2:#A9B8D6;--text3:#6F81A6;--ion:#5CE1FF;--plasma:#9D7BFF;--ok:#3DFFB0;--warn:#FFB547;--crit:#FF4D6A}}
body{{margin:0;background:var(--void);color:var(--text);font:15px/1.5 Manrope,system-ui,sans-serif}}
main{{max-width:880px;margin:0 auto;padding:32px 16px}}
h1{{font:600 28px Unbounded,system-ui,sans-serif;margin:0 0 4px;background:linear-gradient(135deg,#5CE1FF,#9D7BFF);-webkit-background-clip:text;color:transparent}}
p{{color:var(--text2)}} .grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(190px,1fr));gap:12px;margin-top:24px}}
.card{{background:rgba(20,30,54,.55);border:1px solid rgba(92,225,255,.12);border-radius:14px;padding:16px}}
.k{{font-size:12px;letter-spacing:.14em;text-transform:uppercase;color:var(--text3)}}
.v{{font:600 28px 'JetBrains Mono',ui-monospace,monospace;margin-top:6px}} a{{color:var(--ion)}}
</style></head><body><main>
<h1>PILOT</h1><p>{tagline} · v{ver}</p><p>{body}</p>
<div class="grid">
<div class="card"><div class="k">{dev}</div><div class="v" id="dev">–</div></div>
<div class="card"><div class="k">{pwr}</div><div class="v" id="pwr">–</div></div>
<div class="card"><div class="k">{loud}</div><div class="v" id="lufs">–</div></div>
<div class="card"><div class="k">{inp}</div><div class="v" id="inp" style="font-size:16px">–</div></div>
</div>
<p style="margin-top:24px"><a href="/api/status">/api/status</a> · <a href="/api/chain">/api/chain</a> · <a href="/api/config">/api/config</a> · <a href="?lang=en">EN</a> · <a href="?lang=nl">NL</a></p>
</main><script>
const ws=new WebSocket((location.protocol==='https:'?'wss://':'ws://')+location.host+'/api/ws');
ws.onmessage=e=>{{const d=JSON.parse(e.data),m=d.meters;
const dev=document.getElementById('dev');dev.textContent=m.deviation_hold_khz.toFixed(1)+' kHz';dev.style.color=m.deviation_hold_khz>m.max_deviation_khz-0.5?'var(--warn)':'var(--ok)';
document.getElementById('pwr').textContent=(m.mpx_power_dbr>0?'+':'')+m.mpx_power_dbr.toFixed(2)+' dBr';
document.getElementById('lufs').textContent=m.output_loudness.short_term.toFixed(1)+' LUFS';
if(d.status){{const i=d.status.input;document.getElementById('inp').textContent=i.connected?(i.source+' · '+i.codec+' · '+i.buffer_ms+' ms'):'{buffering}';}}}};
</script></body></html>"#,
        title = t("page.title"),
        tagline = t("app.tagline"),
        ver = version(),
        body = t("page.body"),
        dev = t("page.deviation"),
        pwr = t("page.mpx_power"),
        loud = t("page.loudness"),
        inp = t("page.input"),
        buffering = t("status.buffering"),
    ))
}
