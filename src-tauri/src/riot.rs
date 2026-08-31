use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct RiotProfile {
    pub tier: Option<String>,
    pub mastery: HashMap<i64, (i64, i64)>,
}

#[derive(Deserialize)]
struct LeagueEntry {
    #[serde(rename = "queueType")]
    queue_type: Option<String>,
    tier: Option<String>,
}

#[derive(Deserialize)]
struct MasteryEntry {
    #[serde(rename = "championId")]
    champion_id: Option<i64>,
    #[serde(rename = "championLevel")]
    champion_level: Option<i64>,
    #[serde(rename = "championPoints")]
    champion_points: Option<i64>,
}

pub async fn fetch_profile(
    client: &reqwest::Client,
    api_key: &str,
    platform: &str,
    puuid: &str,
) -> Result<RiotProfile> {
    if api_key.is_empty() || puuid.is_empty() {
        anyhow::bail!("missing riot credentials");
    }
    let mut profile = RiotProfile::default();
    let league_url = format!(
        "https://{platform}.api.riotgames.com/lol/league/v4/entries/by-puuid/{puuid}"
    );
    if let Ok(res) = client
        .get(league_url)
        .header("X-Riot-Token", api_key)
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        if res.status().is_success() {
            if let Ok(entries) = res.json::<Vec<LeagueEntry>>().await {
                profile.tier = entries
                    .iter()
                    .find(|e| e.queue_type.as_deref() == Some("RANKED_SOLO_5x5"))
                    .or(entries.first())
                    .and_then(|e| e.tier.clone());
            }
        }
    }

    let mastery_url = format!(
        "https://{platform}.api.riotgames.com/lol/champion-mastery/v4/champion-masteries/by-puuid/{puuid}"
    );
    if let Ok(res) = client
        .get(mastery_url)
        .header("X-Riot-Token", api_key)
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        if res.status().is_success() {
            if let Ok(entries) = res.json::<Vec<MasteryEntry>>().await {
                for entry in entries {
                    if let (Some(id), Some(level), Some(points)) =
                        (entry.champion_id, entry.champion_level, entry.champion_points)
                    {
                        profile.mastery.insert(id, (level, points));
                    }
                }
            }
        }
    }
    Ok(profile)
}

pub fn tier_to_bracket(tier: &str) -> String {
    match tier.to_ascii_uppercase().as_str() {
        "CHALLENGER" | "GRANDMASTER" | "MASTER" | "DIAMOND" => "diamond_plus".into(),
        "EMERALD" => "emerald_plus".into(),
        "PLATINUM" => "platinum_plus".into(),
        "GOLD" => "gold_plus".into(),
        _ => "emerald_plus".into(),
    }
}
