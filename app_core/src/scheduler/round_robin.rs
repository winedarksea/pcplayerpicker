use super::match_candidate::{
    evaluate_candidate_match_score, player_ids_for_indices, visit_unique_team_partitions,
    MatchExposureHistory,
};
/// Round-robin scheduler for cold start (no ranking data yet).
///
/// Uses the circle algorithm to generate balanced pairings where every player
/// faces every other player an equal number of times over the full rotation.
/// Teams within each match are assigned by searching all unique partitions and
/// picking the one with the lowest repeat-exposure penalty.
use super::{ScheduleGenerationRequest, Scheduler};
use crate::models::{
    MatchId, MatchStatus, Player, RoundNumber, ScheduledMatch,
};
use rand::seq::SliceRandom;
use std::collections::HashMap;

pub struct RoundRobinScheduler;

impl Scheduler for RoundRobinScheduler {
    fn generate_schedule(&self, request: ScheduleGenerationRequest<'_>) -> Vec<ScheduledMatch> {
        let ScheduleGenerationRequest {
            players,
            rankings: _rankings,
            existing_matches,
            config,
            rng,
            starting_round,
            num_rounds,
        } = request;
        let team_size = config.team_size as usize;
        let players_per_match = 2 * team_size;
        let n = players.len();

        if n < players_per_match {
            return vec![];
        }

        let mut all_matches = Vec::new();
        let mut match_id_counter = starting_round.0 * 1000; // unique IDs per round block
        let mut projected_exposure_history = MatchExposureHistory::from_matches(existing_matches);
        let ranking_snapshot_by_player_id: HashMap<_, _> = players
            .iter()
            .map(|player| {
                (
                    player.id,
                    super::match_candidate::RankingSnapshot {
                        rating: 0.0,
                        uncertainty: 1.0,
                    },
                )
            })
            .collect();

        // Shuffle player IDs for this schedule generation (deterministic via seeded RNG)
        let mut rand_engine = rng.make_rng();
        let mut player_ids: Vec<usize> = (0..n).collect();
        player_ids.shuffle(&mut rand_engine);

        for round_offset in 0..num_rounds {
            let round = RoundNumber(starting_round.0 + round_offset);
            let round_matches = generate_round(
                players,
                &player_ids,
                team_size,
                round,
                &mut match_id_counter,
                &projected_exposure_history,
                &ranking_snapshot_by_player_id,
            );
            for scheduled_match in &round_matches {
                projected_exposure_history.record_match(
                    scheduled_match.team_a.as_slice(),
                    scheduled_match.team_b.as_slice(),
                    round,
                );
            }
            all_matches.extend(round_matches);

            // Rotate player_ids for next round (circle algorithm)
            if player_ids.len() > 1 {
                let last = player_ids.remove(player_ids.len() - 1);
                player_ids.insert(1, last);
            }
        }

        all_matches
    }
}

fn generate_round(
    players: &[Player],
    player_order: &[usize],
    team_size: usize,
    round: RoundNumber,
    match_id_counter: &mut u32,
    exposure_history: &MatchExposureHistory,
    ranking_snapshot_by_player_id: &HashMap<
        crate::models::PlayerId,
        super::match_candidate::RankingSnapshot,
    >,
) -> Vec<ScheduledMatch> {
    let players_per_match = 2 * team_size;
    let mut matches = Vec::new();
    let mut field = 1u8;

    let mut i = 0;
    while i + players_per_match <= player_order.len() {
        let group: Vec<usize> = player_order[i..i + players_per_match].to_vec();
        let (team_a, team_b) = best_team_partition_for_round_robin_group(
            players,
            &group,
            team_size,
            exposure_history,
            ranking_snapshot_by_player_id,
            round,
        );

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
        i += players_per_match;
    }

    matches
}

