pub mod lockfile;
pub mod types;

use crate::models::{lcu_role_to_stats, DraftView, PlayerSlot};
use anyhow::{Context, Result};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use http::header::{AUTHORIZATION, HeaderValue};
use lockfile::LockfileInfo;
use native_tls::TlsConnector;
use serde_json::Value;
use std::collections::HashSet;
use std::time::Duration;
use tokio_tungstenite::connect_async_tls_with_config;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::Connector;
use types::*;

#[derive(Clone)]
pub struct LcuHttp {
    client: reqwest::Client,
    port: u16,
    password: String,
}

impl LcuHttp {
    pub fn new(info: &LockfileInfo) -> Result<Self> {
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(4))
            .build()?;
        Ok(Self {
            client,
            port: info.port,
            password: info.password.clone(),
        })
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("https://127.0.0.1:{}{}", self.port, path);
        let res = self
            .client
            .get(url)
            .basic_auth("riot", Some(&self.password))
            .header("Accept", "application/json")
            .send()
            .await
            .context("lcu request")?;
        if !res.status().is_success() {
            anyhow::bail!("lcu {} {}", path, res.status());
        }
        Ok(res.json().await?)
    }

    pub async fn get_value(&self, path: &str) -> Result<Value> {
        self.get_json(path).await
    }

    pub async fn current_summoner(&self) -> Result<CurrentSummoner> {
        self.get_json("/lol-summoner/v1/current-summoner").await
    }

    pub async fn champ_select_session(&self) -> Result<ChampSelectSession> {
        self.get_json("/lol-champ-select/v1/session").await
    }

    pub async fn gameflow_phase(&self) -> Result<String> {
        match self.get_json::<String>("/lol-gameflow/v1/gameflow-phase").await {
            Ok(phase) => Ok(phase.trim_matches('"').to_string()),
            Err(_) => {
                let session: GameflowSession = self.get_json("/lol-gameflow/v1/session").await?;
                Ok(session.phase)
            }
        }
    }

    pub async fn pickable_champion_ids(&self) -> Result<HashSet<i64>> {
        if let Ok(ids) = self
            .get_json::<Vec<i64>>("/lol-champ-select/v1/pickable-champion-ids")
            .await
        {
            return Ok(ids.into_iter().filter(|id| *id > 0).collect());
        }
        let value: Value = self.get_value("/lol-champ-select/v1/pickable-champions").await?;
        let mut ids = HashSet::new();
        if let Some(arr) = value.get("championIds").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(id) = item.as_i64() {
                    if id > 0 {
                        ids.insert(id);
                    }
                }
            }
        } else if let Some(arr) = value.as_array() {
            for item in arr {
                if let Some(id) = item.as_i64().filter(|id| *id > 0) {
                    ids.insert(id);
                } else if let Some(id) = item
                    .get("championId")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_i64())
                    .filter(|id| *id > 0)
                {
                    ids.insert(id);
                }
            }
        }
        Ok(ids)
    }

    pub async fn owned_champion_ids(&self) -> Result<HashSet<i64>> {
        let owned: Vec<OwnedChampion> = self
            .get_json("/lol-champions/v1/owned-champions-minimal")
            .await?;
        Ok(owned
            .into_iter()
            .filter(|c| c.active || c.ownership.owned)
            .filter_map(|c| {
                let id = if c.id > 0 { c.id } else { c.champion_id };
                (id > 0).then_some(id)
            })
            .collect())
    }

    pub async fn current_queue_id(&self) -> Option<i64> {
        let session: GameflowSession = self.get_json("/lol-gameflow/v1/session").await.ok()?;
        (session.game_data.queue.id > 0).then_some(session.game_data.queue.id)
    }

    pub async fn ranked_tier(&self, queue_id: Option<i64>) -> Option<String> {
        let stats: RankedStats = self
            .get_json("/lol-ranked/v1/current-ranked-stats")
            .await
            .ok()?;
        tier_for_queue(&stats, queue_id)
    }

    pub async fn champion_masteries(&self) -> Result<Vec<ChampionMastery>> {
        if let Ok(list) = self
            .get_json::<Vec<ChampionMastery>>("/lol-collections/v1/inventories/champion-mastery")
            .await
        {
            return Ok(list);
        }
        let summoner = self.current_summoner().await?;
        self.get_json(&format!(
            "/lol-collections/v1/inventories/{}/champion-mastery",
            summoner.summoner_id
        ))
        .await
    }
}

pub const RANKED_SOLO_QUEUE: i64 = 420;
pub const RANKED_FLEX_QUEUE: i64 = 440;

fn queue_map_tier(stats: &RankedStats, key: &str) -> Option<String> {
    let tier = stats
        .queue_map
        .as_object()?
        .get(key)?
        .get("tier")?
        .as_str()?;
    (!tier.is_empty() && !tier.eq_ignore_ascii_case("NONE")).then(|| tier.to_string())
}

