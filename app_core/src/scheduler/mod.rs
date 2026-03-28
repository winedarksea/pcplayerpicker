pub mod info_max;
pub mod round_robin;

use crate::models::{Player, PlayerRanking, RoundNumber, ScheduledMatch, SessionConfig};
use crate::rng::SessionRng;

/// Pluggable match scheduling algorithm.
pub trait Scheduler {
    /// Generate `num_rounds` worth of match schedules.
    ///
    /// `players` contains only active players.
    /// `rankings` may be empty (cold start → round-robin).
    fn generate_schedule(
        &self,
        players: &[Player],
        rankings: &[PlayerRanking],
        config: &SessionConfig,
        rng: &mut SessionRng,
        starting_round: RoundNumber,
        num_rounds: u32,
    ) -> Vec<ScheduledMatch>;
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