fn best_team_partition_for_round_robin_group(
    players: &[Player],
    group: &[usize],
    team_size: usize,
    exposure_history: &MatchExposureHistory,
    ranking_snapshot_by_player_id: &HashMap<
        crate::models::PlayerId,
        super::match_candidate::RankingSnapshot,
    >,
    round: RoundNumber,
) -> (Vec<crate::models::PlayerId>, Vec<crate::models::PlayerId>) {
    let mut best_team_a = player_ids_for_indices(players, &group[..team_size]);
    let mut best_team_b = player_ids_for_indices(players, &group[team_size..]);
    let mut best_score = f64::NEG_INFINITY;

    visit_unique_team_partitions(group, team_size, |team_a_indices, team_b_indices| {
        let team_a = player_ids_for_indices(players, team_a_indices);
        let team_b = player_ids_for_indices(players, team_b_indices);
        let candidate_score = evaluate_candidate_match_score(
            team_a.as_slice(),
            team_b.as_slice(),
            ranking_snapshot_by_player_id,
            exposure_history,
            round,
        )
        .total_score;

        if candidate_score > best_score {
            best_score = candidate_score;
            best_team_a = team_a;
            best_team_b = team_b;
        }
    });

    (best_team_a, best_team_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;
    use crate::rng::SessionRng;

    fn make_players(n: u32) -> Vec<Player> {
        (1..=n)
            .map(|i| Player {
                id: PlayerId(i),
                name: format!("P{}", i),
                status: PlayerStatus::Active,
                joined_at_round: RoundNumber(1),
                deactivated_at_round: None,
            })
            .collect()
    }

    #[test]
    fn generates_correct_match_count() {
        let scheduler = RoundRobinScheduler;
        let players = make_players(8);
        let config = SessionConfig::new(2, 1, Sport::Soccer);
        let session_id = config.id.clone();
        let mut rng = SessionRng::new(&session_id);
        let matches = scheduler.generate_schedule(ScheduleGenerationRequest {
            players: &players,
            rankings: &[],
            existing_matches: &[],
            config: &config,
            rng: &mut rng,
            starting_round: RoundNumber(1),
            num_rounds: 1,
        });
        // 8 players / (2*2 per match) = 2 matches per round
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn no_player_plays_twice_in_same_round() {
        let scheduler = RoundRobinScheduler;
        let players = make_players(8);
        let config = SessionConfig::new(2, 1, Sport::Soccer);
        let session_id = config.id.clone();
        let mut rng = SessionRng::new(&session_id);
        let matches = scheduler.generate_schedule(ScheduleGenerationRequest {
            players: &players,
            rankings: &[],
            existing_matches: &[],
            config: &config,
            rng: &mut rng,
            starting_round: RoundNumber(1),
            num_rounds: 1,
        });

        let mut seen = std::collections::HashSet::new();
        for m in &matches {
            for pid in m.team_a.iter().chain(m.team_b.iter()) {
                assert!(
                    seen.insert(pid.0),
                    "Player {} scheduled twice in same round",
                    pid.0
                );
            }
        }
    }

    #[test]
    fn deterministic_with_same_seed() {
        let players = make_players(8);
        let config = SessionConfig::new(2, 1, Sport::Soccer);
        let session_id = config.id.clone();

        let scheduler = RoundRobinScheduler;
        let mut rng1 = SessionRng::new(&session_id);
        let mut rng2 = SessionRng::new(&session_id);

        let m1 = scheduler.generate_schedule(ScheduleGenerationRequest {
            players: &players,
            rankings: &[],
            existing_matches: &[],
            config: &config,
            rng: &mut rng1,
            starting_round: RoundNumber(1),
            num_rounds: 3,
        });
        let m2 = scheduler.generate_schedule(ScheduleGenerationRequest {
            players: &players,
            rankings: &[],
            existing_matches: &[],
            config: &config,
            rng: &mut rng2,
            starting_round: RoundNumber(1),
            num_rounds: 3,
        });

        let ids1: Vec<_> = m1
            .iter()
            .flat_map(|m| m.team_a.iter().chain(m.team_b.iter()))
            .collect();
        let ids2: Vec<_> = m2
            .iter()
            .flat_map(|m| m.team_a.iter().chain(m.team_b.iter()))
            .collect();
        assert_eq!(ids1, ids2);
    }

    #[test]
    fn avoids_repeating_same_teammates_when_history_exists() {
        let scheduler = RoundRobinScheduler;
        let players = make_players(4);
        let config = SessionConfig::new(2, 1, Sport::Soccer);
        let session_id = config.id.clone();
        let mut rng = SessionRng::new(&session_id);
        let prior_match = ScheduledMatch {
            id: MatchId(7),
            round: RoundNumber(1),
            field: 1,
            team_a: vec![PlayerId(1), PlayerId(2)],
            team_b: vec![PlayerId(3), PlayerId(4)],
            status: MatchStatus::Completed,
        };

        let matches = scheduler.generate_schedule(ScheduleGenerationRequest {
            players: &players,
            rankings: &[],
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
            "round-robin should avoid identical team splits"
        );
    }
}