/// The rank to read stats at. A player who is Diamond in flex and Gold in solo
/// should not be shown Diamond numbers in a solo game, so the queue being played
/// wins over the highest rank held. Anything else (normals, unrated) falls back
/// to the best rank on the account.
pub fn tier_for_queue(stats: &RankedStats, queue_id: Option<i64>) -> Option<String> {
    let preferred = match queue_id {
        Some(RANKED_SOLO_QUEUE) => Some("RANKED_SOLO_5x5"),
        Some(RANKED_FLEX_QUEUE) => Some("RANKED_FLEX_SR"),
        _ => None,
    };
    if let Some(tier) = preferred.and_then(|key| queue_map_tier(stats, key)) {
        return Some(tier);
    }
    if let Some(entry) = stats.highest_ranked_entry.as_ref() {
        if !entry.tier.is_empty() && !entry.tier.eq_ignore_ascii_case("NONE") {
            return Some(entry.tier.clone());
        }
    }
    ["RANKED_SOLO_5x5", "RANKED_FLEX_SR"]
        .into_iter()
        .find_map(|key| queue_map_tier(stats, key))
}

pub async fn connect_ws(
    info: &LockfileInfo,
) -> Result<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
> {
    let auth = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("riot:{}", info.password))
    );
    let url = format!("wss://127.0.0.1:{}/", info.port);
    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert(AUTHORIZATION, HeaderValue::from_str(&auth)?);

    let tls = TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()?;
    let connector = Connector::NativeTls(tls);
    let (stream, _) = connect_async_tls_with_config(request, None, false, Some(connector)).await?;
    Ok(stream)
}

pub async fn subscribe_ws(
    stream: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Result<()> {
    for topic in [
        "OnJsonApiEvent_lol-champ-select_v1_session",
        "OnJsonApiEvent_lol-gameflow_v1_session",
        "OnJsonApiEvent_lol-gameflow_v1_gameflow-phase",
    ] {
        let payload = format!("[5,\"{topic}\"]");
        stream.send(Message::Text(payload.into())).await?;
    }
    Ok(())
}

pub fn parse_ws_event(text: &str) -> Option<(String, Value)> {
    let value: Value = serde_json::from_str(text).ok()?;
    let arr = value.as_array()?;
    if arr.len() < 3 {
        return None;
    }
    let payload = arr.get(2)?;
    let uri = payload
        .get("uri")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    Some((uri, data))
}

pub async fn ws_next(
    stream: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Result<Option<Message>> {
    match stream.next().await {
        Some(Ok(msg)) => Ok(Some(msg)),
        Some(Err(err)) => Err(err.into()),
        None => Ok(None),
    }
}

pub fn draft_from_session(session: &ChampSelectSession, role_override: &str) -> DraftView {
    let local_id = session.local_player_cell_id;
    let local = session
        .my_team
        .iter()
        .find(|p| p.cell_id == local_id);
    let assigned = local
        .map(|p| lcu_role_to_stats(&p.assigned_position))
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| lcu_role_to_stats(role_override));

    let allies = session
        .my_team
        .iter()
        .map(|p| slot_from_player(session, p, p.cell_id == local_id, true))
        .collect::<Vec<_>>();
    let enemies = session
        .their_team
        .iter()
        .map(|p| slot_from_player(session, p, false, false))
        .collect::<Vec<_>>();

    let mut bans = session.bans.my_team_bans.clone();
    bans.extend(session.bans.their_team_bans.iter().copied());
    for group in &session.actions {
        for action in group {
            if action.r#type == "ban" && action.completed && action.champion_id > 0 {
                bans.push(action.champion_id);
            }
        }
    }
    bans.retain(|id| *id > 0);
    bans.sort_unstable();
    bans.dedup();

    let mut is_our_turn = false;
    let mut pick_turn = 0i64;
    for group in &session.actions {
        for action in group {
            if action.r#type == "pick" {
                if action.pick_turn > 0 {
                    pick_turn = pick_turn.max(action.pick_turn);
                }
                if action.actor_cell_id == local_id && action.is_in_progress && !action.completed {
                    is_our_turn = true;
                    if action.pick_turn > 0 {
                        pick_turn = action.pick_turn;
                    }
                }
            }
        }
    }

    let enemies_locked = enemies.iter().filter(|e| e.champion_id > 0).count();
    let allies_locked = allies
        .iter()
        .filter(|a| !a.is_local && a.champion_id > 0)
        .count();
    let lane_enemy_id = enemies
        .iter()
        .find(|e| lcu_role_to_stats(&e.assigned_position) == assigned && e.champion_id > 0)
        .map(|e| e.champion_id);

    DraftView {
        role: if assigned.is_empty() {
            "middle".into()
        } else {
            assigned
        },
        pick_turn,
        is_our_turn,
        phase: session.timer.phase.clone(),
        seconds_left: if session.timer.adjusted_time_left_in_phase_in_sec > 0 {
            session.timer.adjusted_time_left_in_phase_in_sec
        } else {
            session.timer.time_left_in_phase_in_sec
        },
        allies,
        enemies,
        bans,
        enemies_locked,
        allies_locked,
        lane_enemy_id,
    }
}

