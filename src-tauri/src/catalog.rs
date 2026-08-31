use crate::models::ChampionInfo;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Clone, Default)]
pub struct Catalog {
    pub patch: String,
    #[allow(dead_code)]
    pub ddragon_version: String,
    pub by_id: HashMap<i64, ChampionInfo>,
    pub slug_by_id: HashMap<i64, String>,
}

#[derive(Deserialize)]
struct ChampionFile {
    data: HashMap<String, DdragonChampion>,
}

#[derive(Deserialize)]
struct DdragonChampion {
    id: String,
    key: String,
    name: String,
}

pub async fn load_catalog(client: &reqwest::Client) -> Result<Catalog> {
    let versions: Vec<String> = client
        .get("https://ddragon.leagueoflegends.com/api/versions.json")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .context("ddragon versions")?
        .error_for_status()?
        .json()
        .await?;
    let ddragon_version = versions
        .first()
        .cloned()
        .context("empty ddragon versions")?;
    let patch = ddragon_to_patch(&ddragon_version);
    let url = format!(
        "https://ddragon.leagueoflegends.com/cdn/{ddragon_version}/data/en_US/champion.json"
    );
    let file: ChampionFile = client
        .get(url)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("champion.json")?
        .error_for_status()?
        .json()
        .await?;

    let mut catalog = Catalog {
        patch,
        ddragon_version: ddragon_version.clone(),
        by_id: HashMap::new(),
        slug_by_id: HashMap::new(),
    };
    for champ in file.data.into_values() {
        let id: i64 = champ.key.parse().unwrap_or(0);
        if id <= 0 {
            continue;
        }
        let slug = lolalytics_slug(&champ.id, &champ.name);
        catalog.by_id.insert(
            id,
            ChampionInfo {
                id,
                key: champ.key,
                name: champ.name,
                slug: slug.clone(),
                icon_url: format!(
                    "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/v1/champion-icons/{id}.png"
                ),
            },
        );
        catalog.slug_by_id.insert(id, slug);
    }
    Ok(catalog)
}

pub fn ddragon_to_patch(version: &str) -> String {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[0], parts[1])
    } else {
        version.to_string()
    }
}

fn lolalytics_slug(dd_id: &str, name: &str) -> String {
    let lower = dd_id.to_ascii_lowercase();
    match lower.as_str() {
        "monkeyking" => "wukong".into(),
        "nunu" => "nunu".into(),
        _ => name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase(),
    }
}
