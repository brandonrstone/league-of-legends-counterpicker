use crate::models::PublicSettings;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub rank_bracket: String,
    pub owned_only: bool,
    pub comfort_weighting: bool,
    pub always_on_top: bool,
    pub role_override: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rank_bracket: "auto".into(),
            owned_only: true,
            comfort_weighting: true,
            always_on_top: true,
            role_override: "middle".into(),
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
        }
    }

    pub fn resolved_rank(&self, detected: Option<&str>) -> String {
        if self.rank_bracket != "auto" {
            return self.rank_bracket.clone();
        }
        match detected {
            Some(tier) => tier_to_bracket(tier),
            None => "emerald_plus".into(),
        }
    }
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

pub fn settings_path(app_data: &PathBuf) -> PathBuf {
    app_data.join("settings.json")
}

pub fn load_settings(path: &PathBuf) -> Settings {
    if let Ok(raw) = std::fs::read_to_string(path) {
        if let Ok(file) = serde_json::from_str::<Settings>(&raw) {
            return file;
        }
    }
    Settings::default()
}

pub fn save_settings(path: &PathBuf, settings: &Settings) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(settings)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_tiers_to_brackets() {
        assert_eq!(tier_to_bracket("diamond"), "diamond_plus");
        assert_eq!(tier_to_bracket("GOLD"), "gold_plus");
        assert_eq!(tier_to_bracket("iron"), "emerald_plus");
    }

    #[test]
    fn ignores_legacy_riot_fields_when_loading() {
        let raw = r#"{
            "rankBracket": "gold_plus",
            "ownedOnly": false,
            "comfortWeighting": false,
            "alwaysOnTop": false,
            "roleOverride": "top",
            "riotPlatform": "euw1",
            "riotApiKey": "RGAPI-secret"
        }"#;
        let loaded: Settings = serde_json::from_str(raw).unwrap();
        assert_eq!(loaded.rank_bracket, "gold_plus");
        assert_eq!(loaded.role_override, "top");
        assert!(!loaded.owned_only);
    }
}
