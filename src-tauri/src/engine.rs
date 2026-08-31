use crate::catalog::Catalog;
use crate::models::{DraftView, Recommendation};
use crate::stats::store::{MatchupStat, StatsDb};
use std::collections::{HashMap, HashSet};

const PRIOR_N: f64 = 1000.0;
const PRIOR_WR: f64 = 50.0;

#[derive(Clone, Debug)]
pub struct ScoreContext {
    pub rank: String,
    pub patch: String,
    pub owned_only: bool,
    pub comfort_weighting: bool,
    pub pickable: HashSet<i64>,
    pub owned: HashSet<i64>,
    pub mastery: HashMap<i64, (i64, i64)>,
}

struct Weights {
    lane: f64,
    team: f64,
    syn: f64,
    meta: f64,
    flex: f64,
    comfort: f64,
}

struct Scored {
    rec: Recommendation,
    games: i64,
}

pub fn recommend(
    db: &StatsDb,
    catalog: &Catalog,
    draft: &DraftView,
    ctx: &ScoreContext,
) -> Vec<Recommendation> {
    let role = draft.role.as_str();
    let banned: HashSet<i64> = draft.bans.iter().copied().filter(|id| *id > 0).collect();
    let taken: HashSet<i64> = draft
        .allies
        .iter()
        .filter(|p| !p.is_local && p.display_champion_id > 0)
        .map(|p| p.display_champion_id)
        .chain(
            draft
                .enemies
                .iter()
                .filter_map(|p| (p.champion_id > 0).then_some(p.champion_id)),
        )
        .collect();

    let locked_enemies: Vec<(i64, String)> = draft
        .enemies
        .iter()
        .filter(|p| p.champion_id > 0)
        .map(|p| {
            (
                p.champion_id,
                resolve_role(db, p.champion_id, &p.assigned_position, ctx),
            )
        })
        .collect();
    let locked_allies: Vec<i64> = draft
        .allies
        .iter()
        .filter(|p| !p.is_local && p.display_champion_id > 0)
        .map(|p| p.display_champion_id)
        .collect();

    let lane_enemy_id = draft.lane_enemy_id.or_else(|| {
        locked_enemies
            .iter()
            .find(|(_, vs_role)| vs_role == role)
            .map(|(id, _)| *id)
    });

    let weights = weights(lane_enemy_id.is_some(), locked_enemies.len());
    let mut scored = Vec::new();

    let mut candidates: HashSet<i64> = db
        .champions_in_role(role, &ctx.rank, &ctx.patch)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    if !ctx.pickable.is_empty() {
        let filtered: HashSet<i64> = candidates
            .iter()
            .copied()
            .filter(|id| ctx.pickable.contains(id))
            .collect();
        if !filtered.is_empty() {
            candidates = filtered;
        }
    }
    if ctx.owned_only && !ctx.owned.is_empty() {
        let filtered: HashSet<i64> = candidates
            .iter()
            .copied()
            .filter(|id| ctx.owned.contains(id))
            .collect();
        if !filtered.is_empty() {
            candidates = filtered;
        }
    }

    for champ_id in candidates {
        if champ_id <= 0 || banned.contains(&champ_id) || taken.contains(&champ_id) {
            continue;
        }
        let Some(info) = catalog.by_id.get(&champ_id) else {
            continue;
        };
        let Some(meta) = db.role_meta(champ_id, role, &ctx.rank, &ctx.patch) else {
            continue;
        };

        let lane_stat = lane_enemy_id
            .and_then(|enemy| db.matchup(champ_id, enemy, role, role, &ctx.rank, &ctx.patch));
        let lane_delta = lane_stat.as_ref().map(shrunk_delta);

        let mut team_weight_sum = 0.0;
        let mut team_delta_sum = 0.0;
        for (enemy_id, vs_role) in &locked_enemies {
            if lane_enemy_id == Some(*enemy_id) {
                continue;
            }
            if let Some(stat) = db.matchup(champ_id, *enemy_id, role, vs_role, &ctx.rank, &ctx.patch)
            {
                let w = vs_role_weight(role, vs_role);
                team_delta_sum += w * shrunk_delta(&stat);
                team_weight_sum += w;
            }
        }
        let team_delta = if team_weight_sum > 0.0 {
            Some(team_delta_sum / team_weight_sum)
        } else {
            None
        };

        let mut syn_deltas = Vec::new();
        for ally in &locked_allies {
            if let Some(stat) = db.synergy(champ_id, *ally, &ctx.rank, &ctx.patch) {
                syn_deltas.push(shrunk_delta(&stat));
            }
        }
        let synergy_delta = mean(&syn_deltas);

        let flex = db
            .flexibility(champ_id, role, &ctx.rank, &ctx.patch)
            .map(|avg| shrink(avg, 5000) - 50.0)
            .unwrap_or(0.0);
        let meta_wr = meta.winrate;
        let meta_delta = meta_wr - 50.0;
        let comfort = if ctx.comfort_weighting {
            comfort_score(ctx.mastery.get(&champ_id).copied()).min(0.15)
        } else {
            0.0
        };

        let score = weights.lane * lane_delta.unwrap_or(0.0)
            + weights.team * team_delta.unwrap_or(0.0)
            + weights.syn * synergy_delta.unwrap_or(0.0)
            + weights.meta * meta_delta
            + weights.flex * flex
            + weights.comfort * comfort;

        let reason = build_reason(
            catalog,
            lane_delta,
            lane_enemy_id,
            team_delta,
            synergy_delta,
            locked_allies.first().copied(),
            meta_wr,
            locked_enemies.len(),
        );

        scored.push(Scored {
            rec: Recommendation {
                champion_id: champ_id,
                name: info.name.clone(),
                slug: info.slug.clone(),
                icon_url: info.icon_url.clone(),
                score,
                reason,
                lane_delta,
                team_delta,
                synergy_delta,
                meta_wr: Some(meta_wr),
            },
            games: meta.games,
        });
    }

    scored.sort_by(|a, b| {
        b.rec
            .score
            .partial_cmp(&a.rec.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.games.cmp(&a.games))
            .then(a.rec.champion_id.cmp(&b.rec.champion_id))
    });
    scored.truncate(5);
    scored.into_iter().map(|s| s.rec).collect()
}

