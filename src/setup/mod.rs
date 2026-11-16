//! Desktop setup tool: a local web app to connect Home Assistant, design the
//! dashboard with a pixel-exact preview, and install everything on a Kobo.

pub mod generate;
pub mod kobo;
pub mod versions;

use crate::config::AppConfig;
use crate::display::canvas::{has_fallback_font, load_fallback_font};
use crate::display::Canvas;
use crate::ha::HaClient;
use crate::i18n;
use crate::paths;
use crate::ui::screen::hidden_card_count;
use crate::ui::ScreenRenderer;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use generate::EntityInfo;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

const EDITOR_HTML: &str = include_str!("editor.html");

/// Icons offered in the editor's icon picker
pub const ICONS: &[&str] = &[
    "lightbulb", "lightbulb_on", "power", "plug", "fan", "thermometer", "flame", "snowflake", "droplet", "wind", "zap",
    "sun", "moon", "cloud", "home", "sofa", "bed", "door", "lock", "tv", "music", "coffee", "washer", "printer",
    "battery", "clock", "layers", "progress", "play", "refresh", "check",
];

struct Inner {
    config_path: PathBuf,
    /// What the editor shows; only written to disk on Save.
    draft: RwLock<AppConfig>,
    /// YAML of the last saved state, to detect unsaved changes and revert.
    saved: RwLock<String>,
    ha: RwLock<HaClient>,
    entities: RwLock<Vec<EntityInfo>>,
    menu_name: RwLock<String>,
    kobo_binary: Option<PathBuf>,
    /// Rendering touches global state (language, fallback font); one at a time.
    render_lock: Mutex<()>,
    loaded_font: Mutex<Option<String>>,
    /// Fired by the "Quit" button to stop the server.
    shutdown: tokio::sync::Notify,
    /// The preview as a device being tapped
    preview_view: Mutex<PreviewView>,
    /// Port of the setup page; the update server takes the next one.
    port: u16,
    /// Port of the running update server, once it has been started
    update_server: Mutex<Option<u16>>,
    /// Serial of the newest bundle a Kobo has fetched from the update server
    fetched_serial: Arc<std::sync::atomic::AtomicU64>,
}

type Shared = Arc<Inner>;

pub async fn start_setup_server(
    config_path: PathBuf,
    port: u16,
    open_browser: bool,
    kobo_binary: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = if config_path.exists() {
        AppConfig::load_or_default(&config_path)
    } else {
        // Fresh start: no demo pages, the user connects HA first.
        AppConfig { pages: Vec::new(), ..AppConfig::default() }
    };
    let ha = HaClient::for_app(&config);
    spawn_refresh(ha.clone());

    let state: Shared = Arc::new(Inner {
        config_path,
        saved: RwLock::new(serde_yaml::to_string(&config).unwrap_or_default()),
        draft: RwLock::new(config),
        ha: RwLock::new(ha),
        entities: RwLock::new(Vec::new()),
        menu_name: RwLock::new(kobo::DEFAULT_MENU_NAME.to_string()),
        kobo_binary: kobo::find_kobo_binary(kobo_binary.as_deref()),
        render_lock: Mutex::new(()),
        loaded_font: Mutex::new(None),
        shutdown: tokio::sync::Notify::new(),
        preview_view: Mutex::new(PreviewView::default()),
        port,
        update_server: Mutex::new(None),
        fetched_serial: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    });
    // A Kobo set to fetch from this computer can catch up as soon as the
    // tool is open again.
    if !state.draft.read().await.updates.url.is_empty() && bundle_dir(&state).join("manifest.json").exists() {
        if let Err(e) = ensure_update_server(&state).await {
            tracing::warn!("Update server not started: {}", e);
        }
    }

    let app = Router::new()
        .route("/", get(|| async { Html(EDITOR_HTML) }))
        .route("/api/state", get(get_state))
        .route("/api/config", post(put_config))
        .route("/api/save", post(save_config))
        .route("/api/revert", post(revert_config))
        .route("/api/versions", get(list_versions))
        .route("/api/versions/restore", post(restore_version))
        .route("/api/ha/test", post(test_ha))
        .route("/api/ha/entities", get(get_entities))
        .route("/api/generate", post(generate_pages))
        .route("/api/preview.png", get(preview))
        .route("/api/preview/tap", post(preview_tap))
        .route("/api/icon.png", get(icon_png))
        .route("/api/kobo", get(list_kobos))
        .route("/api/kobo/install", post(install_kobo))
        .route("/api/publish", post(publish_update))
        .route("/api/publish/status", get(publish_status))
        .route("/api/kobo/import", post(import_kobo))
        .route("/api/kobo/nickelmenu", post(install_nickelmenu))
        .route("/api/fonts/ensure", post(ensure_font))
        .route("/api/setup-strings", get(setup_strings))
        .route("/api/shutdown", post(shutdown))
        .with_state(state.clone());

    // Local only: the page handles a Home Assistant token.
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let url = format!("http://localhost:{}", port);
    println!();
    println!("  Kobo HA Dashboard setup is running at {}", url);
    println!("  Config file: {}", state.config_path.display());
    match &state.kobo_binary {
        Some(p) => println!("  Kobo program: {}", p.display()),
        None => println!("  Warning: the Kobo (ARM) program was not found next to this tool; installing will not work."),
    }
    println!("  Press Ctrl+C to stop.");
    println!();
    if open_browser {
        open_url(&url);
    }
    let stop = state.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { stop.shutdown.notified().await })
        .await?;
    println!("  Setup closed. Your configuration is saved in {}", state.config_path.display());
    Ok(())
}

