//! CSV export and import for session data.
//!
//! Supports RFC4180-style quoted fields, escaped quotes, and embedded newlines.

use crate::models::{MatchResult, Player, PlayerRanking};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CsvError {
    #[error("CSV format error: {0}")]
    Format(String),
    #[error("Missing column: {0}")]
    MissingColumn(String),
}

pub type CsvResult<T> = Result<T, CsvError>;

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

/// Export match results to CSV string.
pub fn export_results(results: &[&MatchResult], players: &[Player]) -> String {
    let mut out = String::new();
    write_csv_record(
        &mut out,
        &[
            "match_id".to_string(),
            "player".to_string(),
            "goals".to_string(),
            "did_not_play".to_string(),
            "duration_multiplier".to_string(),
        ],
    );

    let name_map: std::collections::HashMap<u32, &str> =
        players.iter().map(|p| (p.id.0, p.name.as_str())).collect();

    for result in results {
        for (player_id, score) in &result.scores {
            let name = name_map.get(&player_id.0).copied().unwrap_or("?");
            write_csv_record(
                &mut out,
                &[
                    result.match_id.0.to_string(),
                    name.to_string(),
                    score.goals.unwrap_or(0).to_string(),
                    if score.goals.is_none() { "yes" } else { "no" }.to_string(),
                    result.duration_multiplier.to_string(),
                ],
            );
        }
    }
    out
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
