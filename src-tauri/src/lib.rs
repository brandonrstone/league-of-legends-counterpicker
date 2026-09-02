mod catalog;
mod engine;
mod lcu;
mod models;
mod settings;
mod stats;
mod update;

use catalog::Catalog;
use engine::ScoreContext;
use lcu::lockfile::{discover_lockfile, LockfileInfo};
use lcu::types::ChampSelectSession;
use lcu::LcuHttp;
use models::{legal_boilerplate, AppSnapshot, AppUpdate, DraftView, LcuStatus, Recommendation, StatsStatus};
use settings::{load_settings, save_settings, settings_path, Settings};
use stats::{cache_is_fresh, ingest_lolalytics, StatsDb};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
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
    recommendations: Vec<Recommendation>,
    stats: StatsStatus,
    update: Option<AppUpdate>,
    current_version: String,
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
            update: self.update.clone(),
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

#[tauri::command]
async fn download_update(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let (url, dest) = {
        let mut inner = state.inner.lock().await;
        let update = inner
            .update
            .as_mut()
            .ok_or_else(|| "no update available".to_string())?;
        if update.status == "downloading" {
            return Ok(());
        }
        update.status = "downloading".into();
        update.progress = 0.0;
        update.message = format!("Downloading {}…", update.version);
        let url = update.download_url.clone();
        let name = update.asset_name.clone();
        let snap = inner.snapshot();
        drop(inner);
        emit_snapshot(&app, &snap);
        let dir = app
            .path()
            .download_dir()
            .or_else(|_| app.path().app_data_dir())
            .map_err(|e| e.to_string())?;
        (url, dir.join(name))
    };

    let result = update::download_installer(&state.http, &url, &dest).await;
    match result {
        Ok(()) => {
            let mut inner = state.inner.lock().await;
            if let Some(update) = inner.update.as_mut() {
                update.status = "ready".into();
                update.progress = 1.0;
                update.message = "Installer downloaded — opening…".into();
            }
            let snap = inner.snapshot();
            drop(inner);
            emit_snapshot(&app, &snap);
            if let Err(err) = app.opener().open_path(dest.to_string_lossy().as_ref(), None::<&str>) {
                let mut inner = state.inner.lock().await;
                if let Some(update) = inner.update.as_mut() {
                    update.status = "ready".into();
                    update.message = format!(
                        "Saved to Downloads as {}. Open it to install.",
                        dest.file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("the installer")
                    );
                }
                let snap = inner.snapshot();
                drop(inner);
                emit_snapshot(&app, &snap);
                let _ = err;
            }
            Ok(())
        }
        Err(err) => {
            let mut inner = state.inner.lock().await;
            if let Some(update) = inner.update.as_mut() {
                update.status = "error".into();
                update.message = "Download failed — click to retry".into();
            }
            let snap = inner.snapshot();
            drop(inner);
            emit_snapshot(&app, &snap);
            Err(err.to_string())
        }
    }
}

