use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChampSelectSession {
    #[serde(default)]
    pub actions: Vec<Vec<ChampSelectAction>>,
    #[serde(default)]
    pub bans: ChampSelectBans,
    #[serde(default)]
    pub local_player_cell_id: i64,
    #[serde(default)]
    pub my_team: Vec<ChampSelectPlayer>,
    #[serde(default)]
    pub their_team: Vec<ChampSelectPlayer>,
    #[serde(default)]
    pub timer: ChampSelectTimer,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct ChampSelectAction {
    #[serde(default)]
    pub actor_cell_id: i64,
    #[serde(default)]
    pub champion_id: i64,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub is_in_progress: bool,
    #[serde(default)]
    pub pick_turn: i64,
    #[serde(default)]
    pub is_ally_action: bool,
    #[serde(default)]
    pub r#type: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChampSelectBans {
    #[serde(default)]
    pub my_team_bans: Vec<i64>,
    #[serde(default)]
    pub their_team_bans: Vec<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct ChampSelectPlayer {
    #[serde(default)]
    pub assigned_position: String,
    #[serde(default)]
    pub cell_id: i64,
    #[serde(default)]
    pub champion_id: i64,
    #[serde(default)]
    pub champion_pick_intent: i64,
    #[serde(default)]
    pub summoner_id: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChampSelectTimer {
    #[serde(default)]
    pub adjusted_time_left_in_phase_in_sec: i64,
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub time_left_in_phase_in_sec: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CurrentSummoner {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub game_name: String,
    #[serde(default)]
    pub tag_line: String,
    #[serde(default)]
    pub summoner_id: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OwnedChampion {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub champion_id: i64,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub ownership: ChampionOwnership,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChampionOwnership {
    #[serde(default)]
    pub owned: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RankedStats {
    #[serde(default)]
    pub queue_map: serde_json::Value,
    #[serde(default)]
    pub highest_ranked_entry: Option<RankedEntry>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct RankedEntry {
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub division: String,
    #[serde(default)]
    pub queue_type: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChampionMastery {
    #[serde(default)]
    pub champion_id: i64,
    #[serde(default)]
    pub champion_level: i64,
    #[serde(default)]
    pub champion_points: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GameflowSession {
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub game_data: GameflowData,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GameflowData {
    #[serde(default)]
    pub queue: GameflowQueue,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GameflowQueue {
    #[serde(default)]
    pub id: i64,
}
