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
    let tier = lolalytics_tier(rank);
    db.begin_ingest(rank, &patch)?;

    on_progress(IngestProgress {
        message: format!("Fetching {patch} role tables"),
        progress: 0.02,
    });

    let mut role_champs: HashMap<String, Vec<i64>> = HashMap::new();
    for (i, role) in ROLES.iter().enumerate() {
        let list = fetch_list(client, &patch, role, &tier).await?;
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

        if let Ok(team) = fetch_team(client, &patch, slug, role, &tier).await {
            if let Some(team_map) = team.get("team").and_then(|v| v.as_object()) {
                for (_lane, rows) in team_map {
                    store_rows(db, champ_id, role, "synergy", rank, &patch, rows, true)?;
                }
            }
        }

        let mut stored_matchups = 0usize;
        for vs_role in ROLES {
            if let Ok(payload) = fetch_counters(client, slug, role, vs_role, &tier, &patch).await {
                stored_matchups += store_counter_json(db, champ_id, role, vs_role, rank, &patch, &payload)?;
            }
        }
        if stored_matchups == 0 {
            if let Ok(html) = fetch_build_html(client, slug, role, &tier).await {
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

fn lolalytics_tier(rank: &str) -> String {
    match rank {
        "emerald" => "emerald_plus".into(),
        "diamond" => "diamond_plus".into(),
        "platinum" => "platinum_plus".into(),
        "gold" => "gold_plus".into(),
        other => other.to_string(),
    }
}

async fn fetch_list(client: &reqwest::Client, patch: &str, role: &str, rank: &str) -> Result<Value> {
    let with_patch = format!(
        "https://a1.lolalytics.com/mega/?ep=list&v=1&patch={}&lane={}&tier={}&queue=ranked&region=all",
        urlencoding::encode(patch),
        urlencoding::encode(role),
        urlencoding::encode(rank)
    );
    let without_patch = format!(
        "https://a1.lolalytics.com/mega/?ep=list&v=1&lane={}&tier={}&queue=ranked&region=all",
        urlencoding::encode(role),
        urlencoding::encode(rank)
    );
    if let Ok(value) = get_json(client, &with_patch).await {
        if value.get("cid").is_some() {
            return Ok(value);
        }
    }
    get_json(client, &without_patch).await
}

async fn fetch_team(
    client: &reqwest::Client,
    patch: &str,
    slug: &str,
    role: &str,
    rank: &str,
) -> Result<Value> {
    let with_patch = format!(
        "https://a1.lolalytics.com/mega/?ep=build-team&v=1&patch={}&c={}&lane={}&tier={}&queue=ranked&region=all",
        urlencoding::encode(patch),
        urlencoding::encode(slug),
        urlencoding::encode(role),
        urlencoding::encode(rank)
    );
    let without_patch = format!(
        "https://a1.lolalytics.com/mega/?ep=build-team&v=1&c={}&lane={}&tier={}&queue=ranked&region=all",
        urlencoding::encode(slug),
        urlencoding::encode(role),
        urlencoding::encode(rank)
    );
    if let Ok(value) = get_json(client, &with_patch).await {
        if value.get("team").is_some() {
            return Ok(value);
        }
    }
    get_json(client, &without_patch).await
}

async fn fetch_counters(
    client: &reqwest::Client,
    slug: &str,
    role: &str,
    vs_role: &str,
    rank: &str,
    patch: &str,
) -> Result<Value> {
    let with_patch = format!(
        "https://a1.lolalytics.com/mega/?ep=counter&v=1&c={}&lane={}&vslane={}&tier={}&queue=ranked&region=all&patch={}",
        urlencoding::encode(slug),
        urlencoding::encode(role),
        urlencoding::encode(vs_role),
        urlencoding::encode(rank),
        urlencoding::encode(patch)
    );
    let without_patch = format!(
        "https://a1.lolalytics.com/mega/?ep=counter&v=1&c={}&lane={}&vslane={}&tier={}&queue=ranked&region=all",
        urlencoding::encode(slug),
        urlencoding::encode(role),
        urlencoding::encode(vs_role),
        urlencoding::encode(rank)
    );
    if let Ok(value) = get_json(client, &with_patch).await {
        if !counter_rows(&value).is_empty() {
            return Ok(value);
        }
    }
    get_json(client, &without_patch).await
}

fn counter_rows(payload: &Value) -> Vec<&Value> {
    let Some(counters) = payload.get("counters") else {
        return Vec::new();
    };
    if let Some(arr) = counters.as_array() {
        return arr.iter().collect();
    }
    if let Some(obj) = counters.as_object() {
        let mut out = Vec::new();
        for value in obj.values() {
            if let Some(arr) = value.as_array() {
                out.extend(arr.iter());
            }
        }
        return out;
    }
    Vec::new()
}

fn store_counter_json(
    db: &StatsDb,
    champ_id: &i64,
    role: &str,
    vs_role: &str,
    rank: &str,
    patch: &str,
    payload: &Value,
) -> Result<usize> {
    let rows = counter_rows(payload);
    if rows.is_empty() {
        return Ok(0);
    }
    let kind = if vs_role == role { "lane" } else { "team" };
    let mut stored = 0usize;
    for row in rows {
        let enemy_id = num_field(row, "cid").unwrap_or(0.0) as i64;
        if enemy_id <= 0 {
            continue;
        }
        let wr = num_field(row, "vsWr").unwrap_or(50.0);
        let games = num_field(row, "n").unwrap_or(0.0) as i64;
        let delta = num_field(row, "d1").unwrap_or(wr - 50.0);
        if games < 40 {
            continue;
        }
        db.upsert_matchup(
            *champ_id,
            enemy_id,
            role,
            kind,
            vs_role,
            rank,
            patch,
            &MatchupStat {
                winrate: wr,
                games,
                delta,
            },
        )?;
        stored += 1;
    }
    Ok(stored)
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
    if !db.has_matchup_data(rank, patch) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::store::StatsDb;
    use serde_json::json;

    #[test]
    fn store_counter_json_reads_vs_wr_rows() {
        let db = StatsDb::open_memory().unwrap();
        let payload = json!({
            "counters": [
                {"cid": 222, "vsWr": 55.2, "n": 8000, "d1": 5.2},
                {"cid": 51, "vsWr": 47.0, "n": 10, "d1": -3.0}
            ]
        });
        let stored =
            store_counter_json(&db, &29, "bottom", "bottom", "emerald", "15.1", &payload).unwrap();
        assert_eq!(stored, 1, "rows with n < 40 should be skipped");
        let mu = db
            .matchup(29, 222, "bottom", "bottom", "emerald", "15.1")
            .expect("jinx matchup");
        assert!((mu.winrate - 55.2).abs() < 0.01);
        assert_eq!(mu.games, 8000);
    }

    #[test]
    fn lolalytics_tier_maps_settings_brackets() {
        assert_eq!(lolalytics_tier("emerald"), "emerald_plus");
        assert_eq!(lolalytics_tier("diamond"), "diamond_plus");
        assert_eq!(lolalytics_tier("platinum_plus"), "platinum_plus");
    }
}