fn open_url(url: &str) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = result {
        tracing::warn!("Could not open a browser ({}); open {} manually", e, url);
    }
}

fn spawn_refresh(ha: HaClient) {
    tokio::spawn(async move {
        let _ = ha.refresh_states().await;
    });
}

fn bad_request(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// Error the editor can translate: `key` under `errors.`
fn bad_request_key(key: &str, english: &str, args: serde_json::Value) -> Response {
    (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": english, "key": key, "args": args }))).into_response()
}

fn hidden_per_page(config: &AppConfig) -> Vec<usize> {
    let (w, h) = config.device.get_dimensions();
    let tabs = config.pages.len() > 1;
    config.pages.iter().map(|p| hidden_card_count(&p.cards, w, h, tabs)).collect()
}

async fn get_state(State(s): State<Shared>) -> Json<serde_json::Value> {
    let config = s.draft.read().await.clone();
    let devices = kobo::detect();
    // Prefer the name already on a connected Kobo.
    if let Some(name) = devices.iter().find_map(|d| kobo::read_menu_name(Path::new(&d.path))) {
        *s.menu_name.write().await = name;
    }
    Json(serde_json::json!({
        "config": config,
        "config_path": s.config_path,
        "hidden": hidden_per_page(&config),
        "languages": i18n::available(),
        "icons": ICONS,
        "models": crate::models::MODELS,
        "devices": devices,
        "kobo_binary": s.kobo_binary.is_some(),
        "menu_name": *s.menu_name.read().await,
        "connected": s.ha.read().await.is_connected().await,
        "dirty": is_dirty(&s).await,
    }))
}

/// Replace the draft. Nothing is written to disk until Save; the HA client
/// is rebuilt when the connection settings change.
async fn put_config(State(s): State<Shared>, Json(config): Json<AppConfig>) -> Response {
    set_draft(&s, config).await;
    Json(draft_status(&s).await).into_response()
}

async fn set_draft(s: &Shared, config: AppConfig) {
    let old = s.draft.read().await.clone();
    let ha_changed = old.home_assistant.url != config.home_assistant.url
        || old.home_assistant.token != config.home_assistant.token
        || entity_set(&old) != entity_set(&config);
    if ha_changed {
        let ha = HaClient::for_app(&config);
        spawn_refresh(ha.clone());
        *s.ha.write().await = ha;
    }
    *s.draft.write().await = config;
}

async fn is_dirty(s: &Shared) -> bool {
    let draft = serde_yaml::to_string(&*s.draft.read().await).unwrap_or_default();
    draft != *s.saved.read().await
}

async fn draft_status(s: &Shared) -> serde_json::Value {
    let draft = s.draft.read().await.clone();
    serde_json::json!({ "hidden": hidden_per_page(&draft), "dirty": is_dirty(s).await })
}

/// Write the draft to disk, keeping the previous file in `backups/`.
async fn write_draft(s: &Shared) -> Result<(), String> {
    let yaml = serde_yaml::to_string(&*s.draft.read().await).map_err(|e| e.to_string())?;
    versions::save(&s.config_path, &yaml).map_err(|e| format!("{}: {}", s.config_path.display(), e))?;
    *s.saved.write().await = yaml;
    Ok(())
}

async fn save_config(State(s): State<Shared>) -> Response {
    match write_draft(&s).await {
        Ok(()) => Json(draft_status(&s).await).into_response(),
        Err(e) => bad_request_key("save_failed", &e, serde_json::json!({ "error": e })),
    }
}

/// Throw away unsaved changes.
async fn revert_config(State(s): State<Shared>) -> Response {
    let saved = s.saved.read().await.clone();
    match serde_yaml::from_str::<AppConfig>(&saved) {
        Ok(config) => {
            set_draft(&s, config.clone()).await;
            let mut status = draft_status(&s).await;
            status["config"] = serde_json::to_value(config).unwrap_or_default();
            Json(status).into_response()
        }
        Err(e) => bad_request(e.to_string()),
    }
}

async fn list_versions(State(s): State<Shared>) -> Json<Vec<versions::Version>> {
    Json(versions::list(&s.config_path))
}

#[derive(Deserialize)]
struct RestoreRequest {
    id: String,
}

/// Load an earlier version into the draft (saved only when the user saves).
async fn restore_version(State(s): State<Shared>, Json(req): Json<RestoreRequest>) -> Response {
    match versions::load(&s.config_path, &req.id) {
        Ok(config) => {
            set_draft(&s, config.clone()).await;
            let mut status = draft_status(&s).await;
            status["config"] = serde_json::to_value(config).unwrap_or_default();
            Json(status).into_response()
        }
        Err(e) => bad_request(e),
    }
}

/// Weather/graph entities need extra API calls, so a new HA client is built
/// when they change.
fn entity_set(c: &AppConfig) -> Vec<String> {
    use crate::config::CardConfig;
    c.pages
        .iter()
        .flat_map(|p| p.cards.iter())
        .filter_map(|card| match card {
            CardConfig::Weather { entity_id, .. } => Some(entity_id.clone()),
            CardConfig::Sensor { entity_id, show_graph: true, .. } => Some(entity_id.clone()),
            _ => None,
        })
        .collect()
}

#[derive(Deserialize)]
struct TestRequest {
    url: String,
    token: String,
    /// Set when battery reporting is on: check that the token may write it.
    #[serde(default)]
    battery_entity: Option<String>,
}

async fn test_ha(Json(req): Json<TestRequest>) -> Json<generate::ConnectionReport> {
    let mut report = generate::test_connection(&req.url, &req.token).await;
    if report.ok {
        if let Some(entity) = &req.battery_entity {
            if let Some(warning) = generate::prepare_battery_sensor(&req.url, &req.token, entity).await {
                report.warnings.push(warning);
            }
        }
    }
    Json(report)
}

async fn load_entities(s: &Shared, force: bool) -> Result<Vec<EntityInfo>, String> {
    if !force {
        let cached = s.entities.read().await;
        if !cached.is_empty() {
            return Ok(cached.clone());
        }
    }
    let (url, token) = {
        let d = s.draft.read().await;
        (d.home_assistant.url.clone(), d.home_assistant.token.clone())
    };
    let list = generate::fetch_entities(&url, &token).await.map_err(|e| e.to_string())?;
    *s.entities.write().await = list.clone();
    Ok(list)
}

#[derive(Deserialize)]
struct EntitiesQuery {
    #[serde(default)]
    refresh: bool,
}

async fn get_entities(State(s): State<Shared>, Query(q): Query<EntitiesQuery>) -> Response {
    match load_entities(&s, q.refresh).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn generate_pages(State(s): State<Shared>) -> Response {
    let entities = match load_entities(&s, true).await {
        Ok(list) => list,
        Err(e) => return bad_request(e),
    };
    let (lang, dims) = {
        let d = s.draft.read().await;
        (d.theme.language.clone(), d.device.get_dimensions())
    };
    let _guard = s.render_lock.lock().await;
    i18n::set_language(&lang);
    Json(generate::generate(&entities, dims)).into_response()
}

/// Load the fallback font a language needs (if downloaded) before rendering.
async fn prepare_font(s: &Shared, lang: &str) {
    let family = i18n::font_for(lang);
    let mut loaded = s.loaded_font.lock().await;
    if *loaded != family || (family.is_some() && !has_fallback_font()) {
        let dir = s.config_path.parent().map(Path::to_path_buf).unwrap_or_default();
        load_fallback_font(&paths::font_dirs(&dir), family.as_deref());
        *loaded = family;
    }
}

#[derive(Deserialize)]
struct PreviewQuery {
    #[serde(default)]
    page: usize,
    #[serde(default)]
    bw: bool,
    /// Draw the sleep-mode note, as the Kobo shows it while asleep
    #[serde(default)]
    sleep: bool,
}

/// How long a lock tapped in the preview waits for its confirming tap, as on the Kobo
const PREVIEW_UNLOCK_CONFIRM: std::time::Duration = std::time::Duration::from_secs(6);

/// What the preview's screen shows beyond the chosen page, so that it can be
/// clicked through like the Kobo is tapped through.
#[derive(Default)]
struct PreviewView {
    weather_detail: Option<(String, String)>,
    unlock_armed: Option<(String, std::time::Instant)>,
    frontlight_off: bool,
    /// States changed by clicks, laid over the real ones. Nothing clicked in
    /// the preview is sent to Home Assistant: a click on the front door lock
    /// must not unlock the front door.
    changed: std::collections::HashMap<String, crate::ha::models::EntityState>,
}

impl PreviewView {
    fn expire(&mut self) {
        if self.unlock_armed.as_ref().is_some_and(|(_, at)| at.elapsed() > PREVIEW_UNLOCK_CONFIRM) {
            self.unlock_armed = None;
        }
    }

    fn show(&self, renderer: &mut ScreenRenderer) {
        renderer.weather_detail = self.weather_detail.clone();
        renderer.unlock_armed = self.unlock_armed.as_ref().map(|(id, _)| id.clone());
        renderer.frontlight = Some(!self.frontlight_off);
    }
}

#[derive(Deserialize)]
struct TapRequest {
    /// Where on the preview, as fractions of its width and height
    x: f32,
    y: f32,
    #[serde(default)]
    page: usize,
}

/// A click on the preview, treated as a finger on the Kobo's screen: find
/// what is under it and do to the preview what the Kobo would do.
async fn preview_tap(State(s): State<Shared>, Json(req): Json<TapRequest>) -> Json<serde_json::Value> {
    use crate::ui::layout::UiAction;
    let config = s.draft.read().await.clone();
    let ha = s.ha.read().await.clone();
    let mut view = s.preview_view.lock().await;
    view.expire();
    let mut states = ha.get_all_states().await;
    states.extend(view.changed.clone());
    let (w, h) = config.device.get_dimensions();
    let mut page = req.page.min(config.pages.len().saturating_sub(1));

    let action = {
        let _guard = s.render_lock.lock().await;
        i18n::set_language(&config.theme.language);
        prepare_font(&s, &config.theme.language).await;
        let mut canvas = Canvas::for_screen(w, h);
        let mut renderer = ScreenRenderer::new(&config);
        view.show(&mut renderer);
        renderer.render(&mut canvas, &config, page, &states, true);
        renderer.handle_touch(req.x.clamp(0.0, 1.0) * w as f32, req.y.clamp(0.0, 1.0) * h as f32)
    };

    // As on the Kobo: only a second tap on the same lock confirms an unlock.
    let armed = view.unlock_armed.take();
    fn simulate(view: &mut PreviewView, states: &mut std::collections::HashMap<String, crate::ha::models::EntityState>, service: &str, data: serde_json::Value) {
        crate::ha::client::simulate_service(states, service, &data);
        let ids: Vec<String> = match &data["entity_id"] {
            serde_json::Value::String(id) => vec![id.clone()],
            serde_json::Value::Array(list) => list.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        };
        for id in ids {
            if let Some(state) = states.get(&id) {
                view.changed.insert(id, state.clone());
            }
        }
    }
    let entity = |id: &str| serde_json::json!({ "entity_id": id });
    let name = match action {
        None => "none",
        Some(UiAction::SwitchPage(idx)) => {
            page = idx;
            view.weather_detail = None;
            "page"
        }
        Some(UiAction::OpenWeatherDay(e, day)) => {
            view.weather_detail = Some((e, day));
            "weather_day"
        }
        Some(UiAction::CloseDetail) => {
            view.weather_detail = None;
            "back"
        }
        Some(UiAction::Refresh) => {
            // The refresh button puts the preview back to the real states.
            view.changed.clear();
            "refresh"
        }
        Some(UiAction::ToggleFrontlight) => {
            view.frontlight_off = !view.frontlight_off;
            "frontlight"
        }
        Some(UiAction::ExitToReader) => "exit",
        Some(UiAction::LockTap(id)) => {
            let unlocked = states.get(&id).map(|st| crate::ui::widgets::lock_card::is_unlocked(&st.state)).unwrap_or(false);
            if unlocked {
                simulate(&mut view, &mut states, "lock", entity(&id));
            } else if armed.as_ref().is_some_and(|(a, _)| *a == id) {
                simulate(&mut view, &mut states, "unlock", entity(&id));
            } else {
                view.unlock_armed = Some((id, std::time::Instant::now()));
            }
            "lock"
        }
        Some(UiAction::ToggleEntity(id)) => {
            simulate(&mut view, &mut states, "toggle", entity(&id));
            "toggle"
        }
        Some(UiAction::SetClimateTemp(id, delta)) => {
            let now = states.get(&id).and_then(|st| st.get_attr_f32("temperature")).unwrap_or(21.0);
            let target = ((now + delta) * 2.0).round() / 2.0;
            simulate(&mut view, &mut states, "set_temperature", serde_json::json!({ "entity_id": id, "temperature": target }));
            "climate"
        }
        Some(UiAction::MediaPlayPause(id)) => {
            simulate(&mut view, &mut states, "media_play_pause", entity(&id));
            "media"
        }
        Some(UiAction::MediaNext(_)) | Some(UiAction::MediaPrevious(_)) => "media",
        Some(UiAction::ActivateScene(id)) => {
            let (_, service, data) = ha.activate_request(&id);
            simulate(&mut view, &mut states, &service, data);
            "scene"
        }
        Some(UiAction::Service { service, data, .. }) => {
            simulate(&mut view, &mut states, &service, data);
            "service"
        }
    };
    Json(serde_json::json!({ "page": page, "action": name }))
}

async fn preview(State(s): State<Shared>, Query(q): Query<PreviewQuery>) -> Response {
    let config = s.draft.read().await.clone();
    let ha = s.ha.read().await.clone();
    let mut states = ha.get_all_states().await;
    let live = ha.is_connected().await;
    let (w, h) = config.device.get_dimensions();
    let view = {
        let mut view = s.preview_view.lock().await;
        view.expire();
        states.extend(view.changed.clone());
        PreviewView { weather_detail: view.weather_detail.clone(), unlock_armed: view.unlock_armed.clone(), frontlight_off: view.frontlight_off, changed: Default::default() }
    };

    let _guard = s.render_lock.lock().await;
    i18n::set_language(&config.theme.language);
    prepare_font(&s, &config.theme.language).await;
    let mut canvas = Canvas::for_screen(w, h);
    let mut renderer = ScreenRenderer::new(&config);
    renderer.last_update = ha.last_update().await;
    // Most Kobos have a frontlight; its button is shown as on the device.
    view.show(&mut renderer);
    if q.sleep {
        let next = crate::ui::widgets::header::now_local(&config.theme) + chrono::Duration::seconds(config.device.sleep_wake_secs.max(120) as i64);
        renderer.sleep_until = Some(crate::ui::widgets::header::format_time(&next, &config.theme));
    }
    renderer.render(&mut canvas, &config, q.page.min(config.pages.len().saturating_sub(1)), &states, live);
    let hidden = renderer.hidden_cards;
    let png = if q.bw { canvas.to_grayscale_png_bytes() } else { canvas.to_png_bytes() };
    match png {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "image/png".to_string()),
                (header::CACHE_CONTROL, "no-store".to_string()),
                (header::HeaderName::from_static("x-hidden-cards"), hidden.to_string()),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => bad_request(e.to_string()),
    }
}

#[derive(Deserialize)]
struct IconQuery {
    name: String,
}

async fn icon_png(Query(q): Query<IconQuery>) -> Response {
    let mut canvas = Canvas::new(48, 48);
    canvas.clear(crate::display::Color::rgba(0, 0, 0, 0));
    canvas.draw_icon(&q.name, 6.0, 6.0, 36.0, crate::display::Color::rgb(20, 24, 32), 2.2);
    match canvas.to_png_bytes() {
        Ok(bytes) => ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "max-age=3600")], bytes).into_response(),
        Err(e) => bad_request(e.to_string()),
    }
}

