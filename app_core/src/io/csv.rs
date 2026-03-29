//! CSV export and import for session data.
//!
//! Supports RFC4180-style quoted fields, escaped quotes, and embedded newlines.

use crate::models::{MatchId, MatchResult, Player, PlayerRanking, ScheduledMatch, SessionConfig};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CsvError {
    #[error("CSV format error: {0}")]
    Format(String),
    #[error("Missing column: {0}")]
    MissingColumn(String),
}

pub type CsvResult<T> = Result<T, CsvError>;

/// One imported match with team assignments preserved.
/// `goals = None` means the player did not play.
#[derive(Debug, Clone)]
pub struct RawImportedMatch {
    pub round: u32,
    /// (player_name, goals). `goals = None` → did not play.
    pub team_a: Vec<(String, Option<u16>)>,
    pub team_b: Vec<(String, Option<u16>)>,
    pub duration_multiplier: f64,
}

/// Output of `import_results`: everything needed to reconstruct a session.
#[derive(Debug, Clone)]
pub struct ImportedResults {
    pub sport: Option<String>,
    pub team_size: Option<u8>,
    pub scheduling_frequency: Option<u8>,
    pub match_duration_minutes: Option<u16>,
    /// All players from the metadata header, in registration order.
    pub players: Vec<String>,
    pub matches: Vec<RawImportedMatch>,
}

fn write_csv_record(out: &mut String, fields: &[String]) {
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if field.contains([',', '"', '\n', '\r']) {
            out.push('"');
            for ch in field.chars() {
                if ch == '"' {
                    out.push('"');
                    out.push('"');
                } else {
                    out.push(ch);
                }
            }
            out.push('"');
        } else {
            out.push_str(field);
        }
    }
    out.push('\n');
}

fn parse_csv_records(csv: &str) -> CsvResult<Vec<Vec<String>>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut chars = csv.chars().peekable();
    let mut in_quotes = false;
    let mut row = 1usize;
    let mut col = 1usize;

    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' => {
                    if matches!(chars.peek(), Some('"')) {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                _ => field.push(ch),
            }
            continue;
        }

        match ch {
            '"' => {
                if field.is_empty() {
                    in_quotes = true;
                } else {
                    return Err(CsvError::Format(format!(
                        "unexpected quote at row {row}, column {col}"
                    )));
                }
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                col += 1;
            }
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                row += 1;
                col = 1;
            }
            '\r' => {
                if matches!(chars.peek(), Some('\n')) {
                    chars.next();
                }
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                row += 1;
                col = 1;
            }
            _ => field.push(ch),
        }
    }

    if in_quotes {
        return Err(CsvError::Format("unterminated quoted field".to_string()));
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }

    if let Some(first_row) = records.first_mut() {
        if let Some(first_cell) = first_row.first_mut() {
            if first_cell.starts_with('\u{feff}') {
                *first_cell = first_cell.trim_start_matches('\u{feff}').to_string();
            }
        }
    }

    Ok(records)
}

#[derive(Clone, Copy)]
struct RankingCols {
    rank: usize,
    name: usize,
    rating: usize,
    uncertainty: usize,
    rank_lo: usize,
    rank_hi: usize,
    matches_played: usize,
    total_goals: usize,
    prob_top_k: Option<usize>,
    active: Option<usize>,
}

impl RankingCols {
    fn defaults() -> Self {
        Self {
            rank: 0,
            name: 1,
            rating: 2,
            uncertainty: 3,
            rank_lo: 4,
            rank_hi: 5,
            matches_played: 6,
            total_goals: 7,
            prob_top_k: Some(8),
            active: Some(9),
        }
    }
}

fn header_index(header: &[String], names: &[&str]) -> Option<usize> {
    let lowered: Vec<String> = header
        .iter()
        .map(|h| h.trim().to_ascii_lowercase())
        .collect();
    names
        .iter()
        .find_map(|name| lowered.iter().position(|h| h == *name))
}

fn parse_rankings_columns(header: &[String]) -> CsvResult<RankingCols> {
    let req = |names: &[&str], label: &str| {
        header_index(header, names).ok_or_else(|| CsvError::MissingColumn(label.to_string()))
    };

    Ok(RankingCols {
        rank: req(&["rank"], "rank")?,
        name: req(&["name"], "name")?,
        rating: req(&["rating"], "rating")?,
        uncertainty: req(&["uncertainty"], "uncertainty")?,
        rank_lo: req(&["rank_lo_90", "rank_lo"], "rank_lo_90")?,
        rank_hi: req(&["rank_hi_90", "rank_hi"], "rank_hi_90")?,
        matches_played: req(&["matches_played"], "matches_played")?,
        total_goals: req(&["total_goals"], "total_goals")?,
        prob_top_k: header_index(header, &["prob_top_k"]),
        active: header_index(header, &["active", "is_active"]),
    })
}