#[tauri::command]
async fn dismiss_update(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<AppSnapshot, String> {
    let mut inner = state.inner.lock().await;
    if let Some(update) = inner.update.take() {
        inner.settings.dismissed_update = Some(update.version);
        let _ = save_settings(&state.settings_file, &inner.settings);
    }
    let snap = inner.snapshot();
    drop(inner);
    let _ = app.emit("snapshot", &snap);
    Ok(snap)
}

async fn check_for_update(app: &AppHandle, state: &Arc<AppState>) {
    let (current, dismissed, busy) = {
        let inner = state.inner.lock().await;
        let busy = inner
            .update
            .as_ref()
            .is_some_and(|u| u.status == "downloading");
        (
            inner.current_version.clone(),
            inner.settings.dismissed_update.clone(),
            busy,
        )
    };
    if busy {
        return;
    }
    let Ok(release) = update::fetch_latest_release(&state.http).await else {
        return;
    };
    let Some(offer) = update::offer_from_release(&release, &current) else {
        let mut inner = state.inner.lock().await;
        if inner
            .update
            .as_ref()
            .is_some_and(|u| u.status != "downloading")
        {
            inner.update = None;
            let snap = inner.snapshot();
            drop(inner);
            emit_snapshot(app, &snap);
        }
        return;
    };
    if dismissed.as_deref() == Some(offer.version.as_str()) {
        return;
    }
    let mut inner = state.inner.lock().await;
    if inner
        .update
        .as_ref()
        .is_some_and(|u| u.status == "downloading")
    {
        return;
    }
    inner.update = Some(offer);
    let snap = inner.snapshot();
    drop(inner);
    emit_snapshot(app, &snap);
}

async fn update_loop(app: AppHandle, state: Arc<AppState>) {
    loop {
        check_for_update(&app, &state).await;
        tokio::time::sleep(Duration::from_secs(4 * 60 * 60)).await;
    }
}

fn emit_snapshot(app: &AppHandle, snap: &AppSnapshot) {
    let _ = app.emit("snapshot", snap);
}

fn is_champ_select(phase: Option<&str>) -> bool {
    phase.is_some_and(|p| p.eq_ignore_ascii_case("ChampSelect"))
}

fn show_for_champ_select(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

fn hide_after_champ_select(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
}

fn on_tray_menu(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id.as_ref() {
        "show" => show_for_champ_select(app),
        "quit" => app.exit(0),
        _ => {}
    }
}

fn on_tray_icon(tray: &tauri::tray::TrayIcon, event: TrayIconEvent) {
    if let TrayIconEvent::DoubleClick {
        button: MouseButton::Left,
        ..
    } = event
    {
        show_for_champ_select(tray.app_handle());
    }
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_i = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

    if let Some(tray) = app.tray_by_id("main-tray") {
        tray.set_menu(Some(menu))?;
        tray.on_menu_event(on_tray_menu);
        tray.on_tray_icon_event(on_tray_icon);
    } else {
        let mut builder = TrayIconBuilder::with_id("main-tray")
            .menu(&menu)
            .show_menu_on_left_click(false)
            .tooltip("Rift Counterpick")
            .on_menu_event(on_tray_menu)
            .on_tray_icon_event(on_tray_icon);
        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }
        builder.build(app)?;
    }
    Ok(())
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
        inner.stats.message = "Downloading champion matchup data…".into();
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
                        hide_after_champ_select(&app);
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
    let had_draft = inner.draft.is_some();
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
    }
    inner.lcu.detected_rank = ranked;
    if let Some(p) = phase.clone() {
        inner.game_phase = Some(p);
    }
    inner.owned = owned;
    if !mastery.is_empty() {
        inner.mastery = mastery;
    }
    if let Some(session) = session.as_ref() {
        inner.draft = Some(lcu::draft_from_session(
            session,
            &inner.settings.role_override,
        ));
        if !pickable.is_empty() {
            let taken: HashSet<i64> = inner
                .draft
                .as_ref()
                .map(|d| {
                    d.allies
                        .iter()
                        .filter_map(|p| {
                            if p.is_local {
                                (p.champion_id > 0).then_some(p.champion_id)
                            } else if p.display_champion_id > 0 {
                                Some(p.display_champion_id)
                            } else {
                                None
                            }
                        })
                        .chain(
                            d.enemies
                                .iter()
                                .filter_map(|p| (p.champion_id > 0).then_some(p.champion_id)),
                        )
                        .collect()
                })
                .unwrap_or_default();
            if pickable.iter().any(|id| !taken.contains(id)) {
                inner.pickable = pickable;
            }
        }
        rescore(&state.db, &mut inner);
    } else if phase
        .as_deref()
        .is_some_and(|p| !p.eq_ignore_ascii_case("ChampSelect"))
    {
        inner.pickable.clear();
        inner.draft = None;
        inner.recommendations.clear();
    }
    let snap = inner.snapshot();
    let enter_select = (!is_champ_select(previous_phase.as_deref())
        && is_champ_select(inner.game_phase.as_deref()))
        || (!had_draft && inner.draft.is_some());
    let leave_select = is_champ_select(previous_phase.as_deref())
        && phase
            .as_deref()
            .is_some_and(|p| !p.eq_ignore_ascii_case("ChampSelect"));
    drop(inner);
    emit_snapshot(app, &snap);
    if enter_select {
        show_for_champ_select(app);
    } else if leave_select {
        hide_after_champ_select(app);
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
                            let enter_select = !is_champ_select(inner.game_phase.as_deref())
                                && inner.draft.is_none();
                            inner.game_phase = Some("ChampSelect".into());
                            inner.draft = Some(lcu::draft_from_session(
                                &session,
                                &inner.settings.role_override,
                            ));
                            rescore(&state.db, &mut inner);
                            let snap = inner.snapshot();
                            drop(inner);
                            emit_snapshot(&app, &snap);
                            if enter_select {
                                show_for_champ_select(&app);
                            }
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
                                hide_after_champ_select(&app);
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
                            let enter_select = !is_champ_select(previous_phase.as_deref())
                                && is_champ_select(Some(phase.as_str()));
                            let leave_select = is_champ_select(previous_phase.as_deref())
                                && !is_champ_select(Some(phase.as_str()));
                            drop(inner);
                            emit_snapshot(&app, &snap);
                            if enter_select {
                                show_for_champ_select(&app);
                            } else if leave_select {
                                hide_after_champ_select(&app);
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
            setup_tray(app)?;
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
                    recommendations: Vec::new(),
                    stats: StatsStatus::default(),
                    update: None,
                    current_version: app.package_info().version.to_string(),
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
                update_loop(handle, s3).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            update_settings,
            refresh_stats,
            download_update,
            dismiss_update
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
