use crate::models::PublicSettings;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub rank_bracket: String,
    pub owned_only: bool,
    pub comfort_weighting: bool,
    pub always_on_top: bool,
    pub role_override: String,
    pub riot_platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub riot_api_key: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rank_bracket: "auto".into(),
            owned_only: true,
            comfort_weighting: true,
            always_on_top: true,
            role_override: "middle".into(),
            riot_platform: std::env::var("RIOT_PLATFORM").unwrap_or_else(|_| "na1".into()),
            riot_api_key: std::env::var("RIOT_API_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }
}

impl Settings {
    pub fn public(&self) -> PublicSettings {
        PublicSettings {
            rank_bracket: self.rank_bracket.clone(),
            owned_only: self.owned_only,
            comfort_weighting: self.comfort_weighting,
            always_on_top: self.always_on_top,
            role_override: self.role_override.clone(),
            riot_platform: self.riot_platform.clone(),
            has_riot_key: self
                .riot_api_key
                .as_ref()
                .map(|k| !k.is_empty())
                .unwrap_or(false),
        }
    }

    pub fn resolved_rank(&self, detected: Option<&str>) -> String {
        if self.rank_bracket != "auto" {
            return self.rank_bracket.clone();
        }
        match detected {
            Some(tier) => crate::riot::tier_to_bracket(tier),
            None => "emerald_plus".into(),
        }
    }
}

pub fn settings_path(app_data: &PathBuf) -> PathBuf {
    app_data.join("settings.json")
}

pub fn load_settings(path: &PathBuf) -> Settings {
    let mut settings = Settings::default();
    if let Ok(raw) = std::fs::read_to_string(path) {
        if let Ok(file) = serde_json::from_str::<Settings>(&raw) {
            settings = file;
        }
    }
    if settings.riot_api_key.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
        if let Ok(key) = std::env::var("RIOT_API_KEY") {
            if !key.is_empty() {
                settings.riot_api_key = Some(key);
            }
        }
    }
    settings
}

pub fn save_settings(path: &PathBuf, settings: &Settings) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(settings)?)?;
    Ok(())
}
