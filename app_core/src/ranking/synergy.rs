/// Teammate synergy matrix via duration-weighted ridge APM.
///
/// The earlier pair model mixed same-team chemistry with opponent interactions.
/// For "who goes best with whom", only same-team co-play belongs in the score.
///
/// For each completed match m, build sparse features with only players who
/// actually played in the result:
///   individual[i] = +1 if player i played on team A, -1 if on team B
///   pair[i,j]     = +1 if i,j played together on team A
///                   -1 if i,j played together on team B
///                    0 otherwise
///
/// Target: y_m = total goals by team A − total goals by team B.
/// Observation weight: w_m = duration_multiplier.
///
/// Weighted ridge normal equations:
///   (XᵀWX + Λ) β = XᵀWy
///
/// Pair coefficients are regularized more heavily than individual APM terms,
/// then post-processed with explicit same-team exposure and a cheap reliability
/// score so low-data pairs are visibly down-weighted in the UI.
use crate::models::{MatchId, MatchResult, Player, PlayerId, ScheduledMatch};
use nalgebra::{DMatrix, DVector};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Minimum fraction of unique same-team player pairs that must have co-played.
pub const MIN_TEAMMATE_PAIR_COVERAGE: f64 = 0.30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynergyMatrix {
    /// Player IDs in the order corresponding to matrix rows/columns.
    pub players: Vec<PlayerId>,
    /// n×n matrix: teammate_synergy[i][j] = pair coefficient for players i and j.
    /// Positive = better together; negative = worse together than expected.
    pub teammate_synergy: Vec<Vec<f64>>,
    /// Weighted same-team exposures for each pair, summed by duration multiplier.
    pub teammate_exposure: Vec<Vec<f64>>,
    /// Reliability in [0, 1], derived from exposure vs pair regularization.
    pub teammate_reliability: Vec<Vec<f64>>,
    /// Individual APM values (net goal contribution per match).
    pub individual_apm: Vec<f64>,
}

impl SynergyMatrix {
    pub fn synergy(&self, a: PlayerId, b: PlayerId) -> Option<f64> {
        let i = self.players.iter().position(|&p| p == a)?;
        let j = self.players.iter().position(|&p| p == b)?;
        Some(self.teammate_synergy[i][j])
    }

    pub fn apm(&self, player: PlayerId) -> Option<f64> {
        let i = self.players.iter().position(|&p| p == player)?;
        Some(self.individual_apm[i])
    }

    pub fn exposure(&self, a: PlayerId, b: PlayerId) -> Option<f64> {
        let i = self.players.iter().position(|&p| p == a)?;
        let j = self.players.iter().position(|&p| p == b)?;
        Some(self.teammate_exposure[i][j])
    }

    pub fn reliability(&self, a: PlayerId, b: PlayerId) -> Option<f64> {
        let i = self.players.iter().position(|&p| p == a)?;
        let j = self.players.iter().position(|&p| p == b)?;
        Some(self.teammate_reliability[i][j])
    }

    /// Reliability-weighted synergy score for a pair: synergy × reliability.
    /// Used as the edge weight in best-pairing matching.
    pub fn weighted_pair_score(&self, a: PlayerId, b: PlayerId) -> f64 {
        let Some(i) = self.players.iter().position(|&p| p == a) else {
            return 0.0;
        };
        let Some(j) = self.players.iter().position(|&p| p == b) else {
            return 0.0;
        };
        self.teammate_synergy[i][j] * self.teammate_reliability[i][j]
    }

    /// Find the optimal partner pairings for the given active players.
    ///
    /// Enumerates all perfect matchings and picks the one that maximises the
    /// sum of reliability-weighted synergy scores.  Feasible for N ≤ 12 (the
    /// largest case, N=12, has 10395 matchings).
    ///
    /// If `active_players` has an odd count, every possible player to sit out
    /// is tried and the best even-subset assignment is returned.
    ///
    /// Players not present in this matrix are silently ignored.
    /// Returns an empty vec when fewer than 2 valid players are supplied.
    /// Pairs are returned in descending weighted-score order.
    pub fn best_pairing(&self, active_players: &[PlayerId]) -> Vec<(PlayerId, PlayerId)> {
        let valid: Vec<PlayerId> = active_players
            .iter()
            .copied()
            .filter(|id| self.players.contains(id))
            .collect();

        if valid.len() < 2 {
            return vec![];
        }

        let mut pairs = if valid.len() % 2 == 1 {
            // Try each player as the odd one out, keep best total
            let mut best_score = f64::NEG_INFINITY;
            let mut best_pairs = vec![];
            for skip in 0..valid.len() {
                let subset: Vec<PlayerId> = valid
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != skip)
                    .map(|(_, id)| *id)
                    .collect();
                let (score, pairs) = self.max_weight_matching(&subset);
                if score > best_score {
                    best_score = score;
                    best_pairs = pairs;
                }
            }
            best_pairs
        } else {
            self.max_weight_matching(&valid).1
        };

