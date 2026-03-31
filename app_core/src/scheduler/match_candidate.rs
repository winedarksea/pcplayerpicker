use crate::models::{MatchStatus, Player, PlayerId, PlayerRanking, RoundNumber, ScheduledMatch};
use std::collections::HashMap;

const DEFAULT_UNCERTAINTY: f64 = 1.0;
const TEAMMATE_REPEAT_PENALTY: f64 = 0.55;
const OPPONENT_REPEAT_PENALTY: f64 = 0.20;
const RECENT_TEAMMATE_REPEAT_PENALTY: f64 = 0.90;
const RECENT_OPPONENT_REPEAT_PENALTY: f64 = 0.30;

#[derive(Clone, Copy)]
pub(crate) struct MatchCandidateScore {
    pub total_score: f64,
    pub projected_uncertainty_multiplier: f64,
}

#[derive(Clone, Copy)]
pub(crate) struct RankingSnapshot {
    pub rating: f64,
    pub uncertainty: f64,
}

#[derive(Clone, Default)]
pub(crate) struct MatchExposureHistory {
    teammate_counts: HashMap<(PlayerId, PlayerId), u32>,
    opponent_counts: HashMap<(PlayerId, PlayerId), u32>,
    last_teammate_round: HashMap<(PlayerId, PlayerId), RoundNumber>,
    last_opponent_round: HashMap<(PlayerId, PlayerId), RoundNumber>,
}

impl MatchExposureHistory {
    pub(crate) fn from_matches(existing_matches: &[&ScheduledMatch]) -> Self {
        let mut history = Self::default();
        for scheduled_match in existing_matches {
            if scheduled_match.status == MatchStatus::Voided {
                continue;
            }
            history.record_match(
                scheduled_match.team_a.as_slice(),
                scheduled_match.team_b.as_slice(),
                scheduled_match.round,
            );
        }
        history
    }

    pub(crate) fn record_match(
        &mut self,
        team_a: &[PlayerId],
        team_b: &[PlayerId],
        round: RoundNumber,
    ) {
        for (left_index, left_player_id) in team_a.iter().enumerate() {
            for right_player_id in team_a.iter().skip(left_index + 1) {
                self.record_teammates(*left_player_id, *right_player_id, round);
            }
        }
        for (left_index, left_player_id) in team_b.iter().enumerate() {
            for right_player_id in team_b.iter().skip(left_index + 1) {
                self.record_teammates(*left_player_id, *right_player_id, round);
            }
        }
        for left_player_id in team_a {
            for right_player_id in team_b {
                self.record_opponents(*left_player_id, *right_player_id, round);
            }
        }
    }

    pub(crate) fn repeat_penalty(
        &self,
        team_a: &[PlayerId],
        team_b: &[PlayerId],
        candidate_round: RoundNumber,
    ) -> f64 {
        let mut total_penalty = 0.0;

        total_penalty += self.same_team_repeat_penalty(team_a, candidate_round);
        total_penalty += self.same_team_repeat_penalty(team_b, candidate_round);

        for left_player_id in team_a {
            for right_player_id in team_b {
                let pair_key = canonical_player_pair(*left_player_id, *right_player_id);
                total_penalty += self.opponent_counts.get(&pair_key).copied().unwrap_or(0) as f64
                    * OPPONENT_REPEAT_PENALTY;

                if let Some(last_round) = self.last_opponent_round.get(&pair_key).copied() {
                    total_penalty += recency_penalty(
                        candidate_round,
                        last_round,
                        RECENT_OPPONENT_REPEAT_PENALTY,
                    );
                }
            }
        }

        total_penalty
    }

    fn same_team_repeat_penalty(&self, team: &[PlayerId], candidate_round: RoundNumber) -> f64 {
        let mut total_penalty = 0.0;
        for (left_index, left_player_id) in team.iter().enumerate() {
            for right_player_id in team.iter().skip(left_index + 1) {
                let pair_key = canonical_player_pair(*left_player_id, *right_player_id);
                total_penalty += self.teammate_counts.get(&pair_key).copied().unwrap_or(0) as f64
                    * TEAMMATE_REPEAT_PENALTY;

                if let Some(last_round) = self.last_teammate_round.get(&pair_key).copied() {
                    total_penalty += recency_penalty(
                        candidate_round,
                        last_round,
                        RECENT_TEAMMATE_REPEAT_PENALTY,
                    );
                }
            }
        }
        total_penalty
    }

    fn record_teammates(
        &mut self,
        left_player_id: PlayerId,
        right_player_id: PlayerId,
        round: RoundNumber,
    ) {
        let pair_key = canonical_player_pair(left_player_id, right_player_id);
        *self.teammate_counts.entry(pair_key).or_insert(0) += 1;
        self.last_teammate_round.insert(pair_key, round);
    }

    fn record_opponents(
        &mut self,
        left_player_id: PlayerId,
        right_player_id: PlayerId,
        round: RoundNumber,
    ) {
        let pair_key = canonical_player_pair(left_player_id, right_player_id);
        *self.opponent_counts.entry(pair_key).or_insert(0) += 1;
        self.last_opponent_round.insert(pair_key, round);
    }
}

pub(crate) fn build_ranking_snapshot_by_player_id(
    rankings: &[PlayerRanking],
) -> HashMap<PlayerId, RankingSnapshot> {
    rankings
        .iter()
        .map(|ranking| {
            (
                ranking.player_id,
                RankingSnapshot {
                    rating: ranking.rating,
                    uncertainty: ranking.uncertainty.max(0.05),
                },
            )
        })
        .collect()
}

