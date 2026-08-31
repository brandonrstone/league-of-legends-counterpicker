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
    let role_owned = crate::models::lcu_role_to_stats(&draft.role);
    let role = role_owned.as_str();
    let banned: HashSet<i64> = draft.bans.iter().copied().filter(|id| *id > 0).collect();
    let taken: HashSet<i64> = draft
        .allies
        .iter()
        .filter_map(|p| {
            if p.is_local {
                (p.champion_id > 0).then_some(p.champion_id)
            } else if p.display_champion_id > 0 {
                Some(p.display_champion_id)
            } else {
                None
            }
        })
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
    let locked_allies: Vec<(i64, String)> = draft
        .allies
        .iter()
        .filter(|p| !p.is_local && p.display_champion_id > 0)
        .map(|p| {
            (
                p.display_champion_id,
                resolve_role(db, p.display_champion_id, &p.assigned_position, ctx),
            )
        })
        .collect();

    let lane_enemy_id = draft.lane_enemy_id.or_else(|| {
        locked_enemies
            .iter()
            .find(|(_, vs_role)| vs_role == role)
            .map(|(id, _)| *id)
    });

    let weights = weights(lane_enemy_id.is_some(), locked_enemies.len());
    let mut scored = Vec::new();

    let pool: HashSet<i64> = db
        .champions_in_role(role, &ctx.rank, &ctx.patch)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let mut candidates = pool.clone();
    if !ctx.pickable.is_empty() {
        candidates = soft_filter(&pool, &ctx.pickable, &banned, &taken, catalog);
    }
    if ctx.owned_only && !ctx.owned.is_empty() {
        candidates = soft_filter(&candidates, &ctx.owned, &banned, &taken, catalog);
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

        let mut syn_weight_sum = 0.0;
        let mut syn_delta_sum = 0.0;
        let mut best_ally: Option<(i64, f64)> = None;
        for (ally_id, ally_role) in &locked_allies {
            if let Some(stat) = db.synergy(champ_id, *ally_id, &ctx.rank, &ctx.patch) {
                let w = ally_role_weight(role, ally_role);
                let delta = shrunk_delta(&stat);
                syn_delta_sum += w * delta;
                syn_weight_sum += w;
                if best_ally.map(|(_, d)| delta > d).unwrap_or(true) {
                    best_ally = Some((*ally_id, delta));
                }
            }
        }
        let synergy_delta = if syn_weight_sum > 0.0 {
            Some(syn_delta_sum / syn_weight_sum)
        } else {
            None
        };

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
            best_ally.map(|(id, _)| id),
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

fn still_lockable(
    id: i64,
    banned: &HashSet<i64>,
    taken: &HashSet<i64>,
    catalog: &Catalog,
) -> bool {
    id > 0 && !banned.contains(&id) && !taken.contains(&id) && catalog.by_id.contains_key(&id)
}

fn soft_filter(
    pool: &HashSet<i64>,
    filter: &HashSet<i64>,
    banned: &HashSet<i64>,
    taken: &HashSet<i64>,
    catalog: &Catalog,
) -> HashSet<i64> {
    let filtered: HashSet<i64> = pool
        .iter()
        .copied()
        .filter(|id| filter.contains(id))
        .collect();
    if filtered
        .iter()
        .any(|id| still_lockable(*id, banned, taken, catalog))
    {
        filtered
    } else {
        pool.clone()
    }
}

fn resolve_role(db: &StatsDb, champion_id: i64, assigned: &str, ctx: &ScoreContext) -> String {
    let assigned = crate::models::lcu_role_to_stats(assigned);
    if !assigned.is_empty() {
        return assigned;
    }
    db.primary_role(champion_id, &ctx.rank, &ctx.patch)
        .unwrap_or_default()
}

fn weights(lane_known: bool, enemies_known: usize) -> Weights {
    if lane_known {
        Weights {
            lane: 0.45,
            team: 0.20,
            syn: 0.25,
            meta: 0.10,
            flex: 0.00,
            comfort: 0.00,
        }
    } else if enemies_known > 0 {
        Weights {
            lane: 0.0,
            team: 0.40,
            syn: 0.25,
            meta: 0.25,
            flex: 0.05,
            comfort: 0.05,
        }
    } else {
        Weights {
            lane: 0.0,
            team: 0.10,
            syn: 0.20,
            meta: 0.50,
            flex: 0.15,
            comfort: 0.05,
        }
    }
}

fn vs_role_weight(our_role: &str, vs_role: &str) -> f64 {
    if vs_role.is_empty() {
        0.20
    } else if our_role == vs_role {
        1.0
    } else if is_duo_lane(our_role, vs_role) {
        0.45
    } else {
        0.20
    }
}

fn ally_role_weight(our_role: &str, ally_role: &str) -> f64 {
    if is_duo_lane(our_role, ally_role) {
        1.0
    } else if ally_role == "jungle" {
        0.35
    } else if ally_role.is_empty() {
        0.25
    } else {
        0.25
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
    const TWITCH: i64 = 29;
    const BRAUM: i64 = 201;
    const SEJUANI: i64 = 113;
    const JANNA: i64 = 40;
    const NAMI: i64 = 267;
    const LULU: i64 = 117;
    const THRESH: i64 = 412;

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
            champ(TWITCH, "Twitch"),
            champ(BRAUM, "Braum"),
            champ(SEJUANI, "Sejuani"),
            champ(JANNA, "Janna"),
            champ(NAMI, "Nami"),
            champ(LULU, "Lulu"),
            champ(THRESH, "Thresh"),
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
        db.upsert_role_stat(TWITCH, "bottom", RANK, PATCH, &bot_meta(51.4, 11_500, 94.0))
            .unwrap();
        db.upsert_role_stat(SINGED, "bottom", RANK, PATCH, &off_role_meta())
            .unwrap();
        db.upsert_role_stat(SINGED, "top", RANK, PATCH, &off_role_meta())
            .unwrap();
    }

    fn support_meta(winrate: f64, games: i64) -> RoleMeta {
        RoleMeta {
            winrate,
            pickrate: 10.0,
            banrate: 1.0,
            games,
            pct_lane: 92.0,
            default_lane: "support".into(),
        }
    }

    fn seed_support_pool(db: &StatsDb) {
        db.upsert_role_stat(JANNA, "support", RANK, PATCH, &support_meta(51.0, 12_000))
            .unwrap();
        db.upsert_role_stat(NAMI, "support", RANK, PATCH, &support_meta(50.8, 11_000))
            .unwrap();
        db.upsert_role_stat(LULU, "support", RANK, PATCH, &support_meta(51.2, 10_500))
            .unwrap();
        db.upsert_role_stat(THRESH, "support", RANK, PATCH, &support_meta(50.4, 11_200))
            .unwrap();
        db.upsert_role_stat(BRAUM, "support", RANK, PATCH, &support_meta(51.6, 10_800))
            .unwrap();
    }

    fn support_lane_mu(db: &StatsDb, champ: i64, enemy: i64, wr: f64) {
        db.upsert_matchup(
            champ,
            enemy,
            "support",
            "lane",
            "support",
            RANK,
            PATCH,
            &MatchupStat {
                winrate: wr,
                games: 8_000,
                delta: wr - 50.0,
            },
        )
        .unwrap();
    }

    fn local_locked_support(local_id: i64, enemy_id: i64) -> DraftView {
        DraftView {
            role: "support".into(),
            allies: vec![
                PlayerSlot {
                    is_local: true,
                    champion_id: local_id,
                    display_champion_id: local_id,
                    assigned_position: "support".into(),
                    ..Default::default()
                },
                PlayerSlot {
                    is_local: false,
                    champion_id: TWITCH,
                    display_champion_id: TWITCH,
                    assigned_position: "bottom".into(),
                    ..Default::default()
                },
            ],
            enemies: vec![PlayerSlot {
                champion_id: enemy_id,
                assigned_position: "support".into(),
                display_champion_id: enemy_id,
                ..Default::default()
            }],
            enemies_locked: 1,
            lane_enemy_id: Some(enemy_id),
            ..Default::default()
        }
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
        assert_eq!(vs_role_weight("bottom", "top"), 0.20);
        assert_eq!(vs_role_weight("bottom", ""), 0.20);
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

    fn lane_mu(db: &StatsDb, champ: i64, enemy: i64, wr: f64) {
        db.upsert_matchup(
            champ,
            enemy,
            "bottom",
            "lane",
            "bottom",
            RANK,
            PATCH,
            &MatchupStat {
                winrate: wr,
                games: 8_000,
                delta: wr - 50.0,
            },
        )
        .unwrap();
    }

    #[test]
    fn jinx_lock_surfaces_twitch_immediately() {
        let db = StatsDb::open_memory().unwrap();
        seed_adc_pool(&db);
        lane_mu(&db, TWITCH, JINX, 55.0);
        lane_mu(&db, MISS_FORTUNE, JINX, 47.0);
        lane_mu(&db, CAITLYN, JINX, 48.0);
        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[TWITCH, MISS_FORTUNE, CAITLYN, SINGED]);
        let recs = recommend(&db, &catalog, &enemy_adc(JINX), &ctx);
        assert!(!recs.is_empty());
        assert_eq!(recs[0].champion_id, TWITCH, "Twitch should lead vs Jinx: {recs:?}");
        assert!(
            recs[0].reason.contains("Jinx"),
            "reason should name Jinx: {}",
            recs[0].reason
        );
    }

    #[test]
    fn later_enemy_that_beats_twitch_drops_him() {
        let db = StatsDb::open_memory().unwrap();
        seed_adc_pool(&db);
        lane_mu(&db, TWITCH, JINX, 55.0);
        lane_mu(&db, MISS_FORTUNE, JINX, 51.0);
        db.upsert_matchup(
            TWITCH,
            SEJUANI,
            "bottom",
            "team",
            "jungle",
            RANK,
            PATCH,
            &MatchupStat {
                winrate: 40.0,
                games: 6_000,
                delta: -10.0,
            },
        )
        .unwrap();
        db.upsert_matchup(
            MISS_FORTUNE,
            SEJUANI,
            "bottom",
            "team",
            "jungle",
            RANK,
            PATCH,
            &MatchupStat {
                winrate: 52.0,
                games: 6_000,
                delta: 2.0,
            },
        )
        .unwrap();
        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[TWITCH, MISS_FORTUNE]);
        let jinx_only = recommend(&db, &catalog, &enemy_adc(JINX), &ctx);
        let both = recommend(
            &db,
            &catalog,
            &DraftView {
                role: "bottom".into(),
                enemies: vec![
                    PlayerSlot {
                        champion_id: JINX,
                        assigned_position: "bottom".into(),
                        display_champion_id: JINX,
                        ..Default::default()
                    },
                    PlayerSlot {
                        champion_id: SEJUANI,
                        assigned_position: "jungle".into(),
                        display_champion_id: SEJUANI,
                        ..Default::default()
                    },
                ],
                enemies_locked: 2,
                lane_enemy_id: Some(JINX),
                ..Default::default()
            },
            &ctx,
        );
        let twitch_jinx = jinx_only.iter().position(|r| r.champion_id == TWITCH);
        let twitch_both = both.iter().position(|r| r.champion_id == TWITCH);
        let mf_both = both.iter().position(|r| r.champion_id == MISS_FORTUNE);
        assert!(twitch_jinx.is_some() && twitch_both.is_some() && mf_both.is_some());
        assert!(
            twitch_jinx.unwrap() <= twitch_both.unwrap(),
            "Twitch should not rise after a bad team matchup"
        );
        assert!(
            mf_both.unwrap() < twitch_both.unwrap(),
            "MF should outrank Twitch once Sejuani is in: {both:?}"
        );
    }

    #[test]
    fn braum_synergy_raises_twitch() {
        let db = StatsDb::open_memory().unwrap();
        seed_adc_pool(&db);
        lane_mu(&db, TWITCH, JINX, 52.0);
        lane_mu(&db, MISS_FORTUNE, JINX, 51.5);
        db.upsert_synergy(
            TWITCH,
            BRAUM,
            RANK,
            PATCH,
            &MatchupStat {
                winrate: 54.0,
                games: 9_000,
                delta: 4.0,
            },
        )
        .unwrap();
        db.upsert_synergy(
            MISS_FORTUNE,
            BRAUM,
            RANK,
            PATCH,
            &MatchupStat {
                winrate: 50.0,
                games: 9_000,
                delta: 0.0,
            },
        )
        .unwrap();
        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[TWITCH, MISS_FORTUNE]);
        let with_braum = DraftView {
            role: "bottom".into(),
            allies: vec![PlayerSlot {
                is_local: false,
                display_champion_id: BRAUM,
                assigned_position: "support".into(),
                ..Default::default()
            }],
            enemies: vec![PlayerSlot {
                champion_id: JINX,
                assigned_position: "bottom".into(),
                display_champion_id: JINX,
                ..Default::default()
            }],
            enemies_locked: 1,
            lane_enemy_id: Some(JINX),
            ..Default::default()
        };
        let recs = recommend(&db, &catalog, &with_braum, &ctx);
        assert_eq!(recs[0].champion_id, TWITCH, "Braum synergy should put Twitch first: {recs:?}");
        assert!(
            recs[0].reason.contains("Braum") || recs[0].synergy_delta.is_some(),
            "reason should mention Braum: {}",
            recs[0].reason
        );
    }

    #[test]
    fn role_stats_without_matchups_still_recommend() {
        let db = StatsDb::open_memory().unwrap();
        seed_adc_pool(&db);
        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[TWITCH, MISS_FORTUNE, CAITLYN]);
        let recs = recommend(&db, &catalog, &enemy_adc(JINX), &ctx);
        assert!(
            !recs.is_empty(),
            "meta-only cache must still produce ADC recs"
        );
    }

    #[test]
    fn mismatched_rank_patch_falls_back_to_cached_stats() {
        let db = StatsDb::open_memory().unwrap();
        seed_adc_pool(&db);
        let catalog = test_catalog();
        let ctx = ScoreContext {
            rank: "gold_plus".into(),
            patch: "16.17".into(),
            owned_only: false,
            comfort_weighting: false,
            pickable: HashSet::new(),
            owned: HashSet::new(),
            mastery: HashMap::new(),
        };
        let recs = recommend(&db, &catalog, &empty_draft(), &ctx);
        assert!(!recs.is_empty(), "should use fallback stats key: {recs:?}");
    }

    #[test]
    fn nami_lock_surfaces_thresh_for_support() {
        let db = StatsDb::open_memory().unwrap();
        seed_support_pool(&db);
        support_lane_mu(&db, THRESH, NAMI, 55.0);
        support_lane_mu(&db, LULU, NAMI, 47.0);
        support_lane_mu(&db, JANNA, NAMI, 48.0);
        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[THRESH, LULU, JANNA, BRAUM]);
        let recs = recommend(
            &db,
            &catalog,
            &DraftView {
                role: "support".into(),
                enemies: vec![PlayerSlot {
                    champion_id: NAMI,
                    assigned_position: "support".into(),
                    display_champion_id: NAMI,
                    ..Default::default()
                }],
                enemies_locked: 1,
                lane_enemy_id: Some(NAMI),
                ..Default::default()
            },
            &ctx,
        );
        assert!(!recs.is_empty());
        assert_eq!(
            recs[0].champion_id, THRESH,
            "Thresh should lead vs Nami: {recs:?}"
        );
        assert!(
            recs[0].reason.contains("Nami"),
            "reason should name Nami: {}",
            recs[0].reason
        );
    }

    #[test]
    fn local_support_lock_does_not_empty_recs() {
        let db = StatsDb::open_memory().unwrap();
        seed_support_pool(&db);
        support_lane_mu(&db, THRESH, NAMI, 54.0);
        support_lane_mu(&db, LULU, NAMI, 52.0);
        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[JANNA]);
        let recs = recommend(&db, &catalog, &local_locked_support(JANNA, NAMI), &ctx);
        assert!(
            !recs.is_empty(),
            "after locking Janna, other supports must still rank: {recs:?}"
        );
        assert!(
            recs.iter().all(|r| r.champion_id != JANNA),
            "locked local Janna should not be recommended: {recs:?}"
        );
    }

    #[test]
    fn utility_role_alias_still_recommends_supports() {
        let db = StatsDb::open_memory().unwrap();
        seed_support_pool(&db);
        let catalog = test_catalog();
        let ctx = ctx_with_pickable(&[THRESH, LULU, JANNA]);
        let recs = recommend(
            &db,
            &catalog,
            &DraftView {
                role: "utility".into(),
                enemies: vec![PlayerSlot {
                    champion_id: NAMI,
                    assigned_position: "utility".into(),
                    display_champion_id: NAMI,
                    ..Default::default()
                }],
                enemies_locked: 1,
                ..Default::default()
            },
            &ctx,
        );
        assert!(
            !recs.is_empty(),
            "LCU utility must map to support stats: {recs:?}"
        );
        assert!(recs.iter().all(|r| {
            r.champion_id == THRESH || r.champion_id == LULU || r.champion_id == JANNA || r.champion_id == BRAUM
        }));
    }
}