fn parse_required<'a>(
    row: &'a [String],
    idx: usize,
    col_name: &str,
    row_num: usize,
) -> CsvResult<&'a str> {
    row.get(idx)
        .map(|s| s.trim())
        .ok_or_else(|| CsvError::MissingColumn(format!("{col_name} (row {row_num})")))
}

fn parse_active(raw: &str, row_num: usize) -> CsvResult<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "yes" | "true" | "1" => Ok(true),
        "no" | "false" | "0" => Ok(false),
        other => Err(CsvError::Format(format!(
            "bad active flag '{other}' at row {row_num}"
        ))),
    }
}

/// Export rankings to CSV string.
pub fn export_rankings(rankings: &[PlayerRanking], players: &[Player]) -> String {
    let mut out = String::new();
    write_csv_record(
        &mut out,
        &[
            "rank".to_string(),
            "name".to_string(),
            "rating".to_string(),
            "uncertainty".to_string(),
            "rank_lo_90".to_string(),
            "rank_hi_90".to_string(),
            "matches_played".to_string(),
            "total_goals".to_string(),
            "prob_top_k".to_string(),
            "active".to_string(),
        ],
    );

    let name_map: std::collections::HashMap<u32, &str> =
        players.iter().map(|p| (p.id.0, p.name.as_str())).collect();

    let mut sorted: Vec<&PlayerRanking> = rankings.iter().collect();
    sorted.sort_by_key(|r| r.rank);

    for r in sorted {
        let name = name_map.get(&r.player_id.0).copied().unwrap_or("?");
        write_csv_record(
            &mut out,
            &[
                r.rank.to_string(),
                name.to_string(),
                format!("{:.4}", r.rating),
                format!("{:.4}", r.uncertainty),
                r.rank_range_90.0.to_string(),
                r.rank_range_90.1.to_string(),
                r.matches_played.to_string(),
                r.total_goals.to_string(),
                format!("{:.4}", r.prob_top_k),
                if r.is_active { "yes" } else { "no" }.to_string(),
            ],
        );
    }
    out
}

/// Export match results to CSV string with session metadata headers.
///
/// The output includes `# key: value` comment lines at the top (sport, team_size,
/// scheduling_frequency, match_duration, and one `# player:` line per player), followed
/// by a standard CSV with `match_id`, `round`, `team` (0=A / 1=B), `player`, `goals`,
/// `did_not_play`, and `duration_multiplier` columns.
///
/// Pass this output to `import_results` to reconstruct a session on another device.
pub fn export_results(
    results: &[&MatchResult],
    players: &[Player],
    matches: &HashMap<MatchId, ScheduledMatch>,
    config: &SessionConfig,
) -> String {
    let mut out = String::new();

    // ── Metadata header ──────────────────────────────────────────────────────
    out.push_str(&format!("# sport: {}\n", config.sport));
    out.push_str(&format!("# team_size: {}\n", config.team_size));
    out.push_str(&format!(
        "# scheduling_frequency: {}\n",
        config.scheduling_frequency
    ));
    if let Some(mins) = config.match_duration_minutes {
        out.push_str(&format!("# match_duration: {mins}\n"));
    }
    // All players in ID order so the recovered session has a stable roster.
    let mut sorted_players: Vec<&Player> = players.iter().collect();
    sorted_players.sort_by_key(|p| p.id.0);
    for p in &sorted_players {
        out.push_str(&format!("# player: {}\n", p.name));
    }

    // ── CSV header ───────────────────────────────────────────────────────────
    write_csv_record(
        &mut out,
        &[
            "match_id".to_string(),
            "round".to_string(),
            "team".to_string(),
            "player".to_string(),
            "goals".to_string(),
            "did_not_play".to_string(),
            "duration_multiplier".to_string(),
        ],
    );

    let name_map: HashMap<u32, &str> =
        players.iter().map(|p| (p.id.0, p.name.as_str())).collect();

    // Sort by (round, match_id) for deterministic, human-readable output.
    let mut sorted_results: Vec<&MatchResult> = results.iter().copied().collect();
    sorted_results.sort_by_key(|r| {
        let round = matches.get(&r.match_id).map(|m| m.round.0).unwrap_or(0);
        (round, r.match_id.0)
    });

    for result in sorted_results {
        let sm = matches.get(&result.match_id);
        let round = sm.map(|m| m.round.0).unwrap_or(0);
        let team_a_ids: Vec<_> = sm.map(|m| m.team_a.clone()).unwrap_or_default();
        let team_b_ids: Vec<_> = sm.map(|m| m.team_b.clone()).unwrap_or_default();

        let write_row = |out: &mut String, pid: &crate::models::PlayerId, team: u8| {
            if let Some(score) = result.scores.get(pid) {
                let name = name_map.get(&pid.0).copied().unwrap_or("?");
                write_csv_record(
                    out,
                    &[
                        result.match_id.0.to_string(),
                        round.to_string(),
                        team.to_string(),
                        name.to_string(),
                        score.goals.unwrap_or(0).to_string(),
                        if score.goals.is_none() { "yes" } else { "no" }.to_string(),
                        result.duration_multiplier.to_string(),
                    ],
                );
            }
        };

        for pid in &team_a_ids {
            write_row(&mut out, pid, 0);
        }
        for pid in &team_b_ids {
            write_row(&mut out, pid, 1);
        }

        // Fallback: any scored players not present in either team list.
        let team_ids: std::collections::HashSet<_> =
            team_a_ids.iter().chain(team_b_ids.iter()).copied().collect();
        for pid in result.scores.keys() {
            if !team_ids.contains(pid) {
                write_row(&mut out, pid, 0);
            }
        }
    }

    out
}

