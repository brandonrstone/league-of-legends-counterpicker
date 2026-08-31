mod catalog;
mod engine;
mod lcu;
mod models;
mod riot;
mod settings;
mod stats;

use catalog::Catalog;
use engine::ScoreContext;
use lcu::lockfile::{discover_lockfile, LockfileInfo};
use lcu::types::ChampSelectSession;
use lcu::LcuHttp;
use models::{legal_boilerplate, AppSnapshot, DraftView, LcuStatus, Recommendation, StatsStatus};
use settings::{load_settings, save_settings, settings_path, Settings};
use stats::{cache_is_fresh, ingest_lolalytics, StatsDb};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

pub struct AppState {
    inner: Mutex<InnerState>,
    db: StatsDb,
    http: reqwest::Client,
    settings_file: PathBuf,
}

struct InnerState {
    settings: Settings,
    catalog: Option<Catalog>,
    lcu: LcuStatus,
    game_phase: Option<String>,
    draft: Option<DraftView>,
    pickable: HashSet<i64>,
    owned: HashSet<i64>,
    mastery: HashMap<i64, (i64, i64)>,
    puuid: Option<String>,
    recommendations: Vec<Recommendation>,
    stats: StatsStatus,
}

impl InnerState {
    fn snapshot(&self) -> AppSnapshot {
        AppSnapshot {
            lcu: self.lcu.clone(),
            game_phase: self.game_phase.clone(),
            draft: self.draft.clone(),
            recommendations: self.recommendations.clone(),
            stats: self.stats.clone(),
            settings: self.settings.public(),
            catalog_ready: self.catalog.is_some(),
            legal: legal_boilerplate(),
        }
    }
}

#[tauri::command]
async fn get_snapshot(state: State<'_, Arc<AppState>>) -> Result<AppSnapshot, String> {
    let inner = state.inner.lock().await;
    Ok(inner.snapshot())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsPatch {
    rank_bracket: Option<String>,
    owned_only: Option<bool>,
    comfort_weighting: Option<bool>,
    always_on_top: Option<bool>,
    role_override: Option<String>,
    riot_platform: Option<String>,
    riot_api_key: Option<String>,
}

#[tauri::command]
async fn update_settings(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    patch: SettingsPatch,
) -> Result<AppSnapshot, String> {
    let mut inner = state.inner.lock().await;
    if let Some(v) = patch.rank_bracket {
        inner.settings.rank_bracket = v;
    }
    if let Some(v) = patch.owned_only {
        inner.settings.owned_only = v;
    }
    if let Some(v) = patch.comfort_weighting {
        inner.settings.comfort_weighting = v;
    }
    if let Some(v) = patch.always_on_top {
        inner.settings.always_on_top = v;
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.set_always_on_top(v);
        }
    }
    if let Some(v) = patch.role_override {
        inner.settings.role_override = v;
    }
    if let Some(v) = patch.riot_platform {
        inner.settings.riot_platform = v;
    }
    if let Some(v) = patch.riot_api_key {
        inner.settings.riot_api_key = if v.is_empty() { None } else { Some(v) };
    }
    let _ = save_settings(&state.settings_file, &inner.settings);
    rescore(&state.db, &mut inner);
    let snap = inner.snapshot();
    drop(inner);
    let _ = app.emit("snapshot", &snap);
    Ok(snap)
}

#[tauri::command]
async fn refresh_stats(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let handle = app.clone();
    let state = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        run_ingest(handle, state, true).await;
    });
    Ok(())
}

fn emit_snapshot(app: &AppHandle, snap: &AppSnapshot) {
    let _ = app.emit("snapshot", snap);
}

fn is_champ_select(phase: Option<&str>) -> bool {
    phase.is_some_and(|p| p.eq_ignore_ascii_case("ChampSelect"))
}

fn exit_after_champ_select(app: &AppHandle) {
    app.exit(0);
}

fn rescore(db: &StatsDb, inner: &mut InnerState) {
    let Some(catalog) = inner.catalog.as_ref() else {
        inner.recommendations.clear();
        return;
    };
    let Some(draft) = inner.draft.as_ref() else {
        inner.recommendations.clear();
        return;
    };
    let rank = inner
        .settings
        .resolved_rank(inner.lcu.detected_rank.as_deref());
    let ctx = ScoreContext {
        rank,
        patch: catalog.patch.clone(),
        owned_only: inner.settings.owned_only,
        comfort_weighting: inner.settings.comfort_weighting,
        pickable: inner.pickable.clone(),
        owned: inner.owned.clone(),
        mastery: inner.mastery.clone(),
    };
    inner.recommendations = engine::recommend(db, catalog, draft, &ctx);
}