pub(crate) fn evaluate_candidate_match_score(
    team_a: &[PlayerId],
    team_b: &[PlayerId],
    ranking_snapshot_by_player_id: &HashMap<PlayerId, RankingSnapshot>,
    exposure_history: &MatchExposureHistory,
    candidate_round: RoundNumber,
) -> MatchCandidateScore {
    let team_a_rating = team_rating(team_a, ranking_snapshot_by_player_id);
    let team_b_rating = team_rating(team_b, ranking_snapshot_by_player_id);
    let team_skill_gap = (team_a_rating - team_b_rating).abs();

    let total_variance = team_variance(team_a, ranking_snapshot_by_player_id)
        + team_variance(team_b, ranking_snapshot_by_player_id);
    let balance_scale = total_variance.sqrt().max(0.35);
    let outcome_entropy_factor = logistic_balance_factor(team_skill_gap / balance_scale);
    let uncertainty_value = total_variance.ln_1p();
    let repeat_penalty = exposure_history.repeat_penalty(team_a, team_b, candidate_round);

    let total_score = uncertainty_value * outcome_entropy_factor - repeat_penalty;
    let normalized_information_value = (uncertainty_value * outcome_entropy_factor)
        / (1.0 + uncertainty_value * outcome_entropy_factor);
    let projected_uncertainty_multiplier =
        (0.95 - 0.20 * normalized_information_value).clamp(0.74, 0.95);

    MatchCandidateScore {
        total_score,
        projected_uncertainty_multiplier,
    }
}

pub(crate) fn visit_unique_team_partitions(
    ordered_group_indices: &[usize],
    team_size: usize,
    mut visit_partition: impl FnMut(&[usize], &[usize]),
) {
    if ordered_group_indices.len() != team_size * 2 || ordered_group_indices.is_empty() {
        return;
    }

    let mut current_team_a_indices = vec![ordered_group_indices[0]];
    visit_unique_team_partitions_recursive(
        ordered_group_indices,
        team_size,
        1,
        &mut current_team_a_indices,
        &mut visit_partition,
    );
}

pub(crate) fn player_ids_for_indices(
    players: &[Player],
    player_indices: &[usize],
) -> Vec<PlayerId> {
    player_indices
        .iter()
        .map(|player_index| players[*player_index].id)
        .collect()
}

fn visit_unique_team_partitions_recursive(
    ordered_group_indices: &[usize],
    team_size: usize,
    next_group_position: usize,
    current_team_a_indices: &mut Vec<usize>,
    visit_partition: &mut impl FnMut(&[usize], &[usize]),
) {
    if current_team_a_indices.len() == team_size {
        let mut team_b_indices = Vec::with_capacity(team_size);
        for player_index in ordered_group_indices {
            if !current_team_a_indices.contains(player_index) {
                team_b_indices.push(*player_index);
            }
        }
        visit_partition(current_team_a_indices.as_slice(), team_b_indices.as_slice());
        return;
    }

    let remaining_slots = team_size - current_team_a_indices.len();
    let max_group_position = ordered_group_indices.len() - remaining_slots;
    for group_position in next_group_position..=max_group_position {
        current_team_a_indices.push(ordered_group_indices[group_position]);
        visit_unique_team_partitions_recursive(
            ordered_group_indices,
            team_size,
            group_position + 1,
            current_team_a_indices,
            visit_partition,
        );
        current_team_a_indices.pop();
    }
}

fn canonical_player_pair(
    left_player_id: PlayerId,
    right_player_id: PlayerId,
) -> (PlayerId, PlayerId) {
    if left_player_id.0 <= right_player_id.0 {
        (left_player_id, right_player_id)
    } else {
        (right_player_id, left_player_id)
    }
}

fn recency_penalty(
    candidate_round: RoundNumber,
    prior_round: RoundNumber,
    base_penalty: f64,
) -> f64 {
    let round_distance = candidate_round.0.saturating_sub(prior_round.0);
    match round_distance {
        0 | 1 => base_penalty,
        2 => base_penalty * 0.5,
        3 => base_penalty * 0.25,
        _ => 0.0,
    }
}

fn team_rating(
    team: &[PlayerId],
    ranking_snapshot_by_player_id: &HashMap<PlayerId, RankingSnapshot>,
) -> f64 {
    team.iter()
        .map(|player_id| {
            ranking_snapshot_by_player_id
                .get(player_id)
                .map(|snapshot| snapshot.rating)
                .unwrap_or(0.0)
        })
        .sum()
}

fn team_variance(
    team: &[PlayerId],
    ranking_snapshot_by_player_id: &HashMap<PlayerId, RankingSnapshot>,
) -> f64 {
    team.iter()
        .map(|player_id| {
            let uncertainty = ranking_snapshot_by_player_id
                .get(player_id)
                .map(|snapshot| snapshot.uncertainty)
                .unwrap_or(DEFAULT_UNCERTAINTY);
            uncertainty * uncertainty
        })
        .sum()
}

fn logistic_balance_factor(normalized_gap: f64) -> f64 {
    let win_probability = 1.0 / (1.0 + normalized_gap.exp());
    4.0 * win_probability * (1.0 - win_probability)
}
