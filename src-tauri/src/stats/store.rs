use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

pub const STATS_SCHEMA: &str = "2";

#[derive(Debug, Clone)]
pub struct RoleMeta {
    pub winrate: f64,
    pub pickrate: f64,
    pub banrate: f64,
    pub games: i64,
    pub pct_lane: f64,
    pub default_lane: String,
}

impl RoleMeta {
    pub fn in_role_pool(&self, role: &str) -> bool {
        self.games >= 150
            && (self.pct_lane >= 15.0 || (self.default_lane == role && self.games >= 200))
    }
}

#[derive(Debug, Clone)]
pub struct MatchupStat {
    pub winrate: f64,
    pub games: i64,
    pub delta: f64,
}

pub struct StatsDb {
    conn: Mutex<Connection>,
}

impl StatsDb {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("open sqlite")?;
        init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open memory sqlite")?;
        init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn get_meta(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .ok()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn begin_ingest(&self, _rank: &str, _patch: &str) -> Result<()> {
        // Upsert-only. Never wipe matchups/synergies up front — a failed refresh
        // must leave the previous counter tables intact for champ select.
        Ok(())
    }

    pub fn upsert_role_stat(
        &self,
        champion_id: i64,
        role: &str,
        rank: &str,
        patch: &str,
        meta: &RoleMeta,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO role_stats
             (champion_id, role, rank, patch, winrate, pickrate, banrate, games, pct_lane, default_lane)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                champion_id,
                role,
                rank,
                patch,
                meta.winrate,
                meta.pickrate,
                meta.banrate,
                meta.games,
                meta.pct_lane,
                meta.default_lane
            ],
        )?;
        Ok(())
    }

    pub fn upsert_matchup(
        &self,
        champion_id: i64,
        enemy_id: i64,
        role: &str,
        kind: &str,
        vs_role: &str,
        rank: &str,
        patch: &str,
        stat: &MatchupStat,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO matchups
             (champion_id, enemy_id, role, kind, vs_role, rank, patch, winrate, games, delta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                champion_id,
                enemy_id,
                role,
                kind,
                vs_role,
                rank,
                patch,
                stat.winrate,
                stat.games,
                stat.delta
            ],
        )?;
        Ok(())
    }

    pub fn upsert_synergy(
        &self,
        champion_id: i64,
        ally_id: i64,
        rank: &str,
        patch: &str,
        stat: &MatchupStat,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO synergies
             (champion_id, ally_id, rank, patch, winrate, games, delta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                champion_id,
                ally_id,
                rank,
                patch,
                stat.winrate,
                stat.games,
                stat.delta
            ],
        )?;
        Ok(())
    }

    pub fn role_meta(&self, champion_id: i64, role: &str, rank: &str, patch: &str) -> Option<RoleMeta> {
        let role = crate::models::lcu_role_to_stats(role);
        let (rank, patch) = self.resolve_stats_key(rank, patch);
        let found = {
            let conn = self.conn.lock().ok()?;
            conn.query_row(
                "SELECT winrate, pickrate, banrate, games, pct_lane, default_lane FROM role_stats
                 WHERE champion_id = ?1 AND role = ?2 AND rank = ?3 AND patch = ?4",
                params![champion_id, role, rank, patch],
                |row| Ok(read_role_meta(row, 0)),
            )
            .ok()
        };
        found.or_else(|| self.role_meta_any(champion_id, &role))
    }

    fn role_meta_any(&self, champion_id: i64, role: &str) -> Option<RoleMeta> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT winrate, pickrate, banrate, games, pct_lane, default_lane FROM role_stats
             WHERE champion_id = ?1 AND role = ?2
             ORDER BY games DESC LIMIT 1",
            params![champion_id, role],
            |row| Ok(read_role_meta(row, 0)),
        )
        .ok()
    }

    pub fn matchup(
        &self,
        champion_id: i64,
        enemy_id: i64,
        role: &str,
        vs_role: &str,
        rank: &str,
        patch: &str,
    ) -> Option<MatchupStat> {
        let role = crate::models::lcu_role_to_stats(role);
        let vs_role = crate::models::lcu_role_to_stats(vs_role);
        let (rank, patch) = self.resolve_stats_key(rank, patch);
        self.matchup_at(champion_id, enemy_id, &role, &vs_role, &rank, &patch)
            .or_else(|| self.matchup_any(champion_id, enemy_id, &role, &vs_role))
    }

    fn matchup_at(
        &self,
        champion_id: i64,
        enemy_id: i64,
        role: &str,
        vs_role: &str,
        rank: &str,
        patch: &str,
    ) -> Option<MatchupStat> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT winrate, games, delta FROM matchups
             WHERE champion_id = ?1 AND enemy_id = ?2 AND role = ?3 AND rank = ?4 AND patch = ?5
               AND (vs_role = ?6 OR vs_role = '' OR kind = 'lane')
             ORDER BY CASE
                WHEN vs_role = ?6 THEN 0
                WHEN kind = 'lane' THEN 1
                ELSE 2
             END
             LIMIT 1",
            params![champion_id, enemy_id, role, rank, patch, vs_role],
            |row| {
                Ok(MatchupStat {
                    winrate: row.get(0)?,
                    games: row.get(1)?,
                    delta: row.get(2)?,
                })
            },
        )
        .ok()
    }

    fn matchup_any(
        &self,
        champion_id: i64,
        enemy_id: i64,
        role: &str,
        vs_role: &str,
    ) -> Option<MatchupStat> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT winrate, games, delta FROM matchups
             WHERE champion_id = ?1 AND enemy_id = ?2 AND role = ?3
               AND (vs_role = ?4 OR vs_role = '' OR kind = 'lane')
             ORDER BY CASE
                WHEN vs_role = ?4 THEN 0
                WHEN kind = 'lane' THEN 1
                ELSE 2
             END
             LIMIT 1",
            params![champion_id, enemy_id, role, vs_role],
            |row| {
                Ok(MatchupStat {
                    winrate: row.get(0)?,
                    games: row.get(1)?,
                    delta: row.get(2)?,
                })
            },
        )
        .ok()
    }

    pub fn synergy(
        &self,
        champion_id: i64,
        ally_id: i64,
        rank: &str,
        patch: &str,
    ) -> Option<MatchupStat> {
        let (rank, patch) = self.resolve_stats_key(rank, patch);
        let found = {
            let conn = self.conn.lock().ok()?;
            conn.query_row(
                "SELECT winrate, games, delta FROM synergies
                 WHERE champion_id = ?1 AND ally_id = ?2 AND rank = ?3 AND patch = ?4",
                params![champion_id, ally_id, rank, patch],
                |row| {
                    Ok(MatchupStat {
                        winrate: row.get(0)?,
                        games: row.get(1)?,
                        delta: row.get(2)?,
                    })
                },
            )
            .ok()
        };
        found.or_else(|| self.synergy_any(champion_id, ally_id))
    }

    fn synergy_any(&self, champion_id: i64, ally_id: i64) -> Option<MatchupStat> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT winrate, games, delta FROM synergies
             WHERE champion_id = ?1 AND ally_id = ?2
             ORDER BY games DESC LIMIT 1",
            params![champion_id, ally_id],
            |row| {
                Ok(MatchupStat {
                    winrate: row.get(0)?,
                    games: row.get(1)?,
                    delta: row.get(2)?,
                })
            },
        )
        .ok()
    }

    /// Games-weighted mean win rate across this champion's lane matchups, so a
    /// 40-game matchup no longer counts as much as a 40,000-game one.
    pub fn flexibility(&self, champion_id: i64, role: &str, rank: &str, patch: &str) -> Option<f64> {
        let (rank, patch) = self.resolve_stats_key(rank, patch);
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT SUM(winrate * games) / SUM(games) FROM matchups
             WHERE champion_id = ?1 AND role = ?2 AND kind = 'lane' AND rank = ?3 AND patch = ?4
               AND games > 0",
            params![champion_id, role, rank, patch],
            |row| row.get::<_, Option<f64>>(0),
        )
        .ok()
        .flatten()
    }

    pub fn champions_in_role(&self, role: &str, rank: &str, patch: &str) -> Vec<(i64, RoleMeta)> {
        let role = crate::models::lcu_role_to_stats(role);
        let (rank, patch) = self.resolve_stats_key(rank, patch);
        let strict = self.query_role_champs(&role, &rank, &patch, true);
        if !strict.is_empty() {
            return strict;
        }
        let relaxed = self.query_role_champs(&role, &rank, &patch, false);
        if !relaxed.is_empty() {
            return relaxed;
        }
        self.query_role_champs_any(&role)
    }

    fn query_role_champs(
        &self,
        role: &str,
        rank: &str,
        patch: &str,
        strict: bool,
    ) -> Vec<(i64, RoleMeta)> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let sql = if strict {
            "SELECT champion_id, winrate, pickrate, banrate, games, pct_lane, default_lane FROM role_stats
             WHERE role = ?1 AND rank = ?2 AND patch = ?3 AND games >= 150
               AND (pct_lane >= 15 OR (default_lane = ?1 AND games >= 200))"
        } else {
            "SELECT champion_id, winrate, pickrate, banrate, games, pct_lane, default_lane FROM role_stats
             WHERE role = ?1 AND rank = ?2 AND patch = ?3 AND games >= 80"
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![role, rank, patch], |row| {
            Ok((row.get::<_, i64>(0)?, read_role_meta(row, 1)))
        });
        match rows {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn primary_role(&self, champion_id: i64, rank: &str, patch: &str) -> Option<String> {
        let (rank, patch) = self.resolve_stats_key(rank, patch);
        let found = {
            let conn = self.conn.lock().ok()?;
            conn.query_row(
                "SELECT default_lane, role FROM role_stats
                 WHERE champion_id = ?1 AND rank = ?2 AND patch = ?3
                 ORDER BY games DESC LIMIT 1",
                params![champion_id, rank, patch],
                |row| {
                    let default_lane: String = row.get(0)?;
                    let role: String = row.get(1)?;
                    Ok(if !default_lane.is_empty() {
                        default_lane
                    } else {
                        role
                    })
                },
            )
            .ok()
        };
        found
            .or_else(|| {
                let conn = self.conn.lock().ok()?;
                conn.query_row(
                    "SELECT default_lane, role FROM role_stats
                     WHERE champion_id = ?1
                     ORDER BY games DESC LIMIT 1",
                    params![champion_id],
                    |row| {
                        let default_lane: String = row.get(0)?;
                        let role: String = row.get(1)?;
                        Ok(if !default_lane.is_empty() {
                            default_lane
                        } else {
                            role
                        })
                    },
                )
                .ok()
            })
            .map(|role| crate::models::normalize_role(&role))
            .filter(|role| !role.is_empty())
    }

    pub fn has_patch_data(&self, rank: &str, patch: &str) -> bool {
        self.role_stat_count(rank, patch) > 20
    }

    pub fn has_any_role_data(&self) -> bool {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return false,
        };
        conn.query_row("SELECT COUNT(*) FROM role_stats", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0)
            > 20
    }

    pub fn has_matchup_data(&self, rank: &str, patch: &str) -> bool {
        let (rank, patch) = self.resolve_stats_key(rank, patch);
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return false,
        };
        conn.query_row(
            "SELECT COUNT(*) FROM matchups WHERE rank = ?1 AND patch = ?2",
            params![rank, patch],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 50
    }

    pub fn resolve_stats_key(&self, rank: &str, patch: &str) -> (String, String) {
        if self.role_stat_count(rank, patch) > 0 {
            return (rank.to_string(), patch.to_string());
        }
        if let Some(p) = self.latest_patch_for_rank(rank) {
            return (rank.to_string(), p);
        }
        if let Some(r) = self.any_rank_for_patch(patch) {
            return (r, patch.to_string());
        }
        if let (Some(r), Some(p)) = (self.get_meta("rank"), self.get_meta("patch")) {
            if self.role_stat_count(&r, &p) > 0 {
                return (r, p);
            }
        }
        if let Some(key) = self.any_stats_key() {
            return key;
        }
        (rank.to_string(), patch.to_string())
    }

    fn role_stat_count(&self, rank: &str, patch: &str) -> i64 {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return 0,
        };
        conn.query_row(
            "SELECT COUNT(*) FROM role_stats WHERE rank = ?1 AND patch = ?2",
            params![rank, patch],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    fn latest_patch_for_rank(&self, rank: &str) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT patch FROM role_stats WHERE rank = ?1
             GROUP BY patch ORDER BY COUNT(*) DESC LIMIT 1",
            params![rank],
            |row| row.get(0),
        )
        .ok()
    }

    fn any_rank_for_patch(&self, patch: &str) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT rank FROM role_stats WHERE patch = ?1
             GROUP BY rank ORDER BY COUNT(*) DESC LIMIT 1",
            params![patch],
            |row| row.get(0),
        )
        .ok()
    }

    fn any_stats_key(&self) -> Option<(String, String)> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT rank, patch FROM role_stats
             GROUP BY rank, patch ORDER BY COUNT(*) DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()
    }

    fn query_role_champs_any(&self, role: &str) -> Vec<(i64, RoleMeta)> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut stmt = match conn.prepare(
            "SELECT champion_id, winrate, pickrate, banrate, games, pct_lane, default_lane FROM role_stats
             WHERE role = ?1
             ORDER BY games DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![role], |row| {
            Ok((row.get::<_, i64>(0)?, read_role_meta(row, 1)))
        });
        match rows {
            Ok(iter) => {
                let mut seen = std::collections::HashSet::new();
                iter.filter_map(|r| r.ok())
                    .filter(|(id, _)| seen.insert(*id))
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }
}

