use super::qwik;
use super::store::{MatchupStat, RoleMeta, StatsDb, STATS_SCHEMA};
use crate::catalog::Catalog;
use crate::models::normalize_role;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;

const ROLES: [&str; 5] = ["top", "jungle", "middle", "bottom", "support"];
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

pub struct IngestProgress {
    pub message: String,
    pub progress: f64,
}

pub async fn ingest_lolalytics(
    client: &reqwest::Client,
    db: &StatsDb,
    catalog: &Catalog,
    rank: &str,
    mut on_progress: impl FnMut(IngestProgress),
) -> Result<String> {
    let patch = catalog.patch.clone();
    db.begin_ingest(rank, &patch)?;

    on_progress(IngestProgress {
        message: format!("Fetching {patch} role tables"),
        progress: 0.02,
    });

    let mut role_champs: HashMap<String, Vec<i64>> = HashMap::new();
    for (i, role) in ROLES.iter().enumerate() {
        let list = fetch_list(client, &patch, role, rank).await?;
        let mut ids = Vec::new();
        if let Some(cid_map) = list.get("cid").and_then(|v| v.as_object()) {
            for (id_str, stats) in cid_map {
                let id: i64 = id_str.parse().unwrap_or(0);
                if id <= 0 {
                    continue;
                }
                let default_raw = str_field(stats, "defaultLane")
                    .or_else(|| str_field(stats, "default_lane"))
                    .unwrap_or_default();
                let default_lane = if default_raw.is_empty() {
                    (*role).to_string()
                } else {
                    normalize_role(&default_raw)
                };
                let meta = RoleMeta {
                    winrate: num_field(stats, "wr").unwrap_or(50.0),
                    pickrate: num_field(stats, "pr").unwrap_or(0.0),
                    banrate: num_field(stats, "br").unwrap_or(0.0),
                    games: num_field(stats, "games").unwrap_or(0.0) as i64,
                    pct_lane: num_field(stats, "pctLane")
                        .or_else(|| num_field(stats, "pct_lane"))
                        .unwrap_or(0.0),
                    default_lane,
                };
                if meta.games < 80 {
                    continue;
                }
                db.upsert_role_stat(id, role, rank, &patch, &meta)?;
                if meta.in_role_pool(role) {
                    ids.push((id, meta.games));
                }
            }
        }
        ids.sort_by(|a, b| b.1.cmp(&a.1));
        ids.truncate(40);
        let ids: Vec<i64> = ids.into_iter().map(|(id, _)| id).collect();
        let loaded = ids.len();
        role_champs.insert((*role).to_string(), ids);
        on_progress(IngestProgress {
            message: format!("Loaded {loaded} {role} champions"),
            progress: 0.05 + 0.05 * (i as f64 / ROLES.len() as f64),
        });
        sleep(Duration::from_millis(120)).await;
    }

    let jobs: Vec<(String, i64, String)> = role_champs
        .iter()
        .flat_map(|(role, ids)| {
            ids.iter().filter_map(|id| {
                catalog
                    .slug_by_id
                    .get(id)
                    .map(|slug| (role.clone(), *id, slug.clone()))
            })
        })
        .collect();

    let total = jobs.len().max(1);
    for (idx, (role, champ_id, slug)) in jobs.iter().enumerate() {
        let frac = 0.12 + 0.86 * (idx as f64 / total as f64);
        on_progress(IngestProgress {
            message: format!("Matchups {slug} {role} ({}/{})", idx + 1, total),
            progress: frac,
        });

        if let Ok(team) = fetch_team(client, &patch, slug, role, rank).await {
            if let Some(team_map) = team.get("team").and_then(|v| v.as_object()) {
                for (_lane, rows) in team_map {
                    store_rows(db, champ_id, role, "synergy", rank, &patch, rows, true)?;
                }
            }
        }

        if let Ok(html) = fetch_build_html(client, slug, role, rank).await {
            if let Some(tables) = qwik::parse_enemy_matchups(&html) {
                for (lane, row) in tables.all_rows() {
                    let kind = if lane == role.as_str() { "lane" } else { "team" };
                    db.upsert_matchup(
                        *champ_id,
                        row.champion_id,
                        role,
                        kind,
                        lane,
                        rank,
                        &patch,
                        &MatchupStat {
                            winrate: row.winrate,
                            games: row.games,
                            delta: row.delta,
                        },
                    )?;
                }
            }
        }

        sleep(Duration::from_millis(160)).await;
    }

    db.set_meta("patch", &patch)?;
    db.set_meta("rank", rank)?;
    db.set_meta("stats_schema", STATS_SCHEMA)?;
    db.set_meta(
        "ingested_at",
        &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )?;
    on_progress(IngestProgress {
        message: format!("Stats ready for patch {patch}"),
        progress: 1.0,
    });
    Ok(patch)
}

