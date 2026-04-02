pub mod goal_model;
pub mod synergy;
pub mod trivariate;

use crate::models::{
    MatchId, MatchResult, Player, PlayerRanking, RankingMethod, ScheduledMatch, SessionConfig,
};
use std::collections::HashMap;

/// Pluggable ranking algorithm interface. Implementations live in submodules.
/// The trait is intentionally synchronous — all computation is CPU-bound and
/// runs client-side in WASM. No async needed here.
pub trait RankingEngine {
    /// Compute ratings from the full history of completed, non-voided results.
    fn compute_ratings(
        &self,
        players: &[Player],
        results: &[&MatchResult],
        matches_by_id: Option<&HashMap<MatchId, ScheduledMatch>>,
        config: &SessionConfig,
    ) -> Vec<PlayerRanking>;

    /// Expected reduction in posterior pairwise order entropy if this matchup
    /// were played. Used by the InfoMax scheduler to select informative games.
    fn expected_info_gain(
        &self,
        team_a: &[&Player],
        team_b: &[&Player],
        current_ratings: &[PlayerRanking],
    ) -> f64;

    /// Estimate how many additional rounds are needed to achieve a ranking
    /// where the 90% credible rank interval width averages ≤ `target_spread`.
    fn estimated_rounds_to_confidence(
        &self,
        current_ratings: &[PlayerRanking],
        target_spread: u32,
        config: &SessionConfig,
    ) -> u32;
}

pub fn select_ranking_engine(config: &SessionConfig) -> (RankingMethod, Box<dyn RankingEngine>) {
    match config.ranking_method {
        RankingMethod::GoalModelV1 => (
            RankingMethod::GoalModelV1,
            Box::new(goal_model::GoalModelEngine::default()),
        ),
    }
}