        pairs.sort_by(|(a1, b1), (a2, b2)| {
            self.weighted_pair_score(*a2, *b2)
                .partial_cmp(&self.weighted_pair_score(*a1, *b1))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        pairs
    }

    fn max_weight_matching(&self, players: &[PlayerId]) -> (f64, Vec<(PlayerId, PlayerId)>) {
        debug_assert_eq!(players.len() % 2, 0);
        if players.is_empty() {
            return (0.0, vec![]);
        }
        let mut best = (f64::NEG_INFINITY, vec![]);
        self.enumerate_matchings(players, vec![], &mut best);
        best
    }

    fn enumerate_matchings(
        &self,
        remaining: &[PlayerId],
        current: Vec<(PlayerId, PlayerId)>,
        best: &mut (f64, Vec<(PlayerId, PlayerId)>),
    ) {
        if remaining.is_empty() {
            let total: f64 = current
                .iter()
                .map(|(a, b)| self.weighted_pair_score(*a, *b))
                .sum();
            if total > best.0 {
                *best = (total, current);
            }
            return;
        }
        let first = remaining[0];
        for i in 1..remaining.len() {
            let partner = remaining[i];
            let mut next: Vec<PlayerId> = remaining[1..].to_vec();
            next.remove(i - 1);
            let mut cur = current.clone();
            cur.push((first, partner));
            self.enumerate_matchings(&next, cur, best);
        }
    }
}

pub struct SynergyEngine {
    pub base_individual_lambda: f64,
    pub base_pair_lambda: f64,
}

impl Default for SynergyEngine {
    fn default() -> Self {
        Self {
            base_individual_lambda: 4.0,
            base_pair_lambda: 12.0,
        }
    }
}

impl SynergyEngine {
    /// Adaptive regularization: decreases with more observed matches.
    pub fn effective_individual_lambda(&self, n_matches: usize) -> f64 {
        self.base_individual_lambda / (1.0 + (n_matches as f64).sqrt())
    }

    pub fn effective_pair_lambda(&self, n_matches: usize) -> f64 {
        self.base_pair_lambda / (1.0 + (n_matches as f64).sqrt())
    }

