use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChampionInfo {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub slug: String,
    pub icon_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSlot {
    pub cell_id: i64,
    pub champion_id: i64,
    pub intent_id: i64,
    pub assigned_position: String,
    pub display_champion_id: i64,
    pub is_local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DraftView {
    pub role: String,
    pub pick_turn: i64,
    pub is_our_turn: bool,
    pub phase: String,
    pub seconds_left: i64,
    pub allies: Vec<PlayerSlot>,
    pub enemies: Vec<PlayerSlot>,
    pub bans: Vec<i64>,
    pub enemies_locked: usize,
    pub allies_locked: usize,
    pub lane_enemy_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recommendation {
    pub champion_id: i64,
    pub name: String,
    pub slug: String,
    pub icon_url: String,
    pub score: f64,
    pub reason: String,
    pub lane_delta: Option<f64>,
    pub team_delta: Option<f64>,
    pub synergy_delta: Option<f64>,
    pub meta_wr: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LcuStatus {
    pub connected: bool,
    pub summoner_name: Option<String>,
    pub game_name: Option<String>,
    pub detected_rank: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsStatus {
    pub ready: bool,
    pub ingesting: bool,
    pub stale: bool,
    pub patch: Option<String>,
    pub source: String,
    pub message: String,
    pub progress: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicSettings {
    pub rank_bracket: String,
    pub owned_only: bool,
    pub comfort_weighting: bool,
    pub always_on_top: bool,
    pub role_override: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdate {
    pub version: String,
    pub download_url: String,
    pub asset_name: String,
    pub status: String,
    pub progress: f64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub lcu: LcuStatus,
    pub game_phase: Option<String>,
    pub draft: Option<DraftView>,
    pub recommendations: Vec<Recommendation>,
    pub stats: StatsStatus,
    pub settings: PublicSettings,
    pub catalog_ready: bool,
    pub legal: String,
    pub update: Option<AppUpdate>,
}

impl Default for LcuStatus {
    fn default() -> Self {
        Self {
            connected: false,
            summoner_name: None,
            game_name: None,
            detected_rank: None,
        }
    }
}

impl Default for StatsStatus {
    fn default() -> Self {
        Self {
            ready: false,
            ingesting: false,
            stale: false,
            patch: None,
            source: "lolalytics".to_string(),
            message: "Waiting to load stats".to_string(),
            progress: 0.0,
        }
    }
}

pub fn legal_boilerplate() -> String {
    "Rift Counterpick is not endorsed by Riot Games and does not reflect the views or opinions of Riot Games or anyone officially involved in producing or managing Riot Games properties. Riot Games and all associated properties are trademarks or registered trademarks of Riot Games, Inc.".to_string()
}

pub fn normalize_role(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "top" | "top_lane" => "top".into(),
        "jungle" | "jg" | "jng" => "jungle".into(),
        "middle" | "mid" | "mid_lane" => "middle".into(),
        "bottom" | "bot" | "adc" | "bot_lane" => "bottom".into(),
        "utility" | "support" | "sup" | "supp" => "support".into(),
        other => other.to_string(),
    }
}

pub fn lcu_role_to_stats(role: &str) -> String {
    let n = normalize_role(role);
    if n == "utility" {
        "support".into()
    } else {
        n
    }
}