fn resolve_role(db: &StatsDb, champion_id: i64, assigned: &str, ctx: &ScoreContext) -> String {
    if !assigned.is_empty() {
        return assigned.to_string();
    }
    db.primary_role(champion_id, &ctx.rank, &ctx.patch)
        .unwrap_or_default()
}

fn weights(lane_known: bool, enemies_known: usize) -> Weights {
    if lane_known {
        Weights {
            lane: 0.50,
            team: 0.22,
            syn: 0.10,
            meta: 0.15,
            flex: 0.03,
            comfort: 0.00,
        }
    } else if enemies_known > 0 {
        Weights {
            lane: 0.0,
            team: 0.40,
            syn: 0.20,
            meta: 0.25,
            flex: 0.10,
            comfort: 0.05,
        }
    } else {
        Weights {
            lane: 0.0,
            team: 0.10,
            syn: 0.15,
            meta: 0.45,
            flex: 0.25,
            comfort: 0.05,
        }
    }
}

fn vs_role_weight(our_role: &str, vs_role: &str) -> f64 {
    if vs_role.is_empty() {
        0.15
    } else if our_role == vs_role {
        1.0
    } else if is_duo_lane(our_role, vs_role) {
        0.45
    } else {
        0.15
    }
}

fn is_duo_lane(a: &str, b: &str) -> bool {
    (a == "bottom" && b == "support") || (a == "support" && b == "bottom")
}

pub fn shrink(wr: f64, games: i64) -> f64 {
    let g = games.max(0) as f64;
    (g * wr + PRIOR_N * PRIOR_WR) / (g + PRIOR_N)
}

