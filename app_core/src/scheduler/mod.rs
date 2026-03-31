pub mod info_max;
pub(crate) mod match_candidate;
pub mod round_robin;

use crate::models::{Player, PlayerId, PlayerRanking, RoundNumber, ScheduledMatch, SessionConfig};
use crate::rng::SessionRng;
use std::collections::{HashMap, HashSet};

pub struct ScheduleGenerationRequest<'a> {
    pub players: &'a [Player],
    pub rankings: &'a [PlayerRanking],
    pub existing_matches: &'a [&'a ScheduledMatch],
    pub config: &'a SessionConfig,
    pub rng: &'a mut SessionRng,
    pub starting_round: RoundNumber,
    pub num_rounds: u32,
}

/// Pluggable match scheduling algorithm.
pub trait Scheduler {
    /// Generate `num_rounds` worth of match schedules.
    ///
    /// `players` contains only active players.
    /// `rankings` may be empty (cold start → round-robin).
    fn generate_schedule(&self, request: ScheduleGenerationRequest<'_>) -> Vec<ScheduledMatch>;
}

/// Factory: select the appropriate scheduler based on session state.
/// - No rankings yet (cold start): round-robin for balanced initial matchups.
/// - Rankings available: info-maximizing scheduler.
pub fn select_scheduler(rankings: &[PlayerRanking]) -> Box<dyn Scheduler> {
    if rankings.is_empty() || rankings.iter().all(|r| r.matches_played == 0) {
        Box::new(round_robin::RoundRobinScheduler)
    } else {
        Box::new(info_max::InfoMaxScheduler)
    }
}

/// How many fields are needed for a given player count and team size?
pub fn fields_needed(player_count: usize, team_size: usize) -> usize {
    let players_per_match = 2 * team_size;
    player_count / players_per_match
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BenchFairnessStats {
    consecutive_bench_rounds: u32,
    total_bench_rounds: u32,
}

pub(crate) struct BenchBalanceTracker {
    stats_by_player_id: HashMap<PlayerId, BenchFairnessStats>,
}

impl BenchBalanceTracker {
    pub(crate) fn from_history(
        players: &[Player],
        existing_matches: &[&ScheduledMatch],
        round_before_generation: RoundNumber,
    ) -> Self {
        let mut stats_by_player_id: HashMap<PlayerId, BenchFairnessStats> = players
            .iter()
            .map(|player| (player.id, BenchFairnessStats::default()))
            .collect();
        let tracked_player_ids: HashSet<PlayerId> = stats_by_player_id.keys().copied().collect();

        if tracked_player_ids.is_empty() {
            return Self { stats_by_player_id };
        }

        let mut scheduled_by_round: HashMap<RoundNumber, HashSet<PlayerId>> = HashMap::new();
        for scheduled_match in existing_matches {
            if scheduled_match.round >= round_before_generation {
                continue;
            }
            let round_players = scheduled_by_round.entry(scheduled_match.round).or_default();
            round_players.extend(
                scheduled_match
                    .team_a
                    .iter()
                    .chain(scheduled_match.team_b.iter())
                    .filter(|player_id| tracked_player_ids.contains(player_id))
                    .copied(),
            );
        }

        let mut historical_rounds: Vec<RoundNumber> = scheduled_by_round.keys().copied().collect();
        historical_rounds.sort_unstable();

        for round in historical_rounds {
            let scheduled_player_ids = scheduled_by_round.get(&round).cloned().unwrap_or_default();
            for player in players {
                if player.joined_at_round > round {
                    continue;
                }
                if scheduled_player_ids.contains(&player.id) {
                    if let Some(stats) = stats_by_player_id.get_mut(&player.id) {
                        stats.consecutive_bench_rounds = 0;
                    }
                } else if let Some(stats) = stats_by_player_id.get_mut(&player.id) {
                    stats.total_bench_rounds += 1;
                    stats.consecutive_bench_rounds += 1;
                }
            }
        }

        Self { stats_by_player_id }
    }

    pub(crate) fn choose_benched_player_ids(
        &self,
        ordered_player_ids: &[PlayerId],
        bench_count: usize,
    ) -> HashSet<PlayerId> {
        if bench_count == 0 {
            return HashSet::new();
        }

        let mut candidates: Vec<(usize, PlayerId, BenchFairnessStats)> = ordered_player_ids
            .iter()
            .enumerate()
            .map(|(index, player_id)| {
                (
                    index,
                    *player_id,
                    self.stats_by_player_id
                        .get(player_id)
                        .copied()
                        .unwrap_or_default(),
                )
            })
            .collect();
        candidates.sort_by_key(|(index, _player_id, stats)| {
            (
                stats.consecutive_bench_rounds,
                stats.total_bench_rounds,
                std::cmp::Reverse(*index),
            )
        });

        candidates
            .into_iter()
            .take(bench_count)
            .map(|(_, player_id, _)| player_id)
            .collect()
    }

    pub(crate) fn record_round(
        &mut self,
        eligible_player_ids: &[PlayerId],
        scheduled_player_ids: &HashSet<PlayerId>,
    ) {
        for player_id in eligible_player_ids {
            let Some(stats) = self.stats_by_player_id.get_mut(player_id) else {
                continue;
            };
            if scheduled_player_ids.contains(player_id) {
                stats.consecutive_bench_rounds = 0;
            } else {
                stats.total_bench_rounds += 1;
                stats.consecutive_bench_rounds += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MatchId, MatchStatus, PlayerStatus, Sport};

    fn make_player(id: u32) -> Player {
        Player {
            id: PlayerId(id),
            name: format!("P{id}"),
            status: PlayerStatus::Active,
            joined_at_round: RoundNumber(1),
            deactivated_at_round: None,
        }
    }

    #[test]
    fn bench_selection_avoids_back_to_back_when_possible() {
        let players = vec![
            make_player(1),
            make_player(2),
            make_player(3),
            make_player(4),
            make_player(5),
        ];
        let prior_round = ScheduledMatch {
            id: MatchId(1000),
            round: RoundNumber(1),
            field: 1,
            team_a: vec![PlayerId(1), PlayerId(2)],
            team_b: vec![PlayerId(3), PlayerId(4)],
            status: MatchStatus::Completed,
        };
        let tracker = BenchBalanceTracker::from_history(&players, &[&prior_round], RoundNumber(2));

        let benched = tracker.choose_benched_player_ids(
            &players.iter().map(|player| player.id).collect::<Vec<_>>(),
            1,
        );

        assert_eq!(benched.len(), 1);
        assert!(!benched.contains(&PlayerId(5)));
    }

    #[test]
    fn fields_needed_still_counts_only_full_matches() {
        assert_eq!(fields_needed(10, 2), 2);
        assert_eq!(fields_needed(5, 2), 1);
        assert_eq!(SessionConfig::new(2, 1, Sport::Soccer).team_size, 2);
    }
}