/// Parse a results CSV (as produced by `export_results`) back into structured session data.
///
/// `# key: value` metadata lines are extracted before CSV parsing so that player names
/// containing commas are handled correctly.  If no `# player:` metadata lines are
/// present (e.g., an older export), the player list is inferred from the data rows.
pub fn import_results(csv: &str) -> CsvResult<ImportedResults> {
    let mut sport: Option<String> = None;
    let mut team_size: Option<u8> = None;
    let mut scheduling_frequency: Option<u8> = None;
    let mut match_duration_minutes: Option<u16> = None;
    let mut players: Vec<String> = Vec::new();
    let mut data_lines: Vec<&str> = Vec::new();

    // Split metadata from CSV data before parsing so that commas in player
    // names (on `# player:` lines) don't confuse the RFC4180 parser.
    for line in csv.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let meta = trimmed.trim_start_matches('#').trim();
            if let Some(val) = meta.strip_prefix("sport:") {
                sport = Some(val.trim().to_string());
            } else if let Some(val) = meta.strip_prefix("team_size:") {
                team_size = val.trim().parse().ok();
            } else if let Some(val) = meta.strip_prefix("scheduling_frequency:") {
                scheduling_frequency = val.trim().parse().ok();
            } else if let Some(val) = meta.strip_prefix("match_duration:") {
                match_duration_minutes = val.trim().parse().ok();
            } else if let Some(val) = meta.strip_prefix("player:") {
                let name = val.trim();
                if !name.is_empty() {
                    players.push(name.to_string());
                }
            }
        } else {
            data_lines.push(line);
        }
    }

    let data_csv = data_lines.join("\n");
    let records = parse_csv_records(&data_csv)?;

    // Detect column positions from the header row.
    let mut col_match_id = 0usize;
    let mut col_round = 1usize;
    let mut col_team = 2usize;
    let mut col_player = 3usize;
    let mut col_goals = 4usize;
    let mut col_dnp = 5usize;
    let mut col_duration = 6usize;
    let mut data_start = 0usize;

    if let Some(header) = records.first() {
        if header
            .first()
            .map(|f| f.trim().eq_ignore_ascii_case("match_id"))
            .unwrap_or(false)
        {
            col_match_id = header_index(header, &["match_id"]).unwrap_or(0);
            col_round = header_index(header, &["round"]).unwrap_or(1);
            col_team = header_index(header, &["team"]).unwrap_or(2);
            col_player = header_index(header, &["player"]).unwrap_or(3);
            col_goals = header_index(header, &["goals"]).unwrap_or(4);
            col_dnp = header_index(header, &["did_not_play", "dnp"]).unwrap_or(5);
            col_duration =
                header_index(header, &["duration_multiplier", "duration"]).unwrap_or(6);
            data_start = 1;
        }
    }

    struct FlatRow {
        match_id: String,
        round: u32,
        team: u8,
        player: String,
        goals: Option<u16>,
        duration: f64,
    }

    let mut flat: Vec<FlatRow> = Vec::new();
    for row in records.iter().skip(data_start) {
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let match_id_str = row
            .get(col_match_id)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if match_id_str.is_empty() {
            continue;
        }
        let player = row
            .get(col_player)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if player.is_empty() {
            continue;
        }
        let round: u32 = row
            .get(col_round)
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1);
        let team: u8 = row
            .get(col_team)
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let dnp = row
            .get(col_dnp)
            .map(|s| s.trim().eq_ignore_ascii_case("yes"))
            .unwrap_or(false);
        let goals: Option<u16> = if dnp {
            None
        } else {
            row.get(col_goals)
                .and_then(|s| s.trim().parse().ok())
                .or(Some(0))
        };
        let duration: f64 = row
            .get(col_duration)
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1.0);

        flat.push(FlatRow {
            match_id: match_id_str,
            round,
            team,
            player,
            goals,
            duration,
        });
    }

    if flat.is_empty() && players.is_empty() {
        return Err(CsvError::Format(
            "No session data found in CSV".to_string(),
        ));
    }

    // Group rows by match_id, preserving insertion order.
    let mut seen_ids: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, row) in flat.iter().enumerate() {
        if !groups.contains_key(&row.match_id) {
            seen_ids.push(row.match_id.clone());
        }
        groups.entry(row.match_id.clone()).or_default().push(i);
    }

    let mut imported_matches: Vec<RawImportedMatch> = Vec::new();
    for mid in &seen_ids {
        let indices = &groups[mid];
        let round = flat[indices[0]].round;
        let duration = flat[indices[0]].duration;
        let mut team_a: Vec<(String, Option<u16>)> = Vec::new();
        let mut team_b: Vec<(String, Option<u16>)> = Vec::new();
        for &idx in indices {
            let row = &flat[idx];
            let entry = (row.player.clone(), row.goals);
            if row.team == 0 {
                team_a.push(entry);
            } else {
                team_b.push(entry);
            }
        }
        imported_matches.push(RawImportedMatch {
            round,
            team_a,
            team_b,
            duration_multiplier: duration,
        });
    }

    // If no metadata player list, fall back to unique names from data rows.
    if players.is_empty() {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for row in &flat {
            if seen.insert(row.player.clone()) {
                players.push(row.player.clone());
            }
        }
    }

    Ok(ImportedResults {
        sport,
        team_size,
        scheduling_frequency,
        match_duration_minutes,
        players,
        matches: imported_matches,
    })
}

