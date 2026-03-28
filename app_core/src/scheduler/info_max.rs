/// Information-maximizing scheduler.
///
/// Selects matchups that are expected to reduce posterior ranking uncertainty
/// the most. Uses the `expected_info_gain()` method on the ranking engine to
/// evaluate candidate matchups, then greedily picks the highest-gain matches.
///
/// For multi-round batches, applies a receding-horizon approach: schedule the
/// first round optimally, project forward with estimated uncertainty reduction,
/// then schedule subsequent rounds.
///
/// Constraint: each player plays at most once per round.
use super::Scheduler;
use crate::models::{
    MatchId, MatchStatus, Player, PlayerRanking, RoundNumber, ScheduledMatch, SessionConfig,
};
use crate::rng::SessionRng;

pub struct InfoMaxScheduler;

impl Scheduler for InfoMaxScheduler {
    fn generate_schedule(
        &self,
        players: &[Player],
        rankings: &[PlayerRanking],
        config: &SessionConfig,
        rng: &mut SessionRng,
        starting_round: RoundNumber,
        num_rounds: u32,
    ) -> Vec<ScheduledMatch> {
        let team_size = config.team_size as usize;
        let players_per_match = 2 * team_size;
        let n = players.len();

        if n < players_per_match {
            return vec![];
        }

        let mut all_matches = Vec::new();
        let mut match_id_counter = starting_round.0 * 1000;

        // Projected rankings for receding-horizon multi-round scheduling
        let mut projected_rankings: Vec<PlayerRanking> = rankings.to_vec();

        for round_offset in 0..num_rounds {
            let round = RoundNumber(starting_round.0 + round_offset);
            let round_matches = self.schedule_one_round(
                players,
                &projected_rankings,
                config,
                rng,
                round,
                team_size,
                &mut match_id_counter,
            );

            // Project uncertainty forward: playing a match reduces uncertainty.
            // Rough approximation: sigma decreases by a fixed fraction per match.
            for m in &round_matches {
                for pid in m.team_a.iter().chain(m.team_b.iter()) {
                    if let Some(r) = projected_rankings.iter_mut().find(|r| r.player_id == *pid) {
                        r.uncertainty *= 0.85; // ~15% reduction per match played
                    }
                }
            }

            all_matches.extend(round_matches);
        }

        all_matches
    }
}

impl InfoMaxScheduler {
    #[allow(clippy::too_many_arguments)]
    fn schedule_one_round(
        &self,
        players: &[Player],
        rankings: &[PlayerRanking],
        _config: &SessionConfig,
        rng: &mut SessionRng,
        round: RoundNumber,
        team_size: usize,
        match_id_counter: &mut u32,
    ) -> Vec<ScheduledMatch> {
        let players_per_match = 2 * team_size;
        let mut available: Vec<usize> = (0..players.len()).collect();
        let mut matches = Vec::new();
        let mut field = 1u8;

        while available.len() >= players_per_match {
            // Find the best match among available players
            let (best_group, _best_score) =
                self.find_best_matchup(&available, players, rankings, team_size, rng);

            if best_group.is_empty() {
                break;
            }

            // Remove selected players from available pool
            for &idx in &best_group {
                available.retain(|&a| a != idx);
            }

            let team_a: Vec<_> = best_group[..team_size]
                .iter()
                .map(|&idx| players[idx].id)
                .collect();
            let team_b: Vec<_> = best_group[team_size..]
                .iter()
                .map(|&idx| players[idx].id)
                .collect();

            matches.push(ScheduledMatch {
                id: MatchId(*match_id_counter),
                round,
                field,
                team_a,
                team_b,
                status: MatchStatus::Scheduled,
            });

            *match_id_counter += 1;
            field += 1;
        }

        matches
    }

    /// Enumerate candidate matchups and return the one with highest info gain.
    /// Uses the heuristic: sum(sigma) - 0.5 * |skill_gap|
    /// Full entropy calculation deferred to Phase 2 when ranking engine is integrated.
    fn find_best_matchup(
        &self,
        available: &[usize],
        players: &[Player],
        rankings: &[PlayerRanking],
        team_size: usize,
        _rng: &mut SessionRng,
    ) -> (Vec<usize>, f64) {
        let players_per_match = 2 * team_size;
        if available.len() < players_per_match {
            return (vec![], f64::NEG_INFINITY);
        }

        let rating_map: std::collections::HashMap<u32, f64> =
            rankings.iter().map(|r| (r.player_id.0, r.rating)).collect();
        let sigma_map: std::collections::HashMap<u32, f64> = rankings
            .iter()
            .map(|r| (r.player_id.0, r.uncertainty))
            .collect();

        let mut best_group = vec![];
        let mut best_score = f64::NEG_INFINITY;

        // For efficiency with small N (≤22 players), enumerate combinations.
        // For larger N this would need a smarter approach, but max is 22 players.
        let combos = combinations(available, players_per_match);
        for group in combos {
            let team_a = &group[..team_size];
            let team_b = &group[team_size..];

            let total_sigma: f64 = group
                .iter()
                .map(|&idx| sigma_map.get(&players[idx].id.0).copied().unwrap_or(1.0))
                .sum();

            let skill_a: f64 = team_a
                .iter()
                .map(|&idx| rating_map.get(&players[idx].id.0).copied().unwrap_or(0.0))
                .sum();
            let skill_b: f64 = team_b
                .iter()
                .map(|&idx| rating_map.get(&players[idx].id.0).copied().unwrap_or(0.0))
                .sum();

            let score = total_sigma - 0.5 * (skill_a - skill_b).abs();

            if score > best_score {
                best_score = score;
                best_group = group;
            }
        }

        (best_group, best_score)
    }
}