async fn run_ingest(app: AppHandle, state: Arc<AppState>, force: bool) {
    let (rank, patch, already_fresh, matchups) = {
        let inner = state.inner.lock().await;
        let Some(catalog) = inner.catalog.as_ref() else {
            return;
        };
        let rank = inner
            .settings
            .resolved_rank(inner.lcu.detected_rank.as_deref());
        let fresh = cache_is_fresh(&state.db, &rank, &catalog.patch);
        let matchups = state.db.has_matchup_data(&rank, &catalog.patch);
        (rank, catalog.patch.clone(), fresh, matchups)
    };
    if already_fresh && !force {
        let mut inner = state.inner.lock().await;
        inner.stats = StatsStatus {
            ready: true,
            ingesting: false,
            stale: false,
            patch: Some(patch),
            source: "lolalytics".into(),
            message: if matchups {
                "Stats cache is current".into()
            } else {
                "Role stats cached; matchup tables missing — try Refresh stats".into()
            },
            progress: 1.0,
        };
        let snap = inner.snapshot();
        drop(inner);
        emit_snapshot(&app, &snap);
        return;
    }

    {
        let mut inner = state.inner.lock().await;
        inner.stats.ingesting = true;
        inner.stats.message = "Downloading champion matchup data".into();
        inner.stats.progress = 0.01;
        let snap = inner.snapshot();
        drop(inner);
        emit_snapshot(&app, &snap);
    }

    let catalog = {
        let inner = state.inner.lock().await;
        inner.catalog.clone().unwrap()
    };
    let db_result = ingest_lolalytics(&state.http, &state.db, &catalog, &rank, |p| {
        if let Ok(mut inner) = state.inner.try_lock() {
            inner.stats.ingesting = true;
            inner.stats.message = p.message;
            inner.stats.progress = p.progress;
            inner.stats.patch = Some(catalog.patch.clone());
            let snap = inner.snapshot();
            drop(inner);
            emit_snapshot(&app, &snap);
        }
    })
    .await;

    let mut inner = state.inner.lock().await;
    match db_result {
        Ok(p) => {
            let matchups_ok = state.db.has_matchup_data(&rank, &p);
            inner.stats = StatsStatus {
                ready: true,
                ingesting: false,
                stale: false,
                patch: Some(p),
                source: "lolalytics".into(),
                message: if matchups_ok {
                    "Live stats ready".into()
                } else {
                    "Role stats ready; matchup tables incomplete — try Refresh stats".into()
                },
                progress: 1.0,
            };
        }
        Err(err) => {
            let roles = state.db.has_patch_data(&rank, &patch) || state.db.has_any_role_data();
            let matchups_ok = state.db.has_matchup_data(&rank, &patch);
            inner.stats = StatsStatus {
                ready: roles,
                ingesting: false,
                stale: roles,
                patch: if roles { Some(patch) } else { None },
                source: "lolalytics".into(),
                message: if roles && matchups_ok {
                    "Using cached stats; refresh failed".into()
                } else if roles {
                    "Using cached role stats; matchup tables missing — refresh failed".into()
                } else {
                    format!("Could not refresh stats: {err}")
                },
                progress: if roles { 1.0 } else { 0.0 },
            };
        }
    }
    rescore(&state.db, &mut inner);
    let snap = inner.snapshot();
    drop(inner);
    emit_snapshot(&app, &snap);
}