async fn list_kobos(State(s): State<Shared>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "devices": kobo::detect(), "kobo_binary": s.kobo_binary.is_some() }))
}

#[derive(Deserialize)]
struct InstallRequest {
    path: String,
    menu_name: String,
}

async fn install_kobo(State(s): State<Shared>, Json(req): Json<InstallRequest>) -> Response {
    let Some(binary) = s.kobo_binary.clone() else {
        return bad_request_key("binary_missing", "The Kobo program was not found next to the setup tool.", serde_json::json!({}));
    };
    // What goes on the Kobo is also what is saved on this computer.
    if let Err(e) = write_draft(&s).await {
        return bad_request_key("save_failed", &e, serde_json::json!({ "error": e }));
    }
    let config = s.draft.read().await.clone();
    let mut log = vec![kobo::Msg::new("saved")];

    let mut fonts = Vec::new();
    if let Some(family) = i18n::font_for(&config.theme.language) {
        log.push(kobo::Msg::new("font_download").arg("family", family.as_str()));
        match paths::ensure_font(&family).await {
            Ok(files) => fonts = files,
            Err(e) => return bad_request_key("font_failed", "Font download failed", serde_json::json!({ "error": e.to_string() })),
        }
    }

    *s.menu_name.write().await = req.menu_name.clone();
    match kobo::install(Path::new(&req.path), &config, &req.menu_name, &binary, &fonts) {
        Ok(mut steps) => {
            log.append(&mut steps);
            Json(serde_json::json!({ "log": log })).into_response()
        }
        Err(e) => bad_request_key("install_failed", &e, serde_json::json!({ "error": e })),
    }
}