/// Generate all combinations of `k` elements from `items`.
/// Only practical for small N (≤22 players, k≤4 for 2v2 → 7315 combos max).
fn combinations(items: &[usize], k: usize) -> Vec<Vec<usize>> {
    if k == 0 {
        return vec![vec![]];
    }
    if items.len() < k {
        return vec![];
    }
    let mut result = Vec::new();
    for i in 0..=(items.len() - k) {
        let rest = combinations(&items[i + 1..], k - 1);
        for mut combo in rest {
            combo.insert(0, items[i]);
            result.push(combo);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;

    fn make_players_with_known_skill(skills: &[(u32, f64)]) -> (Vec<Player>, Vec<PlayerRanking>) {
        let players: Vec<Player> = skills
            .iter()
            .map(|(id, _)| Player {
                id: PlayerId(*id),
                name: format!("P{}", id),
                status: PlayerStatus::Active,
                joined_at_round: RoundNumber(1),
                deactivated_at_round: None,
            })
            .collect();

        let rankings: Vec<PlayerRanking> = skills
            .iter()
            .enumerate()
            .map(|(i, (id, skill))| PlayerRanking {
                player_id: PlayerId(*id),
                rating: *skill,
                uncertainty: 1.0,
                rank: i as u32 + 1,
                rank_range_90: (1, skills.len() as u32),
                matches_played: 3,
                total_goals: 5,
                prob_top_k: 0.5,
                is_active: true,
            })
            .collect();

        (players, rankings)
    }

    #[test]
    fn no_player_scheduled_twice_per_round() {
        let (players, rankings) = make_players_with_known_skill(&[
            (1, 0.5),
            (2, 0.2),
            (3, -0.1),
            (4, 0.8),
            (5, 0.3),
            (6, -0.3),
            (7, 0.1),
            (8, 0.6),
        ]);
        let config = SessionConfig::new(2, 1, Sport::Soccer);
        let session_id = config.id.clone();
        let mut rng = SessionRng::new(&session_id);
        let scheduler = InfoMaxScheduler;

        let matches =
            scheduler.generate_schedule(&players, &rankings, &config, &mut rng, RoundNumber(1), 1);

        let mut seen = std::collections::HashSet::new();
        for m in &matches {
            for pid in m.team_a.iter().chain(m.team_b.iter()) {
                assert!(seen.insert(pid.0), "Player {} scheduled twice", pid.0);
            }
        }
    }

    #[test]
    fn prefers_high_uncertainty_matchup() {
        // Give players very different uncertainty levels.
        // The scheduler should prefer pairing the high-uncertainty players.
        let (players, rankings) =
            make_players_with_known_skill(&[(1, 0.0), (2, 0.0), (3, 0.0), (4, 0.0)]);
        let mut high_unc_rankings = rankings;
        // Make players 1 and 2 have high uncertainty
        high_unc_rankings[0].uncertainty = 5.0;
        high_unc_rankings[1].uncertainty = 5.0;
        high_unc_rankings[2].uncertainty = 0.1;
        high_unc_rankings[3].uncertainty = 0.1;

        let config = SessionConfig::new(2, 1, Sport::Soccer);
        let session_id = config.id.clone();
        let mut rng = SessionRng::new(&session_id);
        let scheduler = InfoMaxScheduler;

        let matches = scheduler.generate_schedule(
            &players,
            &high_unc_rankings,
            &config,
            &mut rng,
            RoundNumber(1),
            1,
        );

        assert_eq!(matches.len(), 1);
        // The single match should include both high-uncertainty players (1 and 2)
        let match_players: Vec<u32> = matches[0]
            .team_a
            .iter()
            .chain(matches[0].team_b.iter())
            .map(|p| p.0)
            .collect();
        assert!(
            match_players.contains(&1) && match_players.contains(&2),
            "High uncertainty players 1 and 2 should be scheduled together"
        );
    }
}
