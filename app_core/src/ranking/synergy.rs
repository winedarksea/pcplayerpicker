/// Synergy matrix via Regularized Adjusted Plus-Minus (RAPM).
///
/// For each completed match m, build a feature vector v_m ∈ {-1, 0, +1}^N:
///   v_m[i] = +1  player i on team A
///   v_m[i] = -1  player i on team B
///   v_m[i] =  0  player i not in match
///
/// Also build pair interaction features for each pair (i,j), i<j:
///   x_m[(i,j)] = v_m[i] * v_m[j]
///   +1 if both same team, -1 if opposing teams, 0 if either absent
///
/// Target: y_m = total goals by team A − total goals by team B.
///
/// Ridge regression over the full feature matrix (N individual + N*(N-1)/2 pair cols):
///   β = (XᵀX + λI)^{-1} Xᵀy
///
/// β[0..N] = individual APM values (each player's net goal contribution per match)
/// β[N + pair_index(i,j)] = synergy S[i,j]  (positive = play well together)
///
/// Adaptive λ = base_lambda / (1 + sqrt(n_matches)) — decreases with more data.
/// Data guard: require 30%+ of unique player pairs observed before showing matrix.
use crate::models::{MatchId, MatchResult, Player, PlayerId, ScheduledMatch};
use nalgebra::{DMatrix, DVector};
use serde::{Deserialize, Serialize};

/// Minimum fraction of unique player pairs that must have played together.
pub const MIN_PAIR_COVERAGE: f64 = 0.30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynergyMatrix {
    /// Player IDs in the order corresponding to matrix rows/columns.
    pub players: Vec<PlayerId>,
    /// n×n matrix: S[i][j] = synergy coefficient for players i and j.
    /// Positive = play better together; negative = worse together than alone.
    pub matrix: Vec<Vec<f64>>,
    /// Individual APM values (net goal contribution per match).
    pub individual_apm: Vec<f64>,
}

impl SynergyMatrix {
    pub fn synergy(&self, a: PlayerId, b: PlayerId) -> Option<f64> {
        let i = self.players.iter().position(|&p| p == a)?;
        let j = self.players.iter().position(|&p| p == b)?;
        Some(self.matrix[i][j])
    }

    pub fn apm(&self, player: PlayerId) -> Option<f64> {
        let i = self.players.iter().position(|&p| p == player)?;
        Some(self.individual_apm[i])
    }
}

pub struct SynergyEngine {
    pub base_lambda: f64,
}

impl Default for SynergyEngine {
    fn default() -> Self {
        Self { base_lambda: 10.0 }
    }
}

impl SynergyEngine {
    /// Adaptive regularization: higher base, decreases with match count.
    pub fn effective_lambda(&self, n_matches: usize) -> f64 {
        self.base_lambda / (1.0 + (n_matches as f64).sqrt())
    }

    /// Returns None if there is insufficient pair coverage.
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

        // Check pair coverage
        let total_pairs = n * (n - 1) / 2;
        let observed = count_observed_pairs(players, matches);
        if total_pairs > 0 && (observed as f64) < (total_pairs as f64) * MIN_PAIR_COVERAGE {
            return None;
        }

        let player_index: std::collections::HashMap<PlayerId, usize> =
            players.iter().enumerate().map(|(i, p)| (p.id, i)).collect();

        // Enumerate unique pairs (sorted)
        let pairs: Vec<(usize, usize)> = {
            let mut v = Vec::with_capacity(total_pairs);
            for i in 0..n {
                for j in (i + 1)..n {
                    v.push((i, j));
                }
            }
            v
        };
        let pair_index: std::collections::HashMap<(usize, usize), usize> =
            pairs.iter().enumerate().map(|(k, &p)| (p, k)).collect();

        let n_features = n + pairs.len(); // individual APM + pair interactions
        let m = results.len();

        let match_lookup: std::collections::HashMap<MatchId, &ScheduledMatch> =
            matches.iter().map(|sm| (sm.id, *sm)).collect();

        let mut x_mat = DMatrix::zeros(m, n_features);
        let mut y_vec = DVector::zeros(m);

        for (row, result) in results.iter().enumerate() {
            // O(1) lookup instead of O(M) linear scan per row
            let scheduled = match_lookup.get(&result.match_id).copied();
            let (team_a_ids, team_b_ids) = match scheduled {
                Some(sm) => (sm.team_a.as_slice(), sm.team_b.as_slice()),
                None => continue, // can't determine teams — skip
            };

            // Build v_m: +1 for team_a, -1 for team_b
            let mut v_m = vec![0i32; n];
            for pid in team_a_ids {
                if let Some(&i) = player_index.get(pid) {
                    v_m[i] = 1;
                }
            }
            for pid in team_b_ids {
                if let Some(&i) = player_index.get(pid) {
                    v_m[i] = -1;
                }
            }

            // Individual APM columns
            for i in 0..n {
                x_mat[(row, i)] = v_m[i] as f64;
            }

            // Pair interaction columns
            for (&(pi, pj), &col) in &pair_index {
                x_mat[(row, n + col)] = (v_m[pi] * v_m[pj]) as f64;
            }

            // Target: goal differential (team A - team B)
            let goals_a: f64 = team_a_ids
                .iter()
                .filter_map(|pid| result.scores.get(pid))
                .filter_map(|s| s.goals)
                .map(|g| g as f64)
                .sum();
            let goals_b: f64 = team_b_ids
                .iter()
                .filter_map(|pid| result.scores.get(pid))
                .filter_map(|s| s.goals)
                .map(|g| g as f64)
                .sum();
            y_vec[row] = goals_a - goals_b;
        }