fn action_champions(session: &ChampSelectSession, cell_id: i64) -> (i64, i64) {
    let mut locked = 0;
    let mut hover = 0;
    for group in &session.actions {
        for action in group {
            if action.actor_cell_id != cell_id
                || action.r#type != "pick"
                || action.champion_id <= 0
            {
                continue;
            }
            if action.completed {
                locked = action.champion_id;
            } else {
                hover = action.champion_id;
            }
        }
    }
    (locked, hover)
}

fn slot_from_player(
    session: &ChampSelectSession,
    player: &ChampSelectPlayer,
    is_local: bool,
    include_intent: bool,
) -> PlayerSlot {
    let (action_locked, action_hover) = action_champions(session, player.cell_id);
    let champion_id = if player.champion_id > 0 {
        player.champion_id
    } else {
        action_locked
    };
    let intent_id = if player.champion_pick_intent > 0 {
        player.champion_pick_intent
    } else {
        action_hover
    };
    let display = if champion_id > 0 {
        champion_id
    } else if include_intent {
        intent_id
    } else {
        0
    };
    PlayerSlot {
        cell_id: player.cell_id,
        champion_id,
        intent_id,
        assigned_position: lcu_role_to_stats(&player.assigned_position),
        display_champion_id: display,
        is_local,
    }
}

pub fn session_from_value(value: &Value) -> Option<ChampSelectSession> {
    serde_json::from_value(value.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::types::{ChampSelectAction, ChampSelectPlayer, ChampSelectSession};
    use super::*;

    #[test]
    fn draft_reads_completed_pick_actions_when_team_ids_are_empty() {
        let session = ChampSelectSession {
            local_player_cell_id: 1,
            my_team: vec![
                ChampSelectPlayer {
                    cell_id: 1,
                    assigned_position: "bottom".into(),
                    ..Default::default()
                },
                ChampSelectPlayer {
                    cell_id: 2,
                    assigned_position: "top".into(),
                    champion_pick_intent: 27,
                    ..Default::default()
                },
            ],
            their_team: vec![ChampSelectPlayer {
                cell_id: 5,
                assigned_position: String::new(),
                ..Default::default()
            }],
            actions: vec![vec![ChampSelectAction {
                actor_cell_id: 5,
                champion_id: 51,
                completed: true,
                r#type: "pick".into(),
                pick_turn: 1,
                ..Default::default()
            }]],
            ..Default::default()
        };
        let draft = draft_from_session(&session, "middle");
        assert_eq!(draft.role, "bottom");
        assert_eq!(draft.enemies[0].champion_id, 51);
        assert_eq!(draft.enemies[0].display_champion_id, 51);
        assert_eq!(draft.allies[1].display_champion_id, 27);
        assert_eq!(draft.enemies_locked, 1);
    }

    fn split_ranks() -> RankedStats {
        RankedStats {
            queue_map: serde_json::json!({
                "RANKED_SOLO_5x5": { "tier": "GOLD" },
                "RANKED_FLEX_SR": { "tier": "DIAMOND" },
            }),
            highest_ranked_entry: Some(super::types::RankedEntry {
                tier: "DIAMOND".into(),
                division: "II".into(),
                queue_type: "RANKED_FLEX_SR".into(),
            }),
        }
    }

    #[test]
    fn queue_decides_which_rank_the_stats_come_from() {
        let stats = split_ranks();
        assert_eq!(
            tier_for_queue(&stats, Some(RANKED_SOLO_QUEUE)).as_deref(),
            Some("GOLD"),
            "a solo game should not be scored on a flex Diamond rank"
        );
        assert_eq!(
            tier_for_queue(&stats, Some(RANKED_FLEX_QUEUE)).as_deref(),
            Some("DIAMOND")
        );
    }

    #[test]
    fn unranked_queues_fall_back_to_the_best_rank_held() {
        let stats = split_ranks();
        assert_eq!(
            tier_for_queue(&stats, None).as_deref(),
            Some("DIAMOND"),
            "normals have no rank of their own"
        );
    }

    #[test]
    fn unplayed_queue_falls_through_instead_of_reporting_nothing() {
        let stats = RankedStats {
            queue_map: serde_json::json!({
                "RANKED_SOLO_5x5": { "tier": "NONE" },
                "RANKED_FLEX_SR": { "tier": "PLATINUM" },
            }),
            highest_ranked_entry: None,
        };
        assert_eq!(
            tier_for_queue(&stats, Some(RANKED_SOLO_QUEUE)).as_deref(),
            Some("PLATINUM"),
            "placements in solo should still borrow the flex rank"
        );
    }

    #[test]
    fn gameflow_session_exposes_the_queue() {
        let session: super::types::GameflowSession = serde_json::from_value(serde_json::json!({
            "phase": "ChampSelect",
            "gameData": { "queue": { "id": 420 } },
        }))
        .unwrap();
        assert_eq!(session.phase, "ChampSelect");
        assert_eq!(session.game_data.queue.id, RANKED_SOLO_QUEUE);
    }
}