fn shrunk_delta(stat: &MatchupStat) -> f64 {
    shrink(stat.winrate, stat.games) - 50.0
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

fn comfort_score(mastery: Option<(i64, i64)>) -> f64 {
    let Some((level, points)) = mastery else {
        return 0.0;
    };
    let level_part = (level as f64 / 7.0) * 4.0;
    let points_part = ((points as f64 + 1.0).ln() / 12.0).min(2.0);
    level_part + points_part
}

fn build_reason(
    catalog: &Catalog,
    lane_delta: Option<f64>,
    lane_enemy: Option<i64>,
    team_delta: Option<f64>,
    synergy_delta: Option<f64>,
    ally: Option<i64>,
    meta_wr: f64,
    enemies_locked: usize,
) -> String {
    let mut parts = Vec::new();
    if let (Some(delta), Some(enemy_id)) = (lane_delta, lane_enemy) {
        if let Some(enemy) = catalog.by_id.get(&enemy_id) {
            parts.push(format!("{:+.1}% vs {}", delta, enemy.name));
        }
    }
    if let Some(delta) = team_delta {
        if enemies_locked > 1 {
            parts.push(format!("{:+.1}% vs locked enemies", delta));
        }
    }
    if let (Some(delta), Some(ally_id)) = (synergy_delta, ally) {
        if let Some(ally_info) = catalog.by_id.get(&ally_id) {
            if delta >= 0.4 {
                parts.push(format!("synergizes with {}", ally_info.name));
            }
        }
    }
    if parts.is_empty() {
        if enemies_locked == 0 {
            parts.push(format!(
                "safe {meta_wr:.1}% WR pick while the draft is still open"
            ));
        } else {
            parts.push(format!("{meta_wr:.1}% role win rate this patch"));
        }
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::models::{ChampionInfo, DraftView, PlayerSlot};
    use crate::stats::store::{MatchupStat, RoleMeta, StatsDb};
    use std::collections::{HashMap, HashSet};

    const RANK: &str = "emerald";
    const PATCH: &str = "15.1";
    const JINX: i64 = 222;
    const CAITLYN: i64 = 51;
    const SINGED: i64 = 27;
    const MISS_FORTUNE: i64 = 21;

    fn champ(id: i64, name: &str) -> ChampionInfo {
        ChampionInfo {
            id,
            key: id.to_string(),
            name: name.to_string(),
            slug: name.to_ascii_lowercase().replace(' ', ""),
            icon_url: String::new(),
        }
    }

    fn test_catalog() -> Catalog {
        let champs = [
            champ(JINX, "Jinx"),
            champ(CAITLYN, "Caitlyn"),
            champ(SINGED, "Singed"),
            champ(MISS_FORTUNE, "Miss Fortune"),
        ];
        let mut catalog = Catalog {
            patch: PATCH.to_string(),
            ..Default::default()
        };
        for info in champs {
            catalog.slug_by_id.insert(info.id, info.slug.clone());
            catalog.by_id.insert(info.id, info);
        }
        catalog
    }

    fn bot_meta(winrate: f64, games: i64, pct_lane: f64) -> RoleMeta {
        RoleMeta {
            winrate,
            pickrate: 10.0,
            banrate: 1.0,
            games,
            pct_lane,
            default_lane: "bottom".into(),
        }
    }

    fn off_role_meta() -> RoleMeta {
        RoleMeta {
            winrate: 54.0,
            pickrate: 5.0,
            banrate: 2.0,
            games: 8_000,
            pct_lane: 1.0,
            default_lane: "top".into(),
        }
    }

    fn seed_adc_pool(db: &StatsDb) {
        db.upsert_role_stat(JINX, "bottom", RANK, PATCH, &bot_meta(51.0, 12_000, 96.0))
            .unwrap();
        db.upsert_role_stat(
            CAITLYN,
            "bottom",
            RANK,
            PATCH,
            &bot_meta(50.5, 11_000, 98.0),
        )
        .unwrap();
        db.upsert_role_stat(
            MISS_FORTUNE,
            "bottom",
            RANK,
            PATCH,
            &bot_meta(50.2, 10_000, 90.0),
        )
        .unwrap();
        db.upsert_role_stat(SINGED, "bottom", RANK, PATCH, &off_role_meta())
            .unwrap();
        db.upsert_role_stat(SINGED, "top", RANK, PATCH, &off_role_meta())
            .unwrap();
    }

    fn ctx_with_pickable(ids: &[i64]) -> ScoreContext {
        ScoreContext {
            rank: RANK.into(),
            patch: PATCH.into(),
            owned_only: false,
            comfort_weighting: true,
            pickable: ids.iter().copied().collect(),
            owned: HashSet::new(),
            mastery: HashMap::new(),
        }
    }

    fn empty_draft() -> DraftView {
        DraftView {
            role: "bottom".into(),
            ..Default::default()
        }
    }

    fn enemy_adc(id: i64) -> DraftView {
        DraftView {
            role: "bottom".into(),
            enemies: vec![PlayerSlot {
                champion_id: id,
                assigned_position: "bottom".into(),
                display_champion_id: id,
                ..Default::default()
            }],
            enemies_locked: 1,
            lane_enemy_id: Some(id),
            ..Default::default()
        }
    }

    #[test]
    fn shrinkage_pulls_small_samples_toward_fifty() {
        let shrunk = shrink(80.0, 40);
        assert!(shrunk < 55.0);
        assert!(shrunk > 50.0);
        let stable = shrink(54.0, 20_000);
        assert!((stable - 54.0).abs() < 0.3);
    }

    #[test]
    fn early_draft_weights_meta_and_flex() {
        let w = weights(false, 0);
        assert!(w.meta > w.lane);
        assert!(w.flex > w.lane);
        let mid = weights(false, 2);
        assert!(mid.team > mid.meta);
        let late = weights(true, 1);
        assert!(late.lane > late.meta);
        assert_eq!(late.comfort, 0.0);
    }

    #[test]
    fn adc_pickable_set_drops_off_role_champs() {
        let db = StatsDb::open_memory().unwrap();
        seed_adc_pool(&db);
        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[SINGED, JINX, CAITLYN, MISS_FORTUNE]);
        let recs = recommend(&db, &catalog, &empty_draft(), &ctx);
        assert!(!recs.is_empty(), "expected bot-lane recommendations");
        assert!(
            recs.iter().all(|r| r.champion_id != SINGED),
            "Singed must not appear in ADC recs: {:?}",
            recs.iter().map(|r| &r.name).collect::<Vec<_>>()
        );
        let ids: HashSet<i64> = recs.iter().map(|r| r.champion_id).collect();
        assert!(ids.contains(&JINX) || ids.contains(&CAITLYN) || ids.contains(&MISS_FORTUNE));
    }

    #[test]
    fn lane_counter_outranks_high_mastery_weaker_matchup() {
        let db = StatsDb::open_memory().unwrap();
        seed_adc_pool(&db);
        db.upsert_matchup(
            JINX,
            CAITLYN,
            "bottom",
            "lane",
            "bottom",
            RANK,
            PATCH,
            &MatchupStat {
                winrate: 58.0,
                games: 8_000,
                delta: 8.0,
            },
        )
        .unwrap();
        db.upsert_matchup(
            MISS_FORTUNE,
            CAITLYN,
            "bottom",
            "lane",
            "bottom",
            RANK,
            PATCH,
            &MatchupStat {
                winrate: 44.0,
                games: 8_000,
                delta: -6.0,
            },
        )
        .unwrap();

        let catalog = test_catalog();
        let mut ctx = ctx_with_pickable(&[SINGED, JINX, MISS_FORTUNE]);
        ctx.mastery.insert(MISS_FORTUNE, (7, 1_000_000));
        ctx.mastery.insert(JINX, (1, 1_200));

        let recs = recommend(&db, &catalog, &enemy_adc(CAITLYN), &ctx);
        assert!(
            recs.iter().all(|r| r.champion_id != SINGED),
            "off-role Singed should still be excluded"
        );
        let jinx_idx = recs.iter().position(|r| r.champion_id == JINX);
        let mf_idx = recs.iter().position(|r| r.champion_id == MISS_FORTUNE);
        assert!(jinx_idx.is_some(), "Jinx should be recommended: {recs:?}");
        if let (Some(j), Some(m)) = (jinx_idx, mf_idx) {
            assert!(
                j < m,
                "lane counter Jinx should outrank high-mastery Miss Fortune"
            );
        }
    }

    #[test]
    fn equal_scores_keep_stable_order() {
        let db = StatsDb::open_memory().unwrap();
        let same = bot_meta(50.0, 5_000, 80.0);
        db.upsert_role_stat(CAITLYN, "bottom", RANK, PATCH, &same)
            .unwrap();
        db.upsert_role_stat(JINX, "bottom", RANK, PATCH, &same)
            .unwrap();
        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[CAITLYN, JINX]);
        let draft = empty_draft();
        let first = recommend(&db, &catalog, &draft, &ctx);
        let second = recommend(&db, &catalog, &draft, &ctx);
        assert_eq!(first.len(), 2);
        assert_eq!(
            first.iter().map(|r| r.champion_id).collect::<Vec<_>>(),
            second.iter().map(|r| r.champion_id).collect::<Vec<_>>()
        );
        assert_eq!(first[0].champion_id, CAITLYN);
        assert_eq!(first[1].champion_id, JINX);
    }

    #[test]
    fn duo_lane_outweighs_offlane_matchup() {
        assert_eq!(vs_role_weight("bottom", "bottom"), 1.0);
        assert_eq!(vs_role_weight("bottom", "support"), 0.45);
        assert_eq!(vs_role_weight("support", "bottom"), 0.45);
        assert_eq!(vs_role_weight("bottom", "top"), 0.15);
        assert_eq!(vs_role_weight("bottom", ""), 0.15);
    }

    #[test]
    fn disjoint_pickable_set_still_recommends_role_champs() {
        let db = StatsDb::open_memory().unwrap();
        seed_adc_pool(&db);
        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[SINGED]);
        let recs = recommend(&db, &catalog, &empty_draft(), &ctx);
        assert!(
            !recs.is_empty(),
            "role pool should survive a pickable set with no in-role champs"
        );
        assert!(recs.iter().all(|r| r.champion_id != SINGED));
    }

    #[test]
    fn second_pick_scores_locked_enemy_without_assigned_role() {
        let db = StatsDb::open_memory().unwrap();
        seed_adc_pool(&db);
        db.upsert_matchup(
            JINX,
            CAITLYN,
            "bottom",
            "lane",
            "bottom",
            RANK,
            PATCH,
            &MatchupStat {
                winrate: 58.0,
                games: 8_000,
                delta: 8.0,
            },
        )
        .unwrap();
        db.upsert_matchup(
            MISS_FORTUNE,
            CAITLYN,
            "bottom",
            "lane",
            "bottom",
            RANK,
            PATCH,
            &MatchupStat {
                winrate: 44.0,
                games: 8_000,
                delta: -6.0,
            },
        )
        .unwrap();

        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[JINX, CAITLYN, MISS_FORTUNE, SINGED]);
        let draft = DraftView {
            role: "bottom".into(),
            allies: vec![
                PlayerSlot {
                    cell_id: 1,
                    is_local: true,
                    ..Default::default()
                },
                PlayerSlot {
                    cell_id: 2,
                    intent_id: SINGED,
                    display_champion_id: SINGED,
                    assigned_position: "top".into(),
                    ..Default::default()
                },
            ],
            enemies: vec![PlayerSlot {
                champion_id: CAITLYN,
                display_champion_id: CAITLYN,
                assigned_position: String::new(),
                ..Default::default()
            }],
            enemies_locked: 1,
            allies_locked: 0,
            lane_enemy_id: None,
            ..Default::default()
        };

        let recs = recommend(&db, &catalog, &draft, &ctx);
        assert!(!recs.is_empty(), "second pick should still get recommendations");
        assert!(
            recs.iter().all(|r| r.champion_id != SINGED && r.champion_id != CAITLYN),
            "taken/off-role champs must stay out: {recs:?}"
        );
        let jinx_idx = recs.iter().position(|r| r.champion_id == JINX);
        let mf_idx = recs.iter().position(|r| r.champion_id == MISS_FORTUNE);
        assert!(jinx_idx.is_some(), "Jinx should counter inferred Caitlyn: {recs:?}");
        if let (Some(j), Some(m)) = (jinx_idx, mf_idx) {
            assert!(j < m, "inferred lane counter should outrank the losing matchup");
        }
        assert!(
            recs[0].reason.contains("Caitlyn") || recs[0].lane_delta.is_some(),
            "reason should reflect the already-selected enemy: {}",
            recs[0].reason
        );
    }

    #[test]
    fn ally_hovers_are_not_recommended() {
        let db = StatsDb::open_memory().unwrap();
        seed_adc_pool(&db);
        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[JINX, CAITLYN, MISS_FORTUNE]);
        let draft = DraftView {
            role: "bottom".into(),
            allies: vec![PlayerSlot {
                is_local: false,
                intent_id: JINX,
                display_champion_id: JINX,
                assigned_position: "bottom".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let recs = recommend(&db, &catalog, &draft, &ctx);
        assert!(!recs.is_empty());
        assert!(
            recs.iter().all(|r| r.champion_id != JINX),
            "hovered ally Jinx should be treated as taken: {recs:?}"
        );
    }
}