/// Parse a rankings CSV (as produced by `export_rankings`) back into a list of
/// (player_name, PlayerRanking) pairs with synthetic sequential PlayerIds.
pub fn import_rankings(csv: &str) -> CsvResult<Vec<(String, crate::models::PlayerRanking)>> {
    use crate::models::{PlayerId, PlayerRanking};

    let records = parse_csv_records(csv)?;
    if records.is_empty() {
        return Err(CsvError::Format("No data rows found".to_string()));
    }

    let mut start_idx = 0usize;
    let mut cols = RankingCols::defaults();
    if records[0]
        .first()
        .map(|f| f.trim().eq_ignore_ascii_case("rank"))
        .unwrap_or(false)
    {
        cols = parse_rankings_columns(&records[0])?;
        start_idx = 1;
    }

    let mut out = Vec::new();
    let mut synthetic_id = 1u32;

    for (i, row) in records.iter().enumerate().skip(start_idx) {
        let row_num = i + 1;
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        if row
            .first()
            .map(|c| c.trim().starts_with('#'))
            .unwrap_or(false)
        {
            continue;
        }

        let rank: u32 = parse_required(row, cols.rank, "rank", row_num)?
            .parse()
            .map_err(|_| CsvError::Format(format!("bad rank at row {row_num}")))?;
        let name = parse_required(row, cols.name, "name", row_num)?.to_string();
        let rating: f64 = parse_required(row, cols.rating, "rating", row_num)?
            .parse()
            .map_err(|_| CsvError::Format(format!("bad rating at row {row_num}")))?;
        let uncertainty: f64 = parse_required(row, cols.uncertainty, "uncertainty", row_num)?
            .parse()
            .map_err(|_| CsvError::Format(format!("bad uncertainty at row {row_num}")))?;
        let rank_lo: u32 = parse_required(row, cols.rank_lo, "rank_lo_90", row_num)?
            .parse()
            .map_err(|_| CsvError::Format(format!("bad rank_lo_90 at row {row_num}")))?;
        let rank_hi: u32 = parse_required(row, cols.rank_hi, "rank_hi_90", row_num)?
            .parse()
            .map_err(|_| CsvError::Format(format!("bad rank_hi_90 at row {row_num}")))?;
        let matches_played: u32 =
            parse_required(row, cols.matches_played, "matches_played", row_num)?
                .parse()
                .map_err(|_| CsvError::Format(format!("bad matches_played at row {row_num}")))?;
        let total_goals: u32 = parse_required(row, cols.total_goals, "total_goals", row_num)?
            .parse()
            .map_err(|_| CsvError::Format(format!("bad total_goals at row {row_num}")))?;

        let prob_top_k: f64 = cols
            .prob_top_k
            .and_then(|idx| row.get(idx))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse()
                    .map_err(|_| CsvError::Format(format!("bad prob_top_k at row {row_num}")))
            })
            .transpose()?
            .unwrap_or(0.0);

        let is_active = match cols.active.and_then(|idx| row.get(idx)) {
            Some(raw) => parse_active(raw, row_num)?,
            None => true,
        };

        out.push((
            name,
            PlayerRanking {
                player_id: PlayerId(synthetic_id),
                rating,
                uncertainty,
                rank,
                rank_range_90: (rank_lo, rank_hi),
                matches_played,
                total_goals,
                prob_top_k,
                is_active,
            },
        ));
        synthetic_id += 1;
    }

    if out.is_empty() {
        return Err(CsvError::Format("No data rows found".to_string()));
    }
    Ok(out)
}

