//! CSV export and import for session data.
//!
//! Coaches can export results, rankings, and player stats as CSV for
//! offline analysis or migration. The Analysis tab can load from CSV
//! independently of a live session.
//!
//! Phase 7 will implement full import/export. This stub defines the interface.

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

fn escape_csv_field(raw: &str) -> String {
    if raw.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", raw.replace('"', "\"\""))
    } else {
        raw.to_string()
    }
}

/// Export rankings to CSV string.
pub fn export_rankings(rankings: &[PlayerRanking], players: &[Player]) -> String {
    let mut out = String::from("rank,name,rating,uncertainty,rank_lo_90,rank_hi_90,matches_played,total_goals,prob_top_k,active\n");
    let name_map: std::collections::HashMap<u32, &str> =
        players.iter().map(|p| (p.id.0, p.name.as_str())).collect();

    let mut sorted: Vec<&PlayerRanking> = rankings.iter().collect();
    sorted.sort_by_key(|r| r.rank);

    for r in sorted {
        let name = name_map.get(&r.player_id.0).copied().unwrap_or("?");
        out.push_str(&format!(
            "{},{},{:.4},{:.4},{},{},{},{},{:.4},{}\n",
            r.rank,
            escape_csv_field(name),
            r.rating,
            r.uncertainty,
            r.rank_range_90.0,
            r.rank_range_90.1,
            r.matches_played,
            r.total_goals,
            r.prob_top_k,
            if r.is_active { "yes" } else { "no" },
        ));
    }
    out
}

/// Export match results to CSV string.
pub fn export_results(results: &[&MatchResult], players: &[Player]) -> String {
    let name_map: std::collections::HashMap<u32, &str> =
        players.iter().map(|p| (p.id.0, p.name.as_str())).collect();

    let mut out = String::from("match_id,player,goals,did_not_play,duration_multiplier\n");
    for result in results {
        for (player_id, score) in &result.scores {
            let name = name_map.get(&player_id.0).copied().unwrap_or("?");
            out.push_str(&format!(
                "{},{},{},{},{}\n",
                result.match_id.0,
                escape_csv_field(name),
                score.goals.unwrap_or(0),
                if score.goals.is_none() { "yes" } else { "no" },
                result.duration_multiplier,
            ));
        }
    }
    out
}

/// Parse a rankings CSV (as produced by `export_rankings`) back into a list of
/// (player_name, PlayerRanking) pairs with synthetic sequential PlayerIds.
///
/// Header row is detected by the presence of "rank" in the first field.
pub fn import_rankings(csv: &str) -> CsvResult<Vec<(String, crate::models::PlayerRanking)>> {
    use crate::models::{PlayerId, PlayerRanking};

    let mut out = Vec::new();
    let mut synthetic_id = 1u32;

    for line in csv.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Skip header row
        if line.to_ascii_lowercase().starts_with("rank") {
            continue;
        }
        let cols: Vec<&str> = line.splitn(10, ',').collect();
        if cols.len() < 8 {
            continue; // skip malformed lines
        }
        let rank: u32 = cols[0]
            .trim()
            .parse()
            .map_err(|_| CsvError::Format(format!("bad rank: {}", cols[0])))?;
        let name = cols[1].trim().to_string();
        let rating: f64 = cols[2]
            .trim()
            .parse()
            .map_err(|_| CsvError::Format(format!("bad rating: {}", cols[2])))?;
        let uncertainty: f64 = cols[3]
            .trim()
            .parse()
            .map_err(|_| CsvError::Format(format!("bad uncertainty: {}", cols[3])))?;
        let rank_lo: u32 = cols[4]
            .trim()
            .parse()
            .map_err(|_| CsvError::Format(format!("bad rank_lo: {}", cols[4])))?;
        let rank_hi: u32 = cols[5]
            .trim()
            .parse()
            .map_err(|_| CsvError::Format(format!("bad rank_hi: {}", cols[5])))?;
        let matches_played: u32 = cols[6]
            .trim()
            .parse()
            .map_err(|_| CsvError::Format(format!("bad matches_played: {}", cols[6])))?;
        let total_goals: u32 = cols[7]
            .trim()
            .parse()
            .map_err(|_| CsvError::Format(format!("bad total_goals: {}", cols[7])))?;
        let prob_top_k: f64 = if cols.len() > 8 {
            cols[8].trim().parse().unwrap_or(0.0)
        } else {
            0.0
        };
        let is_active = cols.get(9).map(|s| s.trim() != "no").unwrap_or(true);

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

/// Parse a simple player roster CSV: one name per line (or "id,name" format).
pub fn import_players(csv: &str) -> CsvResult<Vec<String>> {
    let mut names = Vec::new();
    for line in csv.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Support "id,name" or just "name"
        let name = if let Some((_, name)) = line.split_once(',') {
            name.trim().to_string()
        } else {
            line.to_string()
        };
        if name.is_empty() {
            continue;
        }
        names.push(name);
    }
    Ok(names)
}