    /// Returns None if there is insufficient same-team pair coverage.
    pub fn compute(
        &self,
        players: &[Player],
        matches: &[&ScheduledMatch],
        results: &[&MatchResult],
    ) -> Option<SynergyMatrix> {
        let n = players.len();
        if n < 2 || results.is_empty() {
            return None;
        }

        let total_pairs = n * (n - 1) / 2;
        if total_pairs == 0 {
            return None;
        }

        let player_index: HashMap<PlayerId, usize> =
            players.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
        let match_lookup: HashMap<MatchId, &ScheduledMatch> =
            matches.iter().map(|sm| (sm.id, *sm)).collect();

        let schedulable_pairs = count_schedulable_teammate_pairs(players, matches);
        if schedulable_pairs == 0 {
            return None;
        }
        let observed = count_observed_teammate_pairs(players, results, &match_lookup);
        if (observed as f64) < (schedulable_pairs as f64) * MIN_TEAMMATE_PAIR_COVERAGE {
            return None;
        }

        let n_features = n + total_pairs;
        let mut xtx = DMatrix::zeros(n_features, n_features);
        let mut xty = DVector::zeros(n_features);
        let mut teammate_exposure = vec![vec![0.0f64; n]; n];
        let mut valid_match_rows = 0usize;

        for result in results {
            let Some(scheduled) = match_lookup.get(&result.match_id).copied() else {
                continue;
            };

            let weight = result.normalized_duration_multiplier();
            if weight <= 0.0 {
                continue;
            }

            let team_a_played =
                played_team_indices(scheduled.team_a.as_slice(), result, &player_index);
            let team_b_played =
                played_team_indices(scheduled.team_b.as_slice(), result, &player_index);
            if team_a_played.is_empty() && team_b_played.is_empty() {
                continue;
            }

            let Some((team_a_points, team_b_points)) = result.numeric_team_points(scheduled) else {
                continue;
            };
            let target = team_a_points as f64 - team_b_points as f64;

            let mut features: Vec<(usize, f64)> = Vec::with_capacity(
                team_a_played.len()
                    + team_b_played.len()
                    + pair_count(team_a_played.len())
                    + pair_count(team_b_played.len()),
            );

            for &player_idx in &team_a_played {
                features.push((player_idx, 1.0));
            }
            for &player_idx in &team_b_played {
                features.push((player_idx, -1.0));
            }

            add_teammate_pair_features(
                &team_a_played,
                n,
                1.0,
                &mut features,
                &mut teammate_exposure,
                weight,
            );
            add_teammate_pair_features(
                &team_b_played,
                n,
                -1.0,
                &mut features,
                &mut teammate_exposure,
                weight,
            );

            if features.is_empty() {
                continue;
            }

            valid_match_rows += 1;
            for &(feature_idx, value) in &features {
                xty[feature_idx] += weight * value * target;
            }
            for (left_idx, &(left_feature_idx, left_value)) in features.iter().enumerate() {
                for &(right_feature_idx, right_value) in &features[left_idx..] {
                    let contribution = weight * left_value * right_value;
                    xtx[(left_feature_idx, right_feature_idx)] += contribution;
                    if left_feature_idx != right_feature_idx {
                        xtx[(right_feature_idx, left_feature_idx)] += contribution;
                    }
                }
            }
        }

        if valid_match_rows == 0 {
            return None;
        }

        let individual_lambda = self.effective_individual_lambda(valid_match_rows);
        let pair_lambda = self.effective_pair_lambda(valid_match_rows);
        for feature_idx in 0..n_features {
            xtx[(feature_idx, feature_idx)] += if feature_idx < n {
                individual_lambda
            } else {
                pair_lambda
            };
        }

        let beta = match xtx.clone().cholesky() {
            Some(chol) => chol.solve(&xty),
            None => xtx.lu().solve(&xty)?,
        };

        let individual_apm: Vec<f64> = (0..n).map(|i| beta[i]).collect();
        let mut teammate_synergy = vec![vec![0.0f64; n]; n];
        let mut teammate_reliability = vec![vec![0.0f64; n]; n];

        for i in 0..n {
            for j in (i + 1)..n {
                let pair_feature_idx = n + pair_linear_index(i, j, n);
                let score = beta[pair_feature_idx];
                let exposure = teammate_exposure[i][j];
                let reliability = exposure / (exposure + pair_lambda.max(1e-6));
                teammate_synergy[i][j] = score;
                teammate_synergy[j][i] = score;
                teammate_reliability[i][j] = reliability;
                teammate_reliability[j][i] = reliability;
            }
        }

        for i in 0..n {
            teammate_synergy[i][i] = individual_apm[i];
            teammate_exposure[i][i] = 0.0;
            teammate_reliability[i][i] = 1.0;
        }

        Some(SynergyMatrix {
            players: players.iter().map(|p| p.id).collect(),
            teammate_synergy,
            teammate_exposure,
            teammate_reliability,
            individual_apm,
        })
    }
}

fn pair_count(n: usize) -> usize {
    n.saturating_sub(1) * n / 2
}

fn pair_linear_index(i: usize, j: usize, n_players: usize) -> usize {
    debug_assert!(i < j);
    let preceding_pairs = i * (2 * n_players - i - 1) / 2;
    preceding_pairs + (j - i - 1)
}

fn played_team_indices(
    team: &[PlayerId],
    result: &MatchResult,
    player_index: &HashMap<PlayerId, usize>,
) -> Vec<usize> {
    team.iter()
        .filter_map(|player_id| {
            if !result.player_played(player_id) {
                return None;
            }
            player_index.get(player_id).copied()
        })
        .collect()
}

fn add_teammate_pair_features(
    team_indices: &[usize],
    n_players: usize,
    sign: f64,
    features: &mut Vec<(usize, f64)>,
    teammate_exposure: &mut [Vec<f64>],
    weight: f64,
) {
    for left in 0..team_indices.len() {
        for right in (left + 1)..team_indices.len() {
            let i = team_indices[left];
            let j = team_indices[right];
            let (lo, hi) = if i < j { (i, j) } else { (j, i) };
            let pair_feature_idx = n_players + pair_linear_index(lo, hi, n_players);
            features.push((pair_feature_idx, sign));
            teammate_exposure[lo][hi] += weight;
            teammate_exposure[hi][lo] += weight;
        }
    }
}

fn count_observed_teammate_pairs(
    players: &[Player],
    results: &[&MatchResult],
    matches: &HashMap<MatchId, &ScheduledMatch>,
) -> usize {
    let player_set: HashSet<PlayerId> = players.iter().map(|p| p.id).collect();
    let mut observed: HashSet<(PlayerId, PlayerId)> = HashSet::new();

    for result in results {
        let Some(scheduled) = matches.get(&result.match_id).copied() else {
            continue;
        };
        observe_team_pairs(
            scheduled.team_a.as_slice(),
            result,
            &player_set,
            &mut observed,
        );
        observe_team_pairs(
            scheduled.team_b.as_slice(),
            result,
            &player_set,
            &mut observed,
        );
    }

    observed.len()
}

fn count_schedulable_teammate_pairs(players: &[Player], matches: &[&ScheduledMatch]) -> usize {
    let player_set: HashSet<PlayerId> = players.iter().map(|p| p.id).collect();
    let mut schedulable: HashSet<(PlayerId, PlayerId)> = HashSet::new();
    for scheduled in matches {
        collect_scheduled_team_pairs(scheduled.team_a.as_slice(), &player_set, &mut schedulable);
        collect_scheduled_team_pairs(scheduled.team_b.as_slice(), &player_set, &mut schedulable);
    }
    schedulable.len()
}

fn collect_scheduled_team_pairs(
    team: &[PlayerId],
    player_set: &HashSet<PlayerId>,
    schedulable: &mut HashSet<(PlayerId, PlayerId)>,
) {
    let members: Vec<PlayerId> = team
        .iter()
        .copied()
        .filter(|id| player_set.contains(id))
        .collect();
    for left in 0..members.len() {
        for right in (left + 1)..members.len() {
            let a = members[left];
            let b = members[right];
            let pair = if a.0 < b.0 { (a, b) } else { (b, a) };
            schedulable.insert(pair);
        }
    }
}

fn observe_team_pairs(
    team: &[PlayerId],
    result: &MatchResult,
    player_set: &HashSet<PlayerId>,
    observed: &mut HashSet<(PlayerId, PlayerId)>,
) {
    let played: Vec<PlayerId> = team
        .iter()
        .copied()
        .filter(|player_id| player_set.contains(player_id))
        .filter(|player_id| result.player_played(player_id))
        .collect();

    for left in 0..played.len() {
        for right in (left + 1)..played.len() {
            let a = played[left];
            let b = played[right];
            let pair = if a.0 < b.0 { (a, b) } else { (b, a) };
            observed.insert(pair);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;

    fn make_points_per_player_result(
        match_id: MatchId,
        player_points: impl IntoIterator<Item = (PlayerId, Option<u16>)>,
        duration_multiplier: f64,
    ) -> MatchResult {
        let entries: Vec<(PlayerId, Option<u16>)> = player_points.into_iter().collect();
        let participation_by_player = entries
            .iter()
            .map(|(player_id, points)| {
                (
                    *player_id,
                    if points.is_some() {
                        ParticipationStatus::Played
                    } else {
                        ParticipationStatus::DidNotPlay
                    },
                )
            })
            .collect();
        let player_points = entries
            .into_iter()
            .filter_map(|(player_id, points)| points.map(|points| (player_id, points)))
            .collect();
        MatchResult::new_points_per_player(
            match_id,
            participation_by_player,
            player_points,
            duration_multiplier,
            Role::Coach,
        )
    }

    fn make_players(n: usize) -> Vec<Player> {
        (1..=n)
            .map(|i| Player {
                id: PlayerId(i as u32),
                name: format!("P{i}"),
                status: PlayerStatus::Active,
                joined_at_round: RoundNumber(1),
                deactivated_at_round: None,
            })
            .collect()
    }

    #[test]
    fn synergy_returns_none_with_insufficient_same_team_coverage() {
        let engine = SynergyEngine::default();
        let players = make_players(4);
        // Three matches schedule all six same-team pair combinations (6 schedulable pairs).
        let sched = [
            ScheduledMatch {
                id: MatchId(1),
                round: RoundNumber(1),
                field: 1,
                scheduling_method: crate::models::SchedulingMethod::RoundRobinV1,
                team_a: vec![PlayerId(1), PlayerId(2)],
                team_b: vec![PlayerId(3), PlayerId(4)],
                status: MatchStatus::Completed,
            },
            ScheduledMatch {
                id: MatchId(2),
                round: RoundNumber(2),
                field: 1,
                scheduling_method: crate::models::SchedulingMethod::RoundRobinV1,
                team_a: vec![PlayerId(1), PlayerId(3)],
                team_b: vec![PlayerId(2), PlayerId(4)],
                status: MatchStatus::Scheduled,
            },
            ScheduledMatch {
                id: MatchId(3),
                round: RoundNumber(3),
                field: 1,
                scheduling_method: crate::models::SchedulingMethod::RoundRobinV1,
                team_a: vec![PlayerId(1), PlayerId(4)],
                team_b: vec![PlayerId(2), PlayerId(3)],
                status: MatchStatus::Scheduled,
            },
        ];
        // Only one result, and player 2 did not play — so only pair (3,4) is observed.
        // 1 observed pair out of 6 schedulable = 16.7%, below the 30% threshold.
        let result = make_points_per_player_result(
            MatchId(1),
            [
                (PlayerId(1), Some(2)),
                (PlayerId(2), None),
                (PlayerId(3), Some(1)),
                (PlayerId(4), Some(1)),
            ],
            1.0,
        );
        let sched_refs: Vec<_> = sched.iter().collect();
        let result_val = engine.compute(&players, &sched_refs, &[&result]);
        assert!(result_val.is_none());
    }

    #[test]
    fn synergy_computes_with_diverse_same_team_matches() {
        let engine = SynergyEngine {
            base_individual_lambda: 1.0,
            base_pair_lambda: 1.0,
        };
        let players = make_players(4);

        let sched: Vec<ScheduledMatch> = vec![
            ScheduledMatch {
                id: MatchId(1),
                round: RoundNumber(1),
                field: 1,
                scheduling_method: crate::models::SchedulingMethod::RoundRobinV1,
                team_a: vec![PlayerId(1), PlayerId(2)],
                team_b: vec![PlayerId(3), PlayerId(4)],
                status: MatchStatus::Completed,
            },
            ScheduledMatch {
                id: MatchId(2),
                round: RoundNumber(1),
                field: 1,
                scheduling_method: crate::models::SchedulingMethod::RoundRobinV1,
                team_a: vec![PlayerId(1), PlayerId(3)],
                team_b: vec![PlayerId(2), PlayerId(4)],
                status: MatchStatus::Completed,
            },
            ScheduledMatch {
                id: MatchId(3),
                round: RoundNumber(1),
                field: 1,
                scheduling_method: crate::models::SchedulingMethod::RoundRobinV1,
                team_a: vec![PlayerId(1), PlayerId(4)],
                team_b: vec![PlayerId(2), PlayerId(3)],
                status: MatchStatus::Completed,
            },
        ];

        let results: Vec<MatchResult> = sched
            .iter()
            .map(|sm| {
                make_points_per_player_result(
                    sm.id,
                    sm.team_a
                        .iter()
                        .chain(sm.team_b.iter())
                        .map(|&player_id| (player_id, Some(2))),
                    1.0,
                )
            })
            .collect();

        let sched_refs: Vec<_> = sched.iter().collect();
        let result_refs: Vec<_> = results.iter().collect();
        let output = engine.compute(&players, &sched_refs, &result_refs);
        let mat = output.expect("expected synergy matrix");
        assert_eq!(mat.players.len(), 4);
        assert_eq!(mat.teammate_synergy.len(), 4);
        assert_eq!(mat.teammate_exposure.len(), 4);
        assert_eq!(mat.teammate_reliability.len(), 4);
        assert_eq!(mat.individual_apm.len(), 4);
        for i in 0..4 {
            for j in 0..4 {
                if i != j {
                    let diff = (mat.teammate_synergy[i][j] - mat.teammate_synergy[j][i]).abs();
                    assert!(diff < 1e-10, "matrix not symmetric at ({i},{j})");
                    let exposure_diff =
                        (mat.teammate_exposure[i][j] - mat.teammate_exposure[j][i]).abs();
                    assert!(exposure_diff < 1e-10, "exposure not symmetric at ({i},{j})");
                }
            }
        }
    }

    #[test]
    fn synergy_uses_duration_weighted_same_team_exposure() {
        let engine = SynergyEngine {
            base_individual_lambda: 0.5,
            base_pair_lambda: 0.5,
        };
        let players = make_players(3);
        let matches = [
            ScheduledMatch {
                id: MatchId(1),
                round: RoundNumber(1),
                field: 1,
                scheduling_method: crate::models::SchedulingMethod::RoundRobinV1,
                team_a: vec![PlayerId(1), PlayerId(2)],
                team_b: vec![PlayerId(3)],
                status: MatchStatus::Completed,
            },
            ScheduledMatch {
                id: MatchId(2),
                round: RoundNumber(2),
                field: 1,
                scheduling_method: crate::models::SchedulingMethod::RoundRobinV1,
                team_a: vec![PlayerId(1), PlayerId(2)],
                team_b: vec![PlayerId(3)],
                status: MatchStatus::Completed,
            },
        ];
        let results = [
            make_points_per_player_result(
                MatchId(1),
                [
                    (PlayerId(1), Some(3)),
                    (PlayerId(2), Some(2)),
                    (PlayerId(3), Some(1)),
                ],
                1.0,
            ),
            make_points_per_player_result(
                MatchId(2),
                [
                    (PlayerId(1), Some(2)),
                    (PlayerId(2), Some(2)),
                    (PlayerId(3), Some(1)),
                ],
                0.5,
            ),
        ];

        let match_refs: Vec<_> = matches.iter().collect();
        let result_refs: Vec<_> = results.iter().collect();
        let mat = engine
            .compute(&players, &match_refs, &result_refs)
            .expect("expected synergy");

        let exposure = mat.exposure(PlayerId(1), PlayerId(2)).unwrap();
        assert!((exposure - 1.5).abs() < 1e-10);
        let reliability = mat.reliability(PlayerId(1), PlayerId(2)).unwrap();
        assert!(reliability > 0.0 && reliability < 1.0);
    }

    #[test]
    fn synergy_excludes_players_who_did_not_play_from_pair_exposure() {
        let engine = SynergyEngine::default();
        let players = make_players(4);
        let scheduled = ScheduledMatch {
            id: MatchId(1),
            round: RoundNumber(1),
            field: 1,
            scheduling_method: crate::models::SchedulingMethod::RoundRobinV1,
            team_a: vec![PlayerId(1), PlayerId(2)],
            team_b: vec![PlayerId(3), PlayerId(4)],
            status: MatchStatus::Completed,
        };
        let result = make_points_per_player_result(
            MatchId(1),
            [
                (PlayerId(1), Some(2)),
                (PlayerId(2), None),
                (PlayerId(3), Some(1)),
                (PlayerId(4), Some(1)),
            ],
            1.0,
        );

        // schedulable pairs = (1,2) and (3,4) = 2; observed = (3,4) only = 1; 50% ≥ 30%.
        let mat = engine
            .compute(&players, &[&scheduled], &[&result])
            .expect("50% schedulable pair coverage should be sufficient");
        assert_eq!(
            mat.exposure(PlayerId(1), PlayerId(2)).unwrap(),
            0.0,
            "pair (1,2) should have zero exposure since player 2 did not play"
        );
        assert!(
            (mat.exposure(PlayerId(3), PlayerId(4)).unwrap() - 1.0).abs() < 1e-10,
            "pair (3,4) should have exposure of 1.0"
        );
    }
}