/// Parse a simple player roster CSV: one name per row, or `id,name`.
pub fn import_players(csv: &str) -> CsvResult<Vec<String>> {
    let mut names = Vec::new();
    for row in parse_csv_records(csv)? {
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        if row
            .first()
            .map(|c| c.trim().starts_with('#'))
            .unwrap_or(false)
        {
            continue;
        }
        let raw = if row.len() >= 2 { &row[1] } else { &row[0] };
        let name = raw.trim();
        if !name.is_empty() {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{PlayerId, PlayerStatus, RoundNumber};

    #[test]
    fn rankings_csv_round_trip_handles_quoted_names() {
        let players = vec![
            Player {
                id: PlayerId(1),
                name: "Doe, \"Jane\"".to_string(),
                status: PlayerStatus::Active,
                joined_at_round: RoundNumber(1),
                deactivated_at_round: None,
            },
            Player {
                id: PlayerId(2),
                name: "Line\nBreak".to_string(),
                status: PlayerStatus::Active,
                joined_at_round: RoundNumber(1),
                deactivated_at_round: None,
            },
        ];
        let rankings = vec![
            PlayerRanking {
                player_id: PlayerId(1),
                rating: 1.23,
                uncertainty: 0.45,
                rank: 1,
                rank_range_90: (1, 2),
                matches_played: 5,
                total_goals: 8,
                prob_top_k: 0.8,
                is_active: true,
            },
            PlayerRanking {
                player_id: PlayerId(2),
                rating: -0.5,
                uncertainty: 0.9,
                rank: 2,
                rank_range_90: (1, 2),
                matches_played: 5,
                total_goals: 2,
                prob_top_k: 0.2,
                is_active: false,
            },
        ];

        let csv = export_rankings(&rankings, &players);
        let imported = import_rankings(&csv).expect("import succeeds");
        assert_eq!(imported.len(), 2);
        assert_eq!(imported[0].0, "Doe, \"Jane\"");
        assert_eq!(imported[1].0, "Line\nBreak");
        assert_eq!(imported[0].1.rank, 1);
        assert_eq!(imported[1].1.rank, 2);
    }

    #[test]
    fn import_players_supports_quoted_names_and_id_name_rows() {
        let csv = "1,\"Doe, Jane\"\n\"Roe, John\"\n# comment\n2,Alice\n";
        let names = import_players(csv).expect("player import succeeds");
        assert_eq!(names, vec!["Doe, Jane", "Roe, John", "Alice"]);
    }
}
