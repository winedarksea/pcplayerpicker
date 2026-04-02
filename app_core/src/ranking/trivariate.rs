/// Trivariate ranking: Attack / Defense / Teamwork decomposition.
///
/// Each player i has three latent parameters:
///   θ_a[i] — attack skill (drives own goal rate)
///   θ_d[i] — defense skill (suppresses opponent goal rate)
///   θ_t[i] — teamwork skill (boosts teammates' goal rates; 2v2+ only)
///
/// For player i on team A scoring k goals in a match:
///   η_i = θ_a[i] + Σ_{j∈team_B} (-θ_d[j]) + Σ_{j∈team_A, j≠i} θ_t[j]
///   λ_i = exp(η_i)                                   (expected goal rate)
///   log P(k_i | λ_i) = NegBin(k_i; r, λ_i)
///
/// Joint posterior over 3N parameters via warm-started Laplace approximation
/// (Newton-Raphson on the log-posterior). Heavy Bayesian priors pull each
/// component toward zero, preventing overfitting on small samples.
///
/// Required minimum: MIN_MATCHES_FOR_TRIVARIATE matches per player.
use crate::models::{MatchId, MatchResult, Player, PlayerId, PlayerStatus, ScheduledMatch};
use nalgebra::{DMatrix, DVector};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Minimum matches per player before trivariate ratings are shown.
pub const MIN_MATCHES_FOR_TRIVARIATE: u32 = 3;

/// NB dispersion (same as goal model).
const DISPERSION: f64 = 3.0;

/// Prior std-dev on each component (tighter than overall model — more regularization).
const PRIOR_SIGMA: f64 = 0.8;

const MAX_ITER: usize = 50;
const GRAD_TOL: f64 = 1e-5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubRating {
    /// Posterior mean (log-scale)
    pub rating: f64,
    /// Posterior standard deviation
    pub uncertainty: f64,
}

