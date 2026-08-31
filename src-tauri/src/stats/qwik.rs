use serde_json::Value;

/// Resolve Lolalytics Qwik SSR object graph into enemy matchup rows.
/// Rows are `[id, wr, d1, d2, pr, n]`.
pub fn parse_enemy_matchups(html: &str) -> Option<EnemyTables> {
    let json = extract_qwik_json(html)?;
    let root: Value = serde_json::from_str(json).ok()?;
    let objs = root.get("objs")?.as_array()?;
    let enemy_ref = find_enemy_ref(objs)?;
    let enemy_obj = deref(objs, &enemy_ref)?;
    let map = enemy_obj.as_object()?;
    let mut tables = EnemyTables::default();
    for (role, key) in [
        ("top", "top"),
        ("jungle", "jungle"),
        ("middle", "middle"),
        ("bottom", "bottom"),
        ("support", "support"),
    ] {
        if let Some(rows) = map.get(key).and_then(|v| resolve_rows(objs, v)) {
            match role {
                "top" => tables.top = rows,
                "jungle" => tables.jungle = rows,
                "middle" => tables.middle = rows,
                "bottom" => tables.bottom = rows,
                "support" => tables.support = rows,
                _ => {}
            }
        }
    }
    if tables.is_empty() {
        None
    } else {
        Some(tables)
    }
}

#[derive(Default, Debug)]
pub struct EnemyTables {
    pub top: Vec<MatchupRow>,
    pub jungle: Vec<MatchupRow>,
    pub middle: Vec<MatchupRow>,
    pub bottom: Vec<MatchupRow>,
    pub support: Vec<MatchupRow>,
}

impl EnemyTables {
    pub fn is_empty(&self) -> bool {
        self.top.is_empty()
            && self.jungle.is_empty()
            && self.middle.is_empty()
            && self.bottom.is_empty()
            && self.support.is_empty()
    }

    #[allow(dead_code)]
    pub fn for_role(&self, role: &str) -> &[MatchupRow] {
        match role {
            "top" => &self.top,
            "jungle" => &self.jungle,
            "middle" => &self.middle,
            "bottom" => &self.bottom,
            "support" => &self.support,
            _ => &self.middle,
        }
    }

    pub fn all_rows(&self) -> impl Iterator<Item = (&'static str, &MatchupRow)> {
        self.top
            .iter()
            .map(|r| ("top", r))
            .chain(self.jungle.iter().map(|r| ("jungle", r)))
            .chain(self.middle.iter().map(|r| ("middle", r)))
            .chain(self.bottom.iter().map(|r| ("bottom", r)))
            .chain(self.support.iter().map(|r| ("support", r)))
    }
}

#[derive(Debug, Clone)]
pub struct MatchupRow {
    pub champion_id: i64,
    pub winrate: f64,
    pub delta: f64,
    pub games: i64,
}

fn extract_qwik_json(html: &str) -> Option<&str> {
    let start_tag = r#"<script type="qwik/json">"#;
    let start = html.find(start_tag)? + start_tag.len();
    let rest = &html[start..];
    let end = rest.find("</script>")?;
    Some(rest[..end].trim_start_matches('\u{feff}').trim())
}

fn find_enemy_ref(objs: &[Value]) -> Option<String> {
    for obj in objs {
        if let Some(map) = obj.as_object() {
            if let Some(Value::String(r)) = map.get("enemy") {
                return Some(r.clone());
            }
        }
    }
    None
}

fn deref<'a>(objs: &'a [Value], token: &str) -> Option<&'a Value> {
    let idx = usize::from_str_radix(token, 36).ok()?;
    objs.get(idx)
}

fn resolve_rows(objs: &[Value], node: &Value) -> Option<Vec<MatchupRow>> {
    let arr_val = match node {
        Value::String(s) => deref(objs, s)?,
        other => other,
    };
    let arr = arr_val.as_array()?;
    let mut rows = Vec::new();
    for item in arr {
        let row_val = match item {
            Value::String(s) => deref(objs, s).unwrap_or(item),
            other => other,
        };
        if let Some(row) = parse_row(row_val) {
            rows.push(row);
        }
    }
    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

fn parse_row(value: &Value) -> Option<MatchupRow> {
    let arr = value.as_array()?;
    if arr.len() < 6 {
        return None;
    }
    let champion_id = arr[0].as_i64().or_else(|| arr[0].as_f64().map(|n| n as i64))?;
    let winrate = arr[1].as_f64()?;
    let delta = arr[2].as_f64().unwrap_or(winrate - 50.0);
    let games = arr[5]
        .as_i64()
        .or_else(|| arr[5].as_f64().map(|n| n as i64))?;
    if champion_id <= 0 || !(20.0..=80.0).contains(&winrate) {
        return None;
    }
    Some(MatchupRow {
        champion_id,
        winrate,
        delta,
        games,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_qwik_enemy_graph() {
        let html = r#"<script type="qwik/json">{"refs":{},"objs":[{"enemy":"2"},0,{"top":[],"jungle":[],"middle":"3","bottom":[],"support":[]},[[7,54.2,4.2,1.0,5.0,2000]]],"subs":{}}</script>"#;
        let tables = parse_enemy_matchups(html).expect("tables");
        assert_eq!(tables.middle.len(), 1);
        assert_eq!(tables.middle[0].champion_id, 7);
        assert!((tables.middle[0].winrate - 54.2).abs() < 0.01);
    }
}