#[derive(Deserialize)]
struct PublishRequest {
    /// "direct": the Kobo fetches from this tool. "folder": from a folder
    /// Home Assistant serves, given in `dir`.
    #[serde(default)]
    mode: String,
    #[serde(default)]
    dir: String,
}

/// Where bundles served straight from this computer are kept.
fn bundle_dir(s: &Shared) -> PathBuf {
    s.config_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new(".")).join("published")
}

/// This computer's address on the network that leads to Home Assistant,
/// which is the network the Kobo is on too.
fn lan_ip_towards(ha_url: &str) -> Option<std::net::IpAddr> {
    use std::net::ToSocketAddrs;
    let rest = ha_url.split("://").nth(1).unwrap_or(ha_url);
    let host = rest.split('/').next().unwrap_or("");
    let target = if host.contains(':') { host.to_string() } else { format!("{}:8123", host) };
    let addr = target.to_socket_addrs().ok()?.find(|a| a.is_ipv4())?;
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // Nothing is sent: connecting a UDP socket only picks the route.
    socket.connect(addr).ok()?;
    socket.local_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
}

/// Start the update server if it is not running: a second, separate listener
/// reachable from the network that hands out the signed bundle and nothing
/// else. The setup page itself, which handles the Home Assistant token, stays
/// on localhost.
async fn ensure_update_server(s: &Shared) -> Result<u16, String> {
    let mut running = s.update_server.lock().await;
    if let Some(port) = *running {
        return Ok(port);
    }
    let port = s.port.checked_add(1).ok_or("no port left for the update server")?;
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port)))
        .await
        .map_err(|e| format!("port {}: {}", port, e))?;
    let dir = bundle_dir(s);
    let fetched = s.fetched_serial.clone();
    let app = Router::new().route(
        "/kobo-ha/:name",
        get(move |axum::extract::Path(name): axum::extract::Path<String>, Query(q): Query<std::collections::HashMap<String, String>>| {
            let (dir, fetched) = (dir.clone(), fetched.clone());
            async move {
                if !crate::update::servable(&name) {
                    return Err(StatusCode::NOT_FOUND);
                }
                let bytes = std::fs::read(dir.join(&name)).map_err(|_| StatusCode::NOT_FOUND)?;
                // A Kobo asks for the layout of every bundle it takes, naming
                // the bundle's serial: that is how the page knows it arrived.
                if name == crate::update::CONFIG {
                    if let Some(serial) = q.get("t").and_then(|t| t.parse::<u64>().ok()) {
                        fetched.fetch_max(serial, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                Ok(bytes)
            }
        }),
    );
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!("Update server stopped: {}", e);
        }
    });
    println!("  Update server for the Kobo: port {}", port);
    *running = Some(port);
    Ok(port)
}