async fn fetch_list(client: &reqwest::Client, patch: &str, role: &str, rank: &str) -> Result<Value> {
    let url = format!(
        "https://a1.lolalytics.com/mega/?ep=list&v=1&patch={}&lane={}&tier={}&queue=ranked&region=all",
        urlencoding::encode(patch),
        urlencoding::encode(role),
        urlencoding::encode(rank)
    );
    get_json(client, &url).await
}

async fn fetch_team(
    client: &reqwest::Client,
    patch: &str,
    slug: &str,
    role: &str,
    rank: &str,
) -> Result<Value> {
    let url = format!(
        "https://a1.lolalytics.com/mega/?ep=build-team&v=1&patch={}&c={}&lane={}&tier={}&queue=ranked&region=all",
        urlencoding::encode(patch),
        urlencoding::encode(slug),
        urlencoding::encode(role),
        urlencoding::encode(rank)
    );
    get_json(client, &url).await
}

async fn fetch_build_html(
    client: &reqwest::Client,
    slug: &str,
    role: &str,
    rank: &str,
) -> Result<String> {
    let url = format!(
        "https://lolalytics.com/lol/{}/build/?lane={}&tier={}",
        urlencoding::encode(slug),
        urlencoding::encode(role),
        urlencoding::encode(rank)
    );
    let res = client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", "https://lolalytics.com/")
        .timeout(Duration::from_secs(25))
        .send()
        .await
        .context("build html")?;
    if !res.status().is_success() {
        anyhow::bail!("build html {}", res.status());
    }
    Ok(res.text().await?)
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value> {
    let res = client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", "https://lolalytics.com/")
        .header("Origin", "https://lolalytics.com")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("lolalytics json")?;
    if !res.status().is_success() {
        anyhow::bail!("lolalytics {} {}", url, res.status());
    }
    Ok(res.json().await?)
}

fn num_field(value: &Value, key: &str) -> Option<f64> {
    match value.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn store_rows(
    db: &StatsDb,
    champ_id: &i64,
    role: &str,
    mode: &str,
    rank: &str,
    patch: &str,
    rows: &Value,
    synergy: bool,
) -> Result<()> {
    let arr = match rows.as_array() {
        Some(a) => a,
        None => return Ok(()),
    };
    for row in arr {
        let cells = match row.as_array() {
            Some(c) if c.len() >= 6 => c,
            _ => continue,
        };
        let other_id = cells[0].as_i64().unwrap_or(0);
        if other_id <= 0 {
            continue;
        }
        let wr = cells[1].as_f64().unwrap_or(50.0);
        let delta = cells[2].as_f64().unwrap_or(wr - 50.0);
        let games = cells[5].as_i64().unwrap_or(0);
        let stat = MatchupStat {
            winrate: wr,
            games,
            delta,
        };
        if synergy {
            db.upsert_synergy(*champ_id, other_id, rank, patch, &stat)?;
        } else {
            db.upsert_matchup(*champ_id, other_id, role, mode, "", rank, patch, &stat)?;
        }
    }
    Ok(())
}

pub fn cache_is_fresh(db: &StatsDb, rank: &str, patch: &str) -> bool {
    if db.get_meta("stats_schema").as_deref() != Some(STATS_SCHEMA) {
        return false;
    }
    if !db.has_patch_data(rank, patch) {
        return false;
    }
    if db.get_meta("patch").as_deref() != Some(patch) {
        return false;
    }
    if db.get_meta("rank").as_deref() != Some(rank) {
        return false;
    }
    if let Some(ts) = db.get_meta("ingested_at") {
        if let Ok(then) = chrono::DateTime::parse_from_rfc3339(&ts) {
            let age = chrono::Utc::now().signed_duration_since(then.with_timezone(&chrono::Utc));
            return age.num_hours() < 20;
        }
    }
    true
}
