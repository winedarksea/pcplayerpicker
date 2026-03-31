use super::match_candidate::{
    build_ranking_snapshot_by_player_id, evaluate_candidate_match_score, player_ids_for_indices,
    visit_unique_team_partitions, MatchExposureHistory,
};
use super::BenchBalanceTracker;
/// Information-maximizing scheduler.
///
/// Selects matchups that are expected to reduce ranking uncertainty while also
/// avoiding repeated teammate/opponent exposures. For each candidate group it
/// evaluates every unique team partition, then scores the best split using:
///   1. predictive variance from current player uncertainties
///   2. outcome-balance entropy (close matches teach more)
///   3. repeat-exposure penalties from prior rounds
///
/// For multi-round batches, the scheduler updates a projected exposure history
/// and shrinks player uncertainties more for informative, novel matches.
use super::{ScheduleGenerationRequest, Scheduler};
use crate::models::{
    MatchId, MatchStatus, Player, PlayerId, PlayerRanking, RoundNumber, ScheduledMatch,
};
use crate::rng::SessionRng;
use std::collections::HashSet;

pub struct InfoMaxScheduler;

struct PlannedScheduledMatch {
    scheduled_match: ScheduledMatch,
    projected_uncertainty_multiplier: f64,
}

struct SelectedMatchup {
    selected_group_indices: Vec<usize>,
    team_a: Vec<PlayerId>,
    team_b: Vec<PlayerId>,
    total_score: f64,
    projected_uncertainty_multiplier: f64,
}