async fn publish_status(State(s): State<Shared>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "fetched_serial": s.fetched_serial.load(std::sync::atomic::Ordering::Relaxed) }))
}

/// Publish a signed update bundle (layout, Kobo program, launcher scripts)
/// for the Kobo to fetch over Wi-Fi: from this tool's own update server, or
/// from a folder Home Assistant serves.
async fn publish_update(State(s): State<Shared>, Json(req): Json<PublishRequest>) -> Response {
    let Some(binary) = s.kobo_binary.clone() else {
        return bad_request_key("binary_missing", "The Kobo program was not found next to the setup tool.", serde_json::json!({}));
    };
    let failed = |e: String| bad_request_key("publish_failed", &e, serde_json::json!({ "error": e }));
    let direct = req.mode != "folder";
    let (dir, url) = if direct {
        let ha_url = s.draft.read().await.home_assistant.url.clone();
        let Some(ip) = lan_ip_towards(&ha_url) else {
            return failed("could not work out this computer's network address; check the Home Assistant URL".to_string());
        };
        match ensure_update_server(&s).await {
            Ok(port) => (bundle_dir(&s), format!("http://{}:{}/kobo-ha", ip, port)),
            Err(e) => return failed(e),
        }
    } else {
        let dir = req.dir.trim();
        if dir.is_empty() {
            return failed("no folder given".to_string());
        }
        (PathBuf::from(dir), String::new())
    };
    // The Kobo learns the key, and where to look, over USB: say so whenever
    // either is new to it.
    let needs_usb = {
        let mut draft = s.draft.write().await;
        let mut changed = !draft.updates.enabled || draft.updates.url != url;
        if draft.updates.key.is_empty() {
            match crate::update::new_key() {
                Ok(key) => draft.updates.key = key,
                Err(e) => return failed(e),
            }
            changed = true;
        }
        draft.updates.enabled = true;
        draft.updates.url = url.clone();
        draft.updates.publish_dir = if direct { String::new() } else { dir.display().to_string() };
        changed
    };
    if let Err(e) = write_draft(&s).await {
        return bad_request_key("save_failed", &e, serde_json::json!({ "error": e }));
    }
    let config = s.draft.read().await.clone();
    let serial = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    match crate::update::publish(&dir, &config, &binary, kobo::LAUNCH_SH, kobo::WIFI_SH, serial) {
        Ok(m) => Json(serde_json::json!({
            "updates": config.updates,
            "needs_usb": needs_usb,
            "direct": direct,
            "serial": m.serial,
            "url": crate::update::base_url(&config),
        }))
        .into_response(),
        Err(e) => failed(e),
    }
}