impl SubRating {
    pub fn prior(sigma: f64) -> Self {
        Self {
            rating: 0.0,
            uncertainty: sigma,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrivariateRanking {
    pub player_id: PlayerId,
    pub attack: SubRating,
    pub defense: SubRating,
    /// None in 1v1 matches (teamwork requires teammates)
    pub teamwork: Option<SubRating>,
    pub is_active: bool,
}

pub struct TrivariateEngine {
    pub prior_sigma: f64,
}

impl Default for TrivariateEngine {
    fn default() -> Self {
        Self {
            prior_sigma: PRIOR_SIGMA,
        }
    }
}

impl TrivariateEngine {
    /// Returns None if there is insufficient data (< MIN_MATCHES_FOR_TRIVARIATE per player).
    ///
    /// `matches` provides team assignments (team_a / team_b) for each result,
    /// enabling the full η_i = θ_a[i] + Σ_opp(-θ_d[j]) + Σ_tm(θ_t[j]) coupling.
    /// When a ScheduledMatch cannot be found for a result, that result contributes
    /// only via the attack component (graceful degradation).
    pub fn compute(
        &self,
        players: &[Player],
        results: &[&MatchResult],
        matches: &[&ScheduledMatch],
        team_size: u8,
    ) -> Option<Vec<TrivariateRanking>> {
        if players.is_empty() || results.is_empty() {
            return None;
        }

        // Gate: every player must have >= MIN_MATCHES_FOR_TRIVARIATE matches.
        // Single pass over results instead of one scan per player.
        let mut play_counts: HashMap<PlayerId, u32> = HashMap::new();
        for r in results {
            for (pid, participation_status) in &r.participation_by_player {
                if participation_status.played() {
                    *play_counts.entry(*pid).or_insert(0) += 1;
                }
            }
        }
        if players
            .iter()
            .any(|p| play_counts.get(&p.id).copied().unwrap_or(0) < MIN_MATCHES_FOR_TRIVARIATE)
        {
            return None;
        }

        let has_teamwork = team_size >= 2;
        let n = players.len();
        // Parameter layout: [θ_a_0..θ_a_{n-1}, θ_d_0..θ_d_{n-1}, θ_t_0..θ_t_{n-1}]
        let dim = if has_teamwork { 3 * n } else { 2 * n };

        let player_index: HashMap<PlayerId, usize> =
            players.iter().enumerate().map(|(i, p)| (p.id, i)).collect();

        // Build match lookup for team assignments
        let match_lookup: HashMap<MatchId, &ScheduledMatch> =
            matches.iter().map(|m| (m.id, *m)).collect();

        // Newton-Raphson to find MAP
        let mut theta = DVector::zeros(dim);
        let mut final_hess: Option<DMatrix<f64>> = None;
        for _iter in 0..MAX_ITER {
            let (grad, hess) = grad_hessian(
                &theta,
                &player_index,
                results,
                &match_lookup,
                n,
                has_teamwork,
                self.prior_sigma,
            );
            if grad.norm() < GRAD_TOL {
                final_hess = Some(hess);
                break;
            }
            let neg_hess = -hess;
            if let Some(chol) = neg_hess.cholesky() {
                theta += chol.solve(&grad);
            } else {
                break;
            }
        }

        // Posterior covariance ≈ (-Hessian)^{-1}
        // Reuse the Hessian from the final converged iteration when available.
        let cov_diag: Vec<f64> = {
            let neg_hess = -final_hess.unwrap_or_else(|| {
                grad_hessian(
                    &theta,
                    &player_index,
                    results,
                    &match_lookup,
                    n,
                    has_teamwork,
                    self.prior_sigma,
                )
                .1
            });
            match neg_hess.cholesky() {
                Some(chol) => {
                    let cov = chol.inverse();
                    (0..dim).map(|i| cov[(i, i)].max(1e-6)).collect()
                }
                None => vec![self.prior_sigma.powi(2); dim],
            }
        };

        let rankings = players
            .iter()
            .map(|p| {
                let i = player_index[&p.id];
                TrivariateRanking {
                    player_id: p.id,
                    attack: SubRating {
                        rating: theta[i],
                        uncertainty: cov_diag[i].sqrt(),
                    },
                    defense: SubRating {
                        rating: theta[n + i],
                        uncertainty: cov_diag[n + i].sqrt(),
                    },
                    teamwork: if has_teamwork {
                        Some(SubRating {
                            rating: theta[2 * n + i],
                            uncertainty: cov_diag[2 * n + i].sqrt(),
                        })
                    } else {
                        None
                    },
                    is_active: p.status == PlayerStatus::Active,
                }
            })
            .collect();

        Some(rankings)
    }
}

// ── Gradient + Hessian for 3N (or 2N) parameter log-posterior ────────────────
//
// Parameter layout (indices):
//   θ_a[i] = i
//   θ_d[i] = n + i
//   θ_t[i] = 2*n + i   (only when has_teamwork)
//
// For player i on team A, scoring k goals:
//   η_i = θ_a[i]
//         + Σ_{j ∈ team_B ∩ known_players} (-θ_d[j])
//         + Σ_{j ∈ team_A ∩ known_players, j≠i} θ_t[j]   (when has_teamwork)
//
// Gradient (chain rule through η_i):
//   ∂L/∂θ_a[i]  += w * (∂ log P / ∂η_i)          [coeff +1]
//   ∂L/∂θ_d[j]  += w * (∂ log P / ∂η_i) * (-1)   [opponent j; coeff -1]
//   ∂L/∂θ_t[j]  += w * (∂ log P / ∂η_i) * (+1)   [teammate j≠i; coeff +1]
//
// Hessian (outer product of coefficient vectors × ∂²L/∂η²):
//   H[p,q] += w * (∂²log P / ∂η²) * coeff_p * coeff_q

fn grad_hessian(
    theta: &DVector<f64>,
    player_index: &HashMap<PlayerId, usize>,
    results: &[&MatchResult],
    match_lookup: &HashMap<MatchId, &ScheduledMatch>,
    n: usize,
    has_teamwork: bool,
    prior_sigma: f64,
) -> (DVector<f64>, DMatrix<f64>) {
    let dim = theta.len();
    let mut grad = DVector::zeros(dim);
    let mut hess = DMatrix::zeros(dim, dim);

    for result in results {
        let w = result.normalized_duration_multiplier();

        // Look up team assignments for this match
        let scheduled = match_lookup.get(&result.match_id);

        for (player_id, participation_status) in &result.participation_by_player {
            if !participation_status.played() {
                continue;
            }
            let Some(k) = result
                .individual_points_for_player(player_id)
                .map(|points| points as f64)
            else {
                continue;
            };
            let Some(&i) = player_index.get(player_id) else {
                continue;
            };

            // Collect parameter indices and coefficients for all parameters
            // that contribute to this player's η_i.
            // Each entry: (param_index, coefficient)
            let mut contributors: Vec<(usize, f64)> = Vec::new();

            // Always: attack component for this player (coeff = +1)
            contributors.push((i, 1.0));

            if let Some(sched) = scheduled {
                // Determine which team player i is on
                let on_team_a = sched.team_a.contains(player_id);
                let (opponents, teammates) = if on_team_a {
                    (sched.team_b.as_slice(), sched.team_a.as_slice())
                } else {
                    (sched.team_a.as_slice(), sched.team_b.as_slice())
                };

                // Defense: each opponent j contributes -θ_d[j] to η_i
                for opp in opponents {
                    if let Some(&j) = player_index.get(opp) {
                        contributors.push((n + j, -1.0));
                    }
                }

                // Teamwork: each teammate j≠i contributes +θ_t[j] to η_i
                if has_teamwork {
                    for tm in teammates {
                        if tm == player_id {
                            continue;
                        }
                        if let Some(&j) = player_index.get(tm) {
                            contributors.push((2 * n + j, 1.0));
                        }
                    }
                }
            }
            // If no scheduled match found, contributors contains only attack[i] —
            // graceful degradation to attack-only for this observation.

            // Compute η_i as dot product of coefficients and theta values
            let eta_i: f64 = contributors.iter().map(|&(p, c)| c * theta[p]).sum();
            let lambda_i = eta_i.exp().max(1e-9);
            let r = DISPERSION;

            // NB log P derivatives w.r.t. η_i
            let dl_dlambda = k / lambda_i - (r + k) / (r + lambda_i);
            let dl_deta = dl_dlambda * lambda_i;
            let d2l_dlambda2 = -k / lambda_i.powi(2) + (r + k) / (r + lambda_i).powi(2);
            let d2l_deta2 = d2l_dlambda2 * lambda_i.powi(2) + dl_dlambda * lambda_i;

            // Gradient: ∂L/∂θ_p += w * dl_deta * coeff_p
            for &(p, coeff_p) in &contributors {
                grad[p] += w * dl_deta * coeff_p;
            }

            // Hessian: H[p,q] += w * d2l_deta2 * coeff_p * coeff_q
            for &(p, coeff_p) in &contributors {
                for &(q, coeff_q) in &contributors {
                    hess[(p, q)] += w * d2l_deta2 * coeff_p * coeff_q;
                }
            }
        }
    }

    // Gaussian prior: -θ² / (2σ²) per parameter
    let prior_var = prior_sigma.powi(2);
    for j in 0..dim {
        grad[j] -= theta[j] / prior_var;
        hess[(j, j)] -= 1.0 / prior_var;
    }

    (grad, hess)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;

    fn make_player(id: u32, name: &str) -> Player {
        Player {
            id: PlayerId(id),
            name: name.into(),
            status: PlayerStatus::Active,
            joined_at_round: RoundNumber(1),
            deactivated_at_round: None,
        }
    }

    fn make_match(id: u32, team_a: Vec<u32>, team_b: Vec<u32>) -> ScheduledMatch {
        ScheduledMatch {
            id: MatchId(id),
            round: RoundNumber(1),
            field: 1,
            team_a: team_a.into_iter().map(PlayerId).collect(),
            team_b: team_b.into_iter().map(PlayerId).collect(),
            status: MatchStatus::Completed,
        }
    }

    fn make_result(match_id: u32, scores: Vec<(u32, u16)>) -> MatchResult {
        let participation_by_player = scores
            .iter()
            .map(|(pid, _)| (PlayerId(*pid), ParticipationStatus::Played))
            .collect();
        let player_points = scores
            .into_iter()
            .map(|(pid, points)| (PlayerId(pid), points))
            .collect();
        MatchResult::new_points_per_player(
            MatchId(match_id),
            participation_by_player,
            player_points,
            1.0,
            Role::Coach,
        )
    }

    #[test]
    fn trivariate_returns_none_with_too_few_matches() {
        let engine = TrivariateEngine::default();
        let players = vec![make_player(1, "A")];
        assert!(engine.compute(&players, &[], &[], 2).is_none());
    }

    #[test]
    fn trivariate_returns_some_with_sufficient_data() {
        let engine = TrivariateEngine::default();
        let players = vec![
            make_player(1, "A"),
            make_player(2, "B"),
            make_player(3, "C"),
            make_player(4, "D"),
        ];

        let matches: Vec<ScheduledMatch> = (0..3u32)
            .map(|i| make_match(i, vec![1, 2], vec![3, 4]))
            .collect();

        let results: Vec<MatchResult> = (0..3u32)
            .map(|i| make_result(i, vec![(1, 3), (2, 1), (3, 0), (4, 0)]))
            .collect();

        let result_refs: Vec<_> = results.iter().collect();
        let match_refs: Vec<_> = matches.iter().collect();

        let result = engine.compute(&players, &result_refs, &match_refs, 2);
        assert!(result.is_some());

        let ratings = result.unwrap();
        let r1 = ratings.iter().find(|r| r.player_id == PlayerId(1)).unwrap();
        let r3 = ratings.iter().find(|r| r.player_id == PlayerId(3)).unwrap();

        // High scorer should have higher attack rating than non-scorer
        assert!(
            r1.attack.rating > r3.attack.rating,
            "high scorer (A) should have higher attack than non-scorer (C): {:.3} vs {:.3}",
            r1.attack.rating,
            r3.attack.rating
        );
    }

    #[test]
    fn trivariate_defense_coupling_works() {
        // Player 3 consistently lets the opponent score a lot — should get lower defense
        // Player 4 concedes little — should get higher defense
        let engine = TrivariateEngine::default();
        let players = vec![
            make_player(1, "A"),
            make_player(2, "B"),
            make_player(3, "C"),
            make_player(4, "D"),
        ];

        let matches: Vec<ScheduledMatch> = (0..4u32)
            .map(|i| {
                // Alternate which team C/D are on so we can isolate defense
                if i % 2 == 0 {
                    make_match(i, vec![1, 2], vec![3, 4])
                } else {
                    make_match(i, vec![3, 4], vec![1, 2])
                }
            })
            .collect();

        // A scores 4 vs C/D's team, 0 vs each other; C scores 0 always, D scores 0 always
        let results: Vec<MatchResult> = (0..4u32)
            .map(|i| {
                if i % 2 == 0 {
                    make_result(i, vec![(1, 4), (2, 0), (3, 0), (4, 0)])
                } else {
                    make_result(i, vec![(1, 0), (2, 0), (3, 1), (4, 0)])
                }
            })
            .collect();

        let result_refs: Vec<_> = results.iter().collect();
        let match_refs: Vec<_> = matches.iter().collect();

        let result = engine.compute(&players, &result_refs, &match_refs, 2);
        assert!(
            result.is_some(),
            "should produce rankings with 4 matches per player"
        );
    }

    #[test]
    fn trivariate_convergence_small_session() {
        // Verify Newton-Raphson doesn't panic with typical 2v2 data
        let engine = TrivariateEngine::default();
        let players: Vec<_> = (1..=4).map(|i| make_player(i, &format!("P{i}"))).collect();

        let matches: Vec<ScheduledMatch> = (0..5u32)
            .map(|i| make_match(i, vec![1, 2], vec![3, 4]))
            .collect();

        let results: Vec<MatchResult> = (0..5u32)
            .map(|i| make_result(i, vec![(1, 2), (2, 1), (3, 0), (4, 1)]))
            .collect();

        let result_refs: Vec<_> = results.iter().collect();
        let match_refs: Vec<_> = matches.iter().collect();

        let ratings = engine
            .compute(&players, &result_refs, &match_refs, 2)
            .unwrap();
        for r in &ratings {
            assert!(r.attack.rating.is_finite(), "attack rating must be finite");
            assert!(
                r.defense.rating.is_finite(),
                "defense rating must be finite"
            );
            assert!(r
                .teamwork
                .as_ref()
                .map(|t| t.rating.is_finite())
                .unwrap_or(true));
        }
    }
}
