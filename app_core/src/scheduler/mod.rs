pub mod info_max;
pub(crate) mod match_candidate;
pub mod round_robin;

use crate::models::{Player, PlayerRanking, RoundNumber, ScheduledMatch, SessionConfig};
use crate::rng::SessionRng;

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
    fn generate_schedule(
        &self,
        request: ScheduleGenerationRequest<'_>,
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