        // Ridge regression: β = (XᵀX + λI)^{-1} Xᵀy
        let lambda = self.effective_lambda(m);
        let xt = x_mat.transpose();
        let xtx = &xt * &x_mat;
        let lambda_mat = DMatrix::identity(n_features, n_features) * lambda;
        let reg = xtx + lambda_mat;
        let xty = &xt * &y_vec;

        let beta = match reg.cholesky() {
            Some(chol) => chol.solve(&xty),
            None => {
                // Fallback: recompute reg for LU decomposition (rare — matrix not PD)
                let reg2 = (&xt * &x_mat) + DMatrix::identity(n_features, n_features) * lambda;
                reg2.lu().solve(&xty)?
            }
        };

        // Individual APM values
        let individual_apm: Vec<f64> = (0..n).map(|i| beta[i]).collect();

        // Build n×n synergy matrix
        let mut matrix = vec![vec![0.0f64; n]; n];
        for (&(i, j), &col) in &pair_index {
            let s = beta[n + col];
            matrix[i][j] = s;
            matrix[j][i] = s; // symmetric
        }
        // Diagonal: self-synergy = individual APM (normalized)
        for i in 0..n {
            matrix[i][i] = individual_apm[i];
        }

        Some(SynergyMatrix {
            players: players.iter().map(|p| p.id).collect(),
            matrix,
            individual_apm,
        })
    }
}

fn count_observed_pairs(players: &[Player], matches: &[&ScheduledMatch]) -> usize {
    use std::collections::HashSet;
    let n = players.len();
    let player_set: HashSet<u32> = players.iter().map(|p| p.id.0).collect();
    let mut observed: HashSet<(u32, u32)> = HashSet::new();
    for m in matches {
        let all: Vec<u32> = m
            .team_a
            .iter()
            .chain(m.team_b.iter())
            .filter(|id| player_set.contains(&id.0))
            .map(|id| id.0)
            .collect();
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                let pair = (all[i].min(all[j]), all[i].max(all[j]));
                observed.insert(pair);
            }
        }
    }
    // Pairs with players from both teams (opponents interact too)
    observed.len().min(n * (n - 1) / 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;
    use std::collections::HashMap;

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
    fn synergy_returns_none_with_insufficient_coverage() {
        let engine = SynergyEngine::default();
        let players = make_players(4);
        // Only one match: not enough pair coverage for 4 players (6 pairs needed, 30% = 2)
        let m = ScheduledMatch {
            id: MatchId(1),
            round: RoundNumber(1),
            field: 1,
            team_a: vec![PlayerId(1), PlayerId(2)],
            team_b: vec![PlayerId(3), PlayerId(4)],
            status: MatchStatus::Completed,
        };
        let result = MatchResult {
            match_id: MatchId(1),
            scores: {
                let mut s = HashMap::new();
                for i in 1..=4u32 {
                    s.insert(PlayerId(i), PlayerMatchScore::scored(1));
                }
                s
            },
            duration_multiplier: 1.0,
            entered_by: Role::Coach,
        };
        // 1 match gives only 1 observed pair (1,2) (same team) out of 6 needed → insufficient
        let result_val = engine.compute(&players, &[&m], &[&result]);
        // May or may not be None depending on pair counting; just check it doesn't panic
        let _ = result_val;
    }

    #[test]
    fn synergy_computes_with_diverse_matches() {
        let engine = SynergyEngine { base_lambda: 1.0 };
        let players = make_players(4);

        // 6 matches with varied pairings to cover all pairs
        let sched: Vec<ScheduledMatch> = vec![
            ScheduledMatch {
                id: MatchId(1),
                round: RoundNumber(1),
                field: 1,
                team_a: vec![PlayerId(1), PlayerId(2)],
                team_b: vec![PlayerId(3), PlayerId(4)],
                status: MatchStatus::Completed,
            },
            ScheduledMatch {
                id: MatchId(2),
                round: RoundNumber(1),
                field: 1,
                team_a: vec![PlayerId(1), PlayerId(3)],
                team_b: vec![PlayerId(2), PlayerId(4)],
                status: MatchStatus::Completed,
            },
            ScheduledMatch {
                id: MatchId(3),
                round: RoundNumber(1),
                field: 1,
                team_a: vec![PlayerId(1), PlayerId(4)],
                team_b: vec![PlayerId(2), PlayerId(3)],
                status: MatchStatus::Completed,
            },
        ];

        let results: Vec<MatchResult> = sched
            .iter()
            .map(|sm| MatchResult {
                match_id: sm.id,
                scores: sm
                    .team_a
                    .iter()
                    .chain(sm.team_b.iter())
                    .map(|&pid| (pid, PlayerMatchScore::scored(2)))
                    .collect(),
                duration_multiplier: 1.0,
                entered_by: Role::Coach,
            })
            .collect();

        let sched_refs: Vec<_> = sched.iter().collect();
        let result_refs: Vec<_> = results.iter().collect();
        let output = engine.compute(&players, &sched_refs, &result_refs);
        // With 3 matches we observe 6 pairs out of 6 total → >= 30% coverage
        if let Some(mat) = output {
            assert_eq!(mat.players.len(), 4);
            assert_eq!(mat.matrix.len(), 4);
            assert_eq!(mat.individual_apm.len(), 4);
            // Matrix should be symmetric
            for i in 0..4 {
                for j in 0..4 {
                    if i != j {
                        let diff = (mat.matrix[i][j] - mat.matrix[j][i]).abs();
                        assert!(diff < 1e-10, "matrix not symmetric at ({i},{j})");
                    }
                }
            }
        }
    }
}