fn read_role_meta(row: &rusqlite::Row<'_>, start: usize) -> RoleMeta {
    RoleMeta {
        winrate: row.get(start).unwrap_or(50.0),
        pickrate: row.get(start + 1).unwrap_or(0.0),
        banrate: row.get(start + 2).unwrap_or(0.0),
        games: row.get(start + 3).unwrap_or(0),
        pct_lane: row.get(start + 4).unwrap_or(0.0),
        default_lane: row.get(start + 5).unwrap_or_default(),
    }
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#,
    )?;
    let schema: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'stats_schema'",
            [],
            |row| row.get(0),
        )
        .ok();
    if schema.as_deref() != Some(STATS_SCHEMA) {
        conn.execute_batch("DROP TABLE IF EXISTS matchups;")?;
    }
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS role_stats (
            champion_id INTEGER NOT NULL,
            role TEXT NOT NULL,
            rank TEXT NOT NULL,
            patch TEXT NOT NULL,
            winrate REAL NOT NULL,
            pickrate REAL NOT NULL,
            banrate REAL NOT NULL,
            games INTEGER NOT NULL,
            pct_lane REAL NOT NULL DEFAULT 0,
            default_lane TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (champion_id, role, rank, patch)
        );
        CREATE TABLE IF NOT EXISTS matchups (
            champion_id INTEGER NOT NULL,
            enemy_id INTEGER NOT NULL,
            role TEXT NOT NULL,
            kind TEXT NOT NULL,
            vs_role TEXT NOT NULL DEFAULT '',
            rank TEXT NOT NULL,
            patch TEXT NOT NULL,
            winrate REAL NOT NULL,
            games INTEGER NOT NULL,
            delta REAL NOT NULL,
            PRIMARY KEY (champion_id, enemy_id, role, kind, vs_role, rank, patch)
        );
        CREATE TABLE IF NOT EXISTS synergies (
            champion_id INTEGER NOT NULL,
            ally_id INTEGER NOT NULL,
            rank TEXT NOT NULL,
            patch TEXT NOT NULL,
            winrate REAL NOT NULL,
            games INTEGER NOT NULL,
            delta REAL NOT NULL,
            PRIMARY KEY (champion_id, ally_id, rank, patch)
        );
        CREATE INDEX IF NOT EXISTS idx_matchups_lookup
            ON matchups (champion_id, role, rank, patch);
        "#,
    )?;
    let _ = conn.execute(
        "ALTER TABLE role_stats ADD COLUMN pct_lane REAL NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE role_stats ADD COLUMN default_lane TEXT NOT NULL DEFAULT ''",
        [],
    );
    Ok(())
}