#[derive(Deserialize)]
struct NickelMenuRequest {
    path: String,
}

async fn install_nickelmenu(Json(req): Json<NickelMenuRequest>) -> Response {
    let root = Path::new(&req.path);
    // .kobo/version: serial,kernel,firmware,...
    let firmware = std::fs::read_to_string(root.join(".kobo/version"))
        .ok()
        .and_then(|v| v.trim().split(',').nth(2).map(|f| f.trim().to_string()))
        .filter(|f| !f.is_empty());
    let Some(package) = kobo::nickelmenu_for(firmware.as_deref()) else {
        let version = firmware.unwrap_or_default();
        return bad_request_key("firmware_unsupported", "No NickelMenu for this Kobo software", serde_json::json!({ "version": version }));
    };
    let mut log = vec![kobo::Msg::new("nm_download").arg("version", package.version)];
    let bytes = match paths::nickelmenu_package(package).await {
        Ok(p) => p,
        Err(e) => return bad_request_key("nm_failed", "Could not install NickelMenu", serde_json::json!({ "error": e.to_string() })),
    };
    match kobo::install_nickelmenu(root, &bytes, package.file_name) {
        Ok(()) => {
            log.push(kobo::Msg::new("nm_copied").arg("version", package.version));
            Json(serde_json::json!({ "log": log })).into_response()
        }
        Err(e) if e == "other_package_pending" => {
            bad_request_key("nm_other_pending", "Another update package is already waiting on this Kobo", serde_json::json!({}))
        }
        Err(e) => bad_request_key("nm_failed", "Could not install NickelMenu", serde_json::json!({ "error": e })),
    }
}

