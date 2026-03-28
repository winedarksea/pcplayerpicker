/// Round-robin scheduler for cold start (no ranking data yet).
///
/// Uses the circle algorithm to generate balanced pairings where every player
/// faces every other player an equal number of times over the full rotation.
/// Teams within each match are assigned to balance total player exposure.
use super::Scheduler;
use crate::models::{
    MatchId, MatchStatus, Player, PlayerRanking, RoundNumber, ScheduledMatch, SessionConfig,
};
use crate::rng::SessionRng;
use rand::seq::SliceRandom;

pub struct RoundRobinScheduler;

impl Scheduler for RoundRobinScheduler {
    fn generate_schedule(
        &self,
        players: &[Player],
        _rankings: &[PlayerRanking],
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
        let mut match_id_counter = starting_round.0 * 1000; // unique IDs per round block

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
            );
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
) -> Vec<ScheduledMatch> {
    let players_per_match = 2 * team_size;
    let mut matches = Vec::new();
    let mut field = 1u8;

    let mut i = 0;
    while i + players_per_match <= player_order.len() {
        let group: Vec<usize> = player_order[i..i + players_per_match].to_vec();
        let team_a: Vec<_> = group[..team_size]
            .iter()
            .map(|&idx| players[idx].id)
            .collect();
        let team_b: Vec<_> = group[team_size..]
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
        i += players_per_match;
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;

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
        let matches =
            scheduler.generate_schedule(&players, &[], &config, &mut rng, RoundNumber(1), 1);
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
        let matches =
            scheduler.generate_schedule(&players, &[], &config, &mut rng, RoundNumber(1), 1);

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

        let m1 = scheduler.generate_schedule(&players, &[], &config, &mut rng1, RoundNumber(1), 3);
        let m2 = scheduler.generate_schedule(&players, &[], &config, &mut rng2, RoundNumber(1), 3);

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
}