async fn lcu_loop(app: AppHandle, state: Arc<AppState>) {
    let mut last_lock: Option<LockfileInfo> = None;
    let mut http: Option<LcuHttp> = None;
    loop {
        match discover_lockfile() {
            None => {
                last_lock = None;
                http = None;
                let mut inner = state.inner.lock().await;
                if inner.lcu.connected {
                    let leave_select =
                        is_champ_select(inner.game_phase.as_deref()) || inner.draft.is_some();
                    inner.lcu = LcuStatus::default();
                    inner.game_phase = None;
                    inner.draft = None;
                    inner.recommendations.clear();
                    inner.pickable.clear();
                    let snap = inner.snapshot();
                    drop(inner);
                    emit_snapshot(&app, &snap);
                    if leave_select {
                        exit_after_champ_select(&app);
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(1500)).await;
            }
            Some(info) => {
                let reconnect = last_lock.as_ref() != Some(&info);
                if reconnect {
                    last_lock = Some(info.clone());
                    match LcuHttp::new(&info) {
                        Ok(client) => {
                            http = Some(client.clone());
                            refresh_from_lcu(&app, &state, &client).await;
                            let app2 = app.clone();
                            let state2 = state.clone();
                            let info2 = info.clone();
                            tauri::async_runtime::spawn(async move {
                                ws_loop(app2, state2, info2).await;
                            });
                        }
                        Err(_) => {
                            http = None;
                        }
                    }
                } else if let Some(client) = http.as_ref() {
                    refresh_from_lcu(&app, &state, client).await;
                }
                let in_select = {
                    let inner = state.inner.lock().await;
                    inner
                        .game_phase
                        .as_deref()
                        .is_some_and(|p| p.eq_ignore_ascii_case("ChampSelect"))
                };
                tokio::time::sleep(Duration::from_millis(if in_select { 400 } else { 1600 })).await;
            }
        }
    }
}

async fn refresh_from_lcu(app: &AppHandle, state: &Arc<AppState>, client: &LcuHttp) {
    let summoner = client.current_summoner().await.ok();
    let phase = client.gameflow_phase().await.ok();
    let ranked = client.ranked_tier().await;
    let owned = client.owned_champion_ids().await.unwrap_or_default();
    let masteries = client.champion_masteries().await.unwrap_or_default();
    let mut mastery = HashMap::new();
    for m in masteries {
        mastery.insert(m.champion_id, (m.champion_level, m.champion_points));
    }

    let mut session: Option<ChampSelectSession> = None;
    let mut pickable = HashSet::new();
    if phase
        .as_deref()
        .is_some_and(|p| p.eq_ignore_ascii_case("ChampSelect"))
    {
        session = client.champ_select_session().await.ok();
        pickable = client.pickable_champion_ids().await.unwrap_or_default();
    }

    let mut inner = state.inner.lock().await;
    let previous_phase = inner.game_phase.clone();
    inner.lcu.connected = summoner.is_some();
    if let Some(s) = summoner {
        inner.lcu.summoner_name = Some(if s.display_name.is_empty() {
            s.game_name.clone()
        } else {
            s.display_name.clone()
        });
        inner.lcu.game_name = Some(if s.tag_line.is_empty() {
            s.game_name
        } else {
            format!("{}#{}", s.game_name, s.tag_line)
        });
        inner.puuid = if s.puuid.is_empty() {
            None
        } else {
            Some(s.puuid)
        };
    }
    inner.lcu.detected_rank = ranked;
    if let Some(p) = phase.clone() {
        inner.game_phase = Some(p);
    }
    inner.owned = owned;
    if !mastery.is_empty() {
        inner.mastery = mastery;
    }
    if !pickable.is_empty() {
        inner.pickable = pickable;
    } else if phase
        .as_deref()
        .is_some_and(|p| !p.eq_ignore_ascii_case("ChampSelect"))
    {
        inner.pickable.clear();
    }
    if let Some(session) = session.as_ref() {
        inner.draft = Some(lcu::draft_from_session(
            session,
            &inner.settings.role_override,
        ));
        rescore(&state.db, &mut inner);
    } else if phase
        .as_deref()
        .is_some_and(|p| !p.eq_ignore_ascii_case("ChampSelect"))
    {
        inner.draft = None;
        inner.recommendations.clear();
    }
    let snap = inner.snapshot();
    let leave_select = is_champ_select(previous_phase.as_deref())
        && phase
            .as_deref()
            .is_some_and(|p| !p.eq_ignore_ascii_case("ChampSelect"));
    drop(inner);
    emit_snapshot(app, &snap);
    if leave_select {
        exit_after_champ_select(app);
    }
}

async fn ws_loop(app: AppHandle, state: Arc<AppState>, info: LockfileInfo) {
    let Ok(mut stream) = lcu::connect_ws(&info).await else {
        return;
    };
    if lcu::subscribe_ws(&mut stream).await.is_err() {
        return;
    }
    loop {
        match lcu::ws_next(&mut stream).await {
            Ok(Some(Message::Text(text))) => {
                if let Some((uri, data)) = lcu::parse_ws_event(&text) {
                    if uri.contains("champ-select") {
                        if let Some(session) = lcu::session_from_value(&data) {
                            let mut inner = state.inner.lock().await;
                            inner.game_phase = Some("ChampSelect".into());
                            inner.draft = Some(lcu::draft_from_session(
                                &session,
                                &inner.settings.role_override,
                            ));
                            rescore(&state.db, &mut inner);
                            let snap = inner.snapshot();
                            drop(inner);
                            emit_snapshot(&app, &snap);
                        } else if data.is_null() {
                            let mut inner = state.inner.lock().await;
                            let leave_select = is_champ_select(inner.game_phase.as_deref())
                                || inner.draft.is_some();
                            inner.draft = None;
                            inner.recommendations.clear();
                            if is_champ_select(inner.game_phase.as_deref()) {
                                inner.game_phase = Some("None".into());
                            }
                            let snap = inner.snapshot();
                            drop(inner);
                            emit_snapshot(&app, &snap);
                            if leave_select {
                                exit_after_champ_select(&app);
                                return;
                            }
                        }
                    } else if uri.contains("gameflow") {
                        let phase = data.as_str().map(|s| s.to_string()).or_else(|| {
                            data.get("phase")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        });
                        if let Some(phase) = phase {
                            let mut inner = state.inner.lock().await;
                            let previous_phase = inner.game_phase.clone();
                            inner.game_phase = Some(phase.clone());
                            if !phase.eq_ignore_ascii_case("ChampSelect") {
                                inner.draft = None;
                                inner.recommendations.clear();
                            }
                            let snap = inner.snapshot();
                            let leave_select = is_champ_select(previous_phase.as_deref())
                                && !is_champ_select(Some(phase.as_str()));
                            drop(inner);
                            emit_snapshot(&app, &snap);
                            if leave_select {
                                exit_after_champ_select(&app);
                                return;
                            }
                        }
                    }
                }
            }
            Ok(Some(Message::Ping(p))) => {
                let _ = futures_util::SinkExt::send(&mut stream, Message::Pong(p)).await;
            }
            Ok(Some(Message::Close(_))) | Ok(None) => break,
            Err(_) => break,
            _ => {}
        }
    }
}

async fn bootstrap(app: AppHandle, state: Arc<AppState>) {
    {
        let mut inner = state.inner.lock().await;
        inner.stats.message = "Loading champion catalog".into();
        let snap = inner.snapshot();
        drop(inner);
        emit_snapshot(&app, &snap);
    }
    match catalog::load_catalog(&state.http).await {
        Ok(cat) => {
            let mut inner = state.inner.lock().await;
            inner.catalog = Some(cat);
            inner.stats.message = "Champion catalog ready".into();
            let snap = inner.snapshot();
            drop(inner);
            emit_snapshot(&app, &snap);
            run_ingest(app.clone(), state.clone(), false).await;
        }
        Err(err) => {
            let mut inner = state.inner.lock().await;
            inner.stats.message = format!("Could not load champion catalog: {err}");
            let snap = inner.snapshot();
            drop(inner);
            emit_snapshot(&app, &snap);
        }
    }
}

async fn optional_riot_refresh(app: AppHandle, state: Arc<AppState>) {
    loop {
        tokio::time::sleep(Duration::from_secs(45)).await;
        let (key, platform, puuid, comfort) = {
            let inner = state.inner.lock().await;
            (
                inner.settings.riot_api_key.clone().unwrap_or_default(),
                inner.settings.riot_platform.clone(),
                inner.puuid.clone().unwrap_or_default(),
                inner.settings.comfort_weighting,
            )
        };
        if key.is_empty() || puuid.is_empty() || !comfort {
            continue;
        }
        if let Ok(profile) = riot::fetch_profile(&state.http, &key, &platform, &puuid).await {
            let mut inner = state.inner.lock().await;
            if let Some(tier) = profile.tier {
                inner.lcu.detected_rank = Some(tier);
            }
            if !profile.mastery.is_empty() {
                inner.mastery = profile.mastery;
            }
            rescore(&state.db, &mut inner);
            let snap = inner.snapshot();
            drop(inner);
            emit_snapshot(&app, &snap);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = dotenvy::from_filename(".env");
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let _ = dotenvy::from_path(dir.join(".env"));
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_data = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."));
            std::fs::create_dir_all(&app_data)?;
            let settings_file = settings_path(&app_data);
            let settings = load_settings(&settings_file);
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_always_on_top(settings.always_on_top);
            }
            let db = StatsDb::open(&app_data.join("stats.sqlite"))?;
            let http = reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .build()?;
            let state = Arc::new(AppState {
                inner: Mutex::new(InnerState {
                    settings,
                    catalog: None,
                    lcu: LcuStatus::default(),
                    game_phase: None,
                    draft: None,
                    pickable: HashSet::new(),
                    owned: HashSet::new(),
                    mastery: HashMap::new(),
                    puuid: None,
                    recommendations: Vec::new(),
                    stats: StatsStatus::default(),
                }),
                db,
                http,
                settings_file,
            });
            app.manage(state.clone());
            let handle = app.handle().clone();
            let s1 = state.clone();
            tauri::async_runtime::spawn(async move {
                bootstrap(handle, s1).await;
            });
            let handle = app.handle().clone();
            let s2 = state.clone();
            tauri::async_runtime::spawn(async move {
                lcu_loop(handle, s2).await;
            });
            let handle = app.handle().clone();
            let s3 = state.clone();
            tauri::async_runtime::spawn(async move {
                optional_riot_refresh(handle, s3).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            update_settings,
            refresh_stats
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