#[derive(Deserialize)]
struct ImportRequest {
    path: String,
}

async fn import_kobo(Json(req): Json<ImportRequest>) -> Response {
    let path = Path::new(&req.path).join(".kobo-ha/config.yaml");
    match AppConfig::load_from_file(&path) {
        Ok(config) => Json(config).into_response(),
        Err(e) => bad_request_key(
            "import_failed",
            "Could not read the config",
            serde_json::json!({ "path": path.display().to_string(), "error": e.to_string() }),
        ),
    }
}

#[derive(Deserialize)]
struct FontQuery {
    lang: String,
}

async fn ensure_font(State(s): State<Shared>, Query(q): Query<FontQuery>) -> Response {
    let Some(family) = i18n::font_for(&q.lang) else {
        return Json(serde_json::json!({ "needed": false })).into_response();
    };
    match paths::ensure_font(&family).await {
        Ok(_) => {
            *s.loaded_font.lock().await = None;
            Json(serde_json::json!({ "needed": true, "ready": true, "family": family })).into_response()
        }
        Err(e) => bad_request_key("font_failed", "Font download failed", serde_json::json!({ "error": e.to_string() })),
    }
}

#[derive(Deserialize)]
struct StringsQuery {
    lang: String,
}

/// Editor translations for one language, English filling any gaps.
async fn setup_strings(Query(q): Query<StringsQuery>) -> Json<std::collections::HashMap<String, String>> {
    Json(i18n::setup_strings(&q.lang))
}