impl Scheduler for InfoMaxScheduler {
    fn generate_schedule(&self, request: ScheduleGenerationRequest<'_>) -> Vec<ScheduledMatch> {
        let ScheduleGenerationRequest {
            players,
            rankings,
            existing_matches,
            config,
            rng,
            starting_round,
            num_rounds,
        } = request;
        let team_size = config.team_size as usize;
        let players_per_match = 2 * team_size;

        if players.len() < players_per_match {
            return vec![];
        }

        let mut all_matches = Vec::new();
        let mut match_id_counter = starting_round.0 * 1000;
        let mut projected_rankings = rankings.to_vec();
        let mut projected_exposure_history = MatchExposureHistory::from_matches(existing_matches);
        let mut bench_balance_tracker =
            BenchBalanceTracker::from_history(players, existing_matches, starting_round);
        let eligible_player_ids: Vec<PlayerId> = players.iter().map(|player| player.id).collect();

        for round_offset in 0..num_rounds {
            let round = RoundNumber(starting_round.0 + round_offset);
            let round_matches = self.schedule_one_round(
                players,
                &projected_rankings,
                rng,
                round,
                team_size,
                &mut match_id_counter,
                &projected_exposure_history,
                &bench_balance_tracker,
            );

            for planned_match in &round_matches {
                for player_id in planned_match
                    .scheduled_match
                    .team_a
                    .iter()
                    .chain(planned_match.scheduled_match.team_b.iter())
                {
                    if let Some(ranking) = projected_rankings
                        .iter_mut()
                        .find(|ranking| ranking.player_id == *player_id)
                    {
                        ranking.uncertainty *= planned_match.projected_uncertainty_multiplier;
                    }
                }
                projected_exposure_history.record_match(
                    planned_match.scheduled_match.team_a.as_slice(),
                    planned_match.scheduled_match.team_b.as_slice(),
                    round,
                );
            }
            let scheduled_player_ids: HashSet<_> = round_matches
                .iter()
                .flat_map(|planned_match| {
                    planned_match
                        .scheduled_match
                        .team_a
                        .iter()
                        .chain(planned_match.scheduled_match.team_b.iter())
                        .copied()
                })
                .collect();
            bench_balance_tracker.record_round(&eligible_player_ids, &scheduled_player_ids);

            all_matches.extend(
                round_matches
                    .into_iter()
                    .map(|planned_match| planned_match.scheduled_match),
            );
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
        _rng: &mut SessionRng,
        round: RoundNumber,
        team_size: usize,
        match_id_counter: &mut u32,
        exposure_history: &MatchExposureHistory,
        bench_balance_tracker: &BenchBalanceTracker,
    ) -> Vec<PlannedScheduledMatch> {
        let players_per_match = 2 * team_size;
        let all_player_ids: Vec<PlayerId> = players.iter().map(|player| player.id).collect();
        let bench_count = players.len() % players_per_match;
        let benched_player_ids =
            bench_balance_tracker.choose_benched_player_ids(&all_player_ids, bench_count);
        let mut available_player_indices: Vec<usize> = players
            .iter()
            .enumerate()
            .filter_map(|(index, player)| {
                (!benched_player_ids.contains(&player.id)).then_some(index)
            })
            .collect();
        let mut matches = Vec::new();
        let mut field = 1u8;
        let ranking_snapshot_by_player_id = build_ranking_snapshot_by_player_id(rankings);

        while available_player_indices.len() >= players_per_match {
            let Some(best_matchup) = self.find_best_matchup(
                available_player_indices.as_slice(),
                players,
                team_size,
                &ranking_snapshot_by_player_id,
                exposure_history,
                round,
            ) else {
                break;
            };

            for selected_player_index in &best_matchup.selected_group_indices {
                available_player_indices
                    .retain(|available_index| available_index != selected_player_index);
            }

            matches.push(PlannedScheduledMatch {
                scheduled_match: ScheduledMatch {
                    id: MatchId(*match_id_counter),
                    round,
                    field,
                    team_a: best_matchup.team_a,
                    team_b: best_matchup.team_b,
                    status: MatchStatus::Scheduled,
                },
                projected_uncertainty_multiplier: best_matchup.projected_uncertainty_multiplier,
            });

            *match_id_counter += 1;
            field += 1;
        }

        matches
    }

    fn find_best_matchup(
        &self,
        available_player_indices: &[usize],
        players: &[Player],
        team_size: usize,
        ranking_snapshot_by_player_id: &std::collections::HashMap<
            PlayerId,
            super::match_candidate::RankingSnapshot,
        >,
        exposure_history: &MatchExposureHistory,
        round: RoundNumber,
    ) -> Option<SelectedMatchup> {
        let players_per_match = 2 * team_size;
        if available_player_indices.len() < players_per_match {
            return None;
        }

        let mut best_matchup: Option<SelectedMatchup> = None;

        for selected_group_indices in combinations(available_player_indices, players_per_match) {
            visit_unique_team_partitions(
                selected_group_indices.as_slice(),
                team_size,
                |team_a_indices, team_b_indices| {
                    let team_a = player_ids_for_indices(players, team_a_indices);
                    let team_b = player_ids_for_indices(players, team_b_indices);
                    let candidate_score = evaluate_candidate_match_score(
                        team_a.as_slice(),
                        team_b.as_slice(),
                        ranking_snapshot_by_player_id,
                        exposure_history,
                        round,
                    );

                    let should_replace = best_matchup
                        .as_ref()
                        .map(|current_best| candidate_score.total_score > current_best.total_score)
                        .unwrap_or(true);
                    if should_replace {
                        best_matchup = Some(SelectedMatchup {
                            selected_group_indices: selected_group_indices.clone(),
                            team_a,
                            team_b,
                            total_score: candidate_score.total_score,
                            projected_uncertainty_multiplier: candidate_score
                                .projected_uncertainty_multiplier,
                        });
                    }
                },
            );
        }

        best_matchup
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
    for item_index in 0..=(items.len() - k) {
        let rest = combinations(&items[item_index + 1..], k - 1);
        for mut combination in rest {
            combination.insert(0, items[item_index]);
            result.push(combination);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;
    use crate::rng::SessionRng;

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

        let matches = scheduler.generate_schedule(ScheduleGenerationRequest {
            players: &players,
            rankings: &rankings,
            existing_matches: &[],
            config: &config,
            rng: &mut rng,
            starting_round: RoundNumber(1),
            num_rounds: 1,
        });

        let mut seen = std::collections::HashSet::new();
        for scheduled_match in &matches {
            for player_id in scheduled_match
                .team_a
                .iter()
                .chain(scheduled_match.team_b.iter())
            {
                assert!(
                    seen.insert(player_id.0),
                    "Player {} scheduled twice",
                    player_id.0
                );
            }
        }
    }

    #[test]
    fn prefers_high_uncertainty_matchup() {
        let (players, rankings) =
            make_players_with_known_skill(&[(1, 0.0), (2, 0.0), (3, 0.0), (4, 0.0)]);
        let mut high_uncertainty_rankings = rankings;
        high_uncertainty_rankings[0].uncertainty = 5.0;
        high_uncertainty_rankings[1].uncertainty = 5.0;
        high_uncertainty_rankings[2].uncertainty = 0.1;
        high_uncertainty_rankings[3].uncertainty = 0.1;

        let config = SessionConfig::new(2, 1, Sport::Soccer);
        let session_id = config.id.clone();
        let mut rng = SessionRng::new(&session_id);
        let scheduler = InfoMaxScheduler;

        let matches = scheduler.generate_schedule(ScheduleGenerationRequest {
            players: &players,
            rankings: &high_uncertainty_rankings,
            existing_matches: &[],
            config: &config,
            rng: &mut rng,
            starting_round: RoundNumber(1),
            num_rounds: 1,
        });

        assert_eq!(matches.len(), 1);
        let match_players: Vec<u32> = matches[0]
            .team_a
            .iter()
            .chain(matches[0].team_b.iter())
            .map(|player_id| player_id.0)
            .collect();
        assert!(
            match_players.contains(&1) && match_players.contains(&2),
            "high-uncertainty players 1 and 2 should be scheduled together",
        );
    }

    #[test]
    fn balances_teams_within_same_group() {
        let (players, rankings) =
            make_players_with_known_skill(&[(1, 10.0), (2, 9.0), (3, 1.0), (4, 0.0)]);
        let config = SessionConfig::new(2, 1, Sport::Soccer);
        let session_id = config.id.clone();
        let mut rng = SessionRng::new(&session_id);
        let scheduler = InfoMaxScheduler;

        let matches = scheduler.generate_schedule(ScheduleGenerationRequest {
            players: &players,
            rankings: &rankings,
            existing_matches: &[],
            config: &config,
            rng: &mut rng,
            starting_round: RoundNumber(1),
            num_rounds: 1,
        });

        assert_eq!(matches.len(), 1);
        let team_a_total: f64 = matches[0]
            .team_a
            .iter()
            .map(|player_id| {
                rankings
                    .iter()
                    .find(|ranking| ranking.player_id == *player_id)
                    .unwrap()
                    .rating
            })
            .sum();
        let team_b_total: f64 = matches[0]
            .team_b
            .iter()
            .map(|player_id| {
                rankings
                    .iter()
                    .find(|ranking| ranking.player_id == *player_id)
                    .unwrap()
                    .rating
            })
            .sum();

        assert!(
            (team_a_total - team_b_total).abs() <= 2.0,
            "scheduler should choose a balanced split inside the same 4-player group",
        );
    }

    #[test]
    fn avoids_repeating_same_teammates_when_alternative_exists() {
        let (players, rankings) =
            make_players_with_known_skill(&[(1, 0.0), (2, 0.0), (3, 0.0), (4, 0.0)]);
        let config = SessionConfig::new(2, 1, Sport::Soccer);
        let session_id = config.id.clone();
        let mut rng = SessionRng::new(&session_id);
        let scheduler = InfoMaxScheduler;
        let prior_match = ScheduledMatch {
            id: MatchId(11),
            round: RoundNumber(1),
            field: 1,
            team_a: vec![PlayerId(1), PlayerId(2)],
            team_b: vec![PlayerId(3), PlayerId(4)],
            status: MatchStatus::Completed,
        };

        let matches = scheduler.generate_schedule(ScheduleGenerationRequest {
            players: &players,
            rankings: &rankings,
            existing_matches: &[&prior_match],
            config: &config,
            rng: &mut rng,
            starting_round: RoundNumber(2),
            num_rounds: 1,
        });

        assert_eq!(matches.len(), 1);
        let repeated_teammates = matches[0].team_a == prior_match.team_a
            || matches[0].team_a == prior_match.team_b
            || matches[0].team_b == prior_match.team_a
            || matches[0].team_b == prior_match.team_b;
        assert!(
            !repeated_teammates,
            "info-max should avoid identical team splits"
        );
    }

    #[test]
    fn uneven_player_batches_rotate_the_bench() {
        let (players, rankings) =
            make_players_with_known_skill(&[(1, 0.5), (2, 0.4), (3, 0.3), (4, 0.2), (5, 0.1)]);
        let config = SessionConfig::new(2, 3, Sport::Soccer);
        let session_id = config.id.clone();
        let mut rng = SessionRng::new(&session_id);
        let scheduler = InfoMaxScheduler;

        let matches = scheduler.generate_schedule(ScheduleGenerationRequest {
            players: &players,
            rankings: &rankings,
            existing_matches: &[],
            config: &config,
            rng: &mut rng,
            starting_round: RoundNumber(1),
            num_rounds: 3,
        });

        let mut benched_by_round = Vec::new();
        for round in 1..=3 {
            let scheduled_player_ids: std::collections::HashSet<_> = matches
                .iter()
                .filter(|scheduled_match| scheduled_match.round == RoundNumber(round))
                .flat_map(|scheduled_match| {
                    scheduled_match
                        .team_a
                        .iter()
                        .chain(scheduled_match.team_b.iter())
                        .copied()
                })
                .collect();
            let benched_player_id = players
                .iter()
                .map(|player| player.id)
                .find(|player_id| !scheduled_player_ids.contains(player_id))
                .expect("one player should sit out each round");
            benched_by_round.push(benched_player_id);
        }

        assert_ne!(benched_by_round[0], benched_by_round[1]);
        assert_ne!(benched_by_round[1], benched_by_round[2]);
    }
}