/// Stop the setup tool. The reply goes out first, then the server shuts down.
async fn shutdown(State(s): State<Shared>) -> Json<serde_json::Value> {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        s.shutdown.notify_one();
        // A browser tab holding a connection open must not keep us alive.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        std::process::exit(0);
    });
    Json(serde_json::json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every translation key the editor uses must exist in lang/setup/en.yaml
    /// (the other languages are checked against en.yaml in i18n.rs).
    #[test]
    fn editor_keys_exist_in_english_strings() {
        let strings = i18n::setup_strings("en");
        let mut keys = Vec::new();
        for attr in ["data-i18n=\"", "data-i18n-html=\"", "data-i18n-title=\"", "data-i18n-aria=\""] {
            for part in EDITOR_HTML.split(attr).skip(1) {
                keys.push(part.split('"').next().unwrap_or_default().to_string());
            }
        }
        for part in EDITOR_HTML.split("T(\"").skip(1) {
            let key = part.split('"').next().unwrap_or_default();
            // `T("install.log." + key)` style prefixes are built at runtime
            if !key.ends_with('.') {
                keys.push(key.to_string());
            }
        }
        assert!(keys.len() > 100, "found only {} keys; did the markup change?", keys.len());
        let missing: Vec<&String> = keys.iter().filter(|k| !strings.contains_key(*k)).collect();
        assert!(missing.is_empty(), "editor uses keys missing from lang/setup/en.yaml: {:?}", missing);
    }

    #[test]
    fn every_card_type_has_a_label() {
        let strings = i18n::setup_strings("en");
        for t in ["weather", "light", "switch", "climate", "sensor", "sensor_grid", "media_player", "lock", "remote", "floorplan", "scenes_row", "scene"] {
            assert!(strings.contains_key(&format!("card_types.{}", t)), "card_types.{} missing", t);
        }
    }
}
