//! Primary Bayesian ranking engine.
//!
//! Model: each player has a latent log-skill θ_i. Observations enter the
//! likelihood as raw counts (not transformed), keeping the model a proper
//! negative-binomial count model:
//!
//! - PointsPerTeam:  team_score ~ NB(rate = exp(Σ θ_i), r)
//!   One observation per team per match.
//! - PointsPerPlayer: player_score ~ NB(rate = exp(θ_i + context), r)
//!   One observation per player per match.
//! - WinDrawLose:   outcome ~ Bernoulli(σ(θ_i + context))  (logistic)
//!
//! The posterior over all θ is approximated via Laplace (Newton-Raphson on the
//! log-posterior), then used to derive rank distributions via Monte Carlo.

use super::RankingEngine;
use crate::models::{
    MatchId, MatchResult, MatchScorePayload, Player, PlayerId, PlayerRanking, ScheduledMatch,
    SessionConfig,
};
use nalgebra::{DMatrix, DVector};
use std::collections::HashMap;

/// Negative binomial dispersion parameter. Controls overdispersion of goal counts.
/// r > 0; higher r = less overdispersion (approaches Poisson as r → ∞).
const DEFAULT_DISPERSION: f64 = 3.0;

/// Prior standard deviation on latent skill (log-scale).
/// e^(2*1.0) ≈ 7x difference in expected rate between ±2σ players.
const PRIOR_SIGMA: f64 = 1.0;

/// Maximum Newton-Raphson iterations for MAP estimation.
const MAX_ITER: usize = 50;

/// Convergence threshold on the gradient norm.
const GRAD_TOL: f64 = 1e-6;

/// Context coupling weights used in the primary model.
const TEAMMATE_CONTEXT_WEIGHT: f64 = 0.35;
const OPPONENT_CONTEXT_WEIGHT: f64 = 0.35;

/// Conservative display score = mu - k*sigma.
const CONSERVATIVE_RATING_SIGMA_MULTIPLIER: f64 = 1.0;

/// Per-completed-round process noise for inactive or infrequently seen players.
const INACTIVITY_PROCESS_SIGMA_PER_ROUND: f64 = 0.12;

pub struct GoalModelEngine {
    /// NB dispersion parameter
    pub dispersion: f64,
    /// Prior standard deviation on latent skill
    pub prior_sigma: f64,
}

impl Default for GoalModelEngine {
    fn default() -> Self {
        Self {
            dispersion: DEFAULT_DISPERSION,
            prior_sigma: PRIOR_SIGMA,
        }
    }
}

/// Internal state after fitting the Laplace posterior
struct Posterior {
    /// MAP estimate of log-skill parameters (one per player)
    theta_map: DVector<f64>,
    /// Posterior covariance (inverse Hessian of negative log-posterior at MAP)
    covariance: DMatrix<f64>,
    /// Ordered list of player IDs corresponding to theta indices
    player_order: Vec<PlayerId>,
}

impl GoalModelEngine {
    fn conservative_rating(theta_mean: f64, theta_uncertainty: f64) -> f64 {
        theta_mean - CONSERVATIVE_RATING_SIGMA_MULTIPLIER * theta_uncertainty
    }

    fn append_weighted_group_coefficients(
        coeffs: &mut Vec<(usize, f64)>,
        group: &[PlayerId],
        player_index: &HashMap<PlayerId, usize>,
        exclude_player_id: Option<PlayerId>,
        total_weight: f64,
        result: Option<&MatchResult>,
    ) {
        let peers: Vec<usize> = group
            .iter()
            .filter(|&&player_id| Some(player_id) != exclude_player_id)
            .filter(|&&player_id| {
                result.is_none_or(|match_result| match_result.player_played(&player_id))
            })
            .filter_map(|player_id| player_index.get(player_id).copied())
            .collect();
        if peers.is_empty() {
            return;
        }
        let weight_per_peer = total_weight / peers.len() as f64;
        coeffs.extend(
            peers
                .into_iter()
                .map(|player_idx| (player_idx, weight_per_peer)),
        );
    }

    fn latest_completed_round(
        results: &[&MatchResult],
        matches_by_id: Option<&HashMap<MatchId, ScheduledMatch>>,
    ) -> Option<u32> {
        matches_by_id.and_then(|match_lookup| {
            results
                .iter()
                .filter_map(|result| {
                    match_lookup
                        .get(&result.match_id)
                        .map(|scheduled_match| scheduled_match.round.0)
                })
                .max()
        })
    }

    fn last_completed_round_played_by_player(
        player_id: PlayerId,
        results: &[&MatchResult],
        matches_by_id: Option<&HashMap<MatchId, ScheduledMatch>>,
    ) -> Option<u32> {
        matches_by_id.and_then(|match_lookup| {
            results
                .iter()
                .filter(|result| result.player_played(&player_id))
                .filter_map(|result| {
                    match_lookup
                        .get(&result.match_id)
                        .map(|scheduled_match| scheduled_match.round.0)
                })
                .max()
        })
    }

    fn inactive_rounds_since_last_completed_match(
        player: &Player,
        results: &[&MatchResult],
        matches_by_id: Option<&HashMap<MatchId, ScheduledMatch>>,
    ) -> u32 {
        let Some(latest_completed_round) = Self::latest_completed_round(results, matches_by_id)
        else {
            return 0;
        };
        let reference_round =
            Self::last_completed_round_played_by_player(player.id, results, matches_by_id)
                .unwrap_or(player.joined_at_round.0.saturating_sub(1));
        latest_completed_round.saturating_sub(reference_round)
    }

    fn variance_with_inactivity_drift(
        base_variance: f64,
        inactive_rounds: u32,
        prior_variance: f64,
    ) -> f64 {
        let process_variance = INACTIVITY_PROCESS_SIGMA_PER_ROUND.powi(2);
        (base_variance + inactive_rounds as f64 * process_variance).min(prior_variance)
    }

    /// Fit the Laplace posterior given all completed, non-voided results.
    /// Returns None if there are no results yet (no update possible).
    fn fit(
        &self,
        players: &[Player],
        results: &[&MatchResult],
        matches_by_id: Option<&HashMap<MatchId, ScheduledMatch>>,
    ) -> Option<Posterior> {
        if results.is_empty() || players.is_empty() {
            return None;
        }
        let n = players.len();
        let player_order: Vec<PlayerId> = players.iter().map(|p| p.id).collect();
        let player_index: std::collections::HashMap<PlayerId, usize> = player_order
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();

        // Initialize theta at zero (prior mean)
        let mut theta = DVector::zeros(n);

        // Newton-Raphson iteration
        let mut final_hessian: Option<DMatrix<f64>> = None;
        for _iter in 0..MAX_ITER {
            let (grad, hessian) =
                self.log_posterior_grad_hessian(&theta, &player_index, matches_by_id, results, n);
            let grad_norm = grad.norm();
            if grad_norm < GRAD_TOL {
                final_hessian = Some(hessian);
                break;
            }
            // Solve: hessian * delta = grad (hessian is negative definite at max)
            // We minimize -log_posterior, so use Cholesky on -hessian
            let neg_hessian = -hessian;
            if let Some(chol) = neg_hessian.cholesky() {
                let delta = chol.solve(&grad);
                theta += delta;
            } else {
                // Non-positive-definite Hessian — add prior regularization and retry
                break;
            }
        }

        // Posterior covariance ≈ (-Hessian)^{-1}
        // Reuse the Hessian from the final converged iteration when available.
        let neg_hessian = -final_hessian.unwrap_or_else(|| {
            self.log_posterior_grad_hessian(&theta, &player_index, matches_by_id, results, n)
                .1
        });
        let covariance = match neg_hessian.cholesky() {
            Some(chol) => chol.inverse(),
            None => DMatrix::identity(n, n) * self.prior_sigma.powi(2),
        };

        Some(Posterior {
            theta_map: theta,
            covariance,
            player_order,
        })
    }

    /// Compute gradient and Hessian of the log-posterior w.r.t. theta.
    /// log p(θ|data) = log p(data|θ) + log p(θ)
    ///   where log p(θ) = -sum_i θ_i² / (2σ²)  (Gaussian prior)
    ///
    /// Likelihood branches on the match score payload type:
    ///   PointsPerTeam   — one NB observation per team; mu = Σ θ_i
    ///   PointsPerPlayer — one NB observation per player; mu = θ_i + context
    ///   WinDrawLose     — per-player logistic (unchanged)
    fn log_posterior_grad_hessian(
        &self,
        theta: &DVector<f64>,
        player_index: &HashMap<PlayerId, usize>,
        matches_by_id: Option<&HashMap<MatchId, ScheduledMatch>>,
        results: &[&MatchResult],
        n: usize,
    ) -> (DVector<f64>, DMatrix<f64>) {
        let mut grad = DVector::zeros(n);
        let mut hess = DMatrix::zeros(n, n);

        for result in results {
            let w = result.normalized_duration_multiplier();
            let match_ctx = matches_by_id.and_then(|m| m.get(&result.match_id));

            match &result.score_payload {
                // ── Win / Draw / Lose ────────────────────────────────────────
                MatchScorePayload::WinDrawLose { outcome } => {
                    let Some(match_ctx) = match_ctx else { continue };
                    let mut coeffs: Vec<(usize, f64)> = Vec::new();
                    Self::append_weighted_group_coefficients(
                        &mut coeffs,
                        &match_ctx.team_a,
                        player_index,
                        None,
                        1.0,
                        Some(result),
                    );
                    Self::append_weighted_group_coefficients(
                        &mut coeffs,
                        &match_ctx.team_b,
                        player_index,
                        None,
                        -1.0,
                        Some(result),
                    );
                    if coeffs.is_empty() {
                        continue;
                    }

                    let mu_linear: f64 =
                        coeffs.iter().map(|(idx, coeff)| theta[*idx] * *coeff).sum();
                    let observed_outcome = match outcome {
                        crate::models::MatchOutcome::TeamAWin => 1.0,
                        crate::models::MatchOutcome::Draw => 0.5,
                        crate::models::MatchOutcome::TeamBWin => 0.0,
                    };
                    let win_prob = 1.0 / (1.0 + (-mu_linear).exp());
                    let dl_dmu = observed_outcome - win_prob;
                    let d2l_dmu2 = -win_prob * (1.0 - win_prob);
                    for (player_idx, coeff) in &coeffs {
                        grad[*player_idx] += w * dl_dmu * *coeff;
                    }
                    for (left_idx, left_coeff) in &coeffs {
                        for (right_idx, right_coeff) in &coeffs {
                            hess[(*left_idx, *right_idx)] +=
                                w * d2l_dmu2 * *left_coeff * *right_coeff;
                        }
                    }
                }

                // ── Points Per Team: one NB observation per team ─────────────
                MatchScorePayload::PointsPerTeam {
                    team_a_points,
                    team_b_points,
                } => {
                    let Some(match_ctx) = match_ctx else { continue };

                    for (team, k_raw) in [
                        (&match_ctx.team_a, *team_a_points),
                        (&match_ctx.team_b, *team_b_points),
                    ] {
                        let team_indices: Vec<usize> = team
                            .iter()
                            .filter(|pid| result.player_played(pid))
                            .filter_map(|pid| player_index.get(pid).copied())
                            .collect();
                        if team_indices.is_empty() {
                            continue;
                        }

                        let mut coeffs: Vec<(usize, f64)> =
                            team_indices.iter().copied().map(|idx| (idx, 1.0)).collect();
                        let opposing_team = if std::ptr::eq(team, &match_ctx.team_a) {
                            &match_ctx.team_b
                        } else {
                            &match_ctx.team_a
                        };
                        Self::append_weighted_group_coefficients(
                            &mut coeffs,
                            opposing_team,
                            player_index,
                            None,
                            -OPPONENT_CONTEXT_WEIGHT,
                            Some(result),
                        );

                        let mu_linear: f64 =
                            coeffs.iter().map(|(idx, coeff)| theta[*idx] * *coeff).sum();
                        let lambda = mu_linear.exp() * w;
                        let k = k_raw as f64;
                        let r = self.dispersion;

                        let dl_dlambda = k / lambda - (r + k) / (r + lambda);
                        let dl_dmu = dl_dlambda * lambda;
                        for (player_idx, coeff) in &coeffs {
                            grad[*player_idx] += dl_dmu * *coeff;
                        }

                        let d2l_dlambda2 = -k / lambda.powi(2) + (r + k) / (r + lambda).powi(2);
                        let d2l_dmu2 = d2l_dlambda2 * lambda.powi(2) + dl_dlambda * lambda;
                        for (left_idx, left_coeff) in &coeffs {
                            for (right_idx, right_coeff) in &coeffs {
                                hess[(*left_idx, *right_idx)] +=
                                    d2l_dmu2 * *left_coeff * *right_coeff;
                            }
                        }
                    }
                }

                // ── Points Per Player: one NB observation per player ─────────
                MatchScorePayload::PointsPerPlayer { player_points } => {
                    for (player_id, participation_status) in &result.participation_by_player {
                        if !participation_status.played() {
                            continue;
                        }
                        let Some(&i) = player_index.get(player_id) else {
                            continue;
                        };
                        let Some(&raw_pts) = player_points.get(player_id) else {
                            continue;
                        };
                        let k = raw_pts as f64;

                        // Linear predictor with teammate/opponent context.
                        let mut coeffs: Vec<(usize, f64)> = vec![(i, 1.0)];
                        if let Some(match_ctx) = match_ctx {
                            let (teammates, opponents): (&[PlayerId], &[PlayerId]) =
                                if match_ctx.team_a.contains(player_id) {
                                    (&match_ctx.team_a, &match_ctx.team_b)
                                } else if match_ctx.team_b.contains(player_id) {
                                    (&match_ctx.team_b, &match_ctx.team_a)
                                } else {
                                    (&[], &[])
                                };
                            Self::append_weighted_group_coefficients(
                                &mut coeffs,
                                teammates,
                                player_index,
                                Some(*player_id),
                                TEAMMATE_CONTEXT_WEIGHT,
                                Some(result),
                            );
                            Self::append_weighted_group_coefficients(
                                &mut coeffs,
                                opponents,
                                player_index,
                                Some(*player_id),
                                -OPPONENT_CONTEXT_WEIGHT,
                                Some(result),
                            );
                        }

                        let mu_linear: f64 = coeffs.iter().map(|(idx, c)| theta[*idx] * *c).sum();

                        let lambda = mu_linear.exp() * w;
                        let r = self.dispersion;

                        let dl_dlambda = k / lambda - (r + k) / (r + lambda);
                        let dl_dmu = dl_dlambda * lambda;
                        for (j, c_j) in &coeffs {
                            grad[*j] += dl_dmu * *c_j;
                        }

                        let d2l_dlambda2 = -k / lambda.powi(2) + (r + k) / (r + lambda).powi(2);
                        let d2l_dmu2 = d2l_dlambda2 * lambda.powi(2) + dl_dlambda * lambda;
                        for (j, c_j) in &coeffs {
                            for (k_idx, c_k) in &coeffs {
                                hess[(*j, *k_idx)] += d2l_dmu2 * *c_j * *c_k;
                            }
                        }
                    }
                }
            }
        }

        // Prior: -theta_i² / (2 sigma²), grad += -theta_i/sigma², hess += -1/sigma²
        let prior_var = self.prior_sigma.powi(2);
        for i in 0..n {
            grad[i] -= theta[i] / prior_var;
            hess[(i, i)] -= 1.0 / prior_var;
        }

        (grad, hess)
    }

    /// Sample rank distributions from the Laplace-approximate Gaussian posterior.
    /// Returns (rank_samples) where rank_samples[i][s] is the rank of player i in sample s.
    fn sample_ranks(posterior: &Posterior, n_samples: usize) -> Vec<Vec<u32>> {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let n = posterior.player_order.len();
        let mut player_ranks: Vec<Vec<u32>> = vec![Vec::with_capacity(n_samples); n];

        // Cholesky of covariance for sampling
        let chol = match posterior.covariance.clone().cholesky() {
            Some(c) => c,
            None => {
                // Fallback: diagonal with MAP estimates (no uncertainty)
                let mut deterministic_order: Vec<(usize, f64)> = (0..n)
                    .map(|index| (index, posterior.theta_map[index]))
                    .collect();
                deterministic_order.sort_by(|left, right| {
                    right
                        .1
                        .partial_cmp(&left.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                let deterministic_rank_by_player: HashMap<usize, u32> = deterministic_order
                    .iter()
                    .enumerate()
                    .map(|(rank_index, (player_index, _))| (*player_index, rank_index as u32 + 1))
                    .collect();

                for (i, ranks) in player_ranks.iter_mut().enumerate() {
                    let deterministic_rank =
                        deterministic_rank_by_player.get(&i).copied().unwrap_or(1);
                    for _ in 0..n_samples {
                        ranks.push(deterministic_rank);
                    }
                }
                return player_ranks;
            }
        };

        fn sample_standard_normals<R: rand::Rng>(rng: &mut R, n: usize) -> Vec<f64> {
            let mut out = Vec::with_capacity(n);
            while out.len() < n {
                let u1 = rng.gen_range(f64::MIN_POSITIVE..1.0);
                let u2 = rng.gen_range(0.0..1.0);
                let mag = (-2.0 * u1.ln()).sqrt();
                let angle = 2.0 * std::f64::consts::PI * u2;
                out.push(mag * angle.cos());
                if out.len() < n {
                    out.push(mag * angle.sin());
                }
            }
            out
        }

        for _ in 0..n_samples {
            // Sample z ~ N(0, I_n) then theta_sample = mu + L*z
            let z = sample_standard_normals(&mut rng, n);
            let z_vec = DVector::from_vec(z);
            let sample = &posterior.theta_map + chol.l() * z_vec;

            // Rank: higher theta = better player = lower rank number
            let mut indexed: Vec<(usize, f64)> = (0..n).map(|i| (i, sample[i])).collect();
            indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            for (rank, (player_idx, _)) in indexed.iter().enumerate() {
                player_ranks[*player_idx].push(rank as u32 + 1);
            }
        }

        player_ranks
    }
}

impl RankingEngine for GoalModelEngine {
    fn compute_ratings(
        &self,
        players: &[Player],
        results: &[&MatchResult],
        matches_by_id: Option<&HashMap<MatchId, ScheduledMatch>>,
        config: &SessionConfig,
    ) -> Vec<PlayerRanking> {
        let n = players.len();

        // Default: uniform uncertainty and no evidence-based ordering.
        // Use a shared top rank rather than leaking input order into the UI.
        let prior_var = self.prior_sigma.powi(2);
        let default_rankings: Vec<PlayerRanking> = players
            .iter()
            .map(|p| PlayerRanking {
                player_id: p.id,
                rating: 0.0,
                conservative_rating: Self::conservative_rating(0.0, prior_var.sqrt()),
                uncertainty: prior_var.sqrt(),
                rank: 1,
                rank_range_90: (1, n as u32),
                matches_played: 0,
                total_score: 0,
                prob_top_k: 1.0 / n as f64,
                is_active: p.status == crate::models::PlayerStatus::Active,
            })
            .collect();

        let posterior = match self.fit(players, results, matches_by_id) {
            Some(p) => p,
            None => return default_rankings,
        };

        let n_samples = 1000;
        let rank_samples = Self::sample_ranks(&posterior, n_samples);

        // Build rankings from posterior + rank samples
        let player_index: std::collections::HashMap<PlayerId, usize> = posterior
            .player_order
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();

        let top_k = (config.team_size as usize).max(1);

        players
            .iter()
            .map(|player| {
                let Some(&idx) = player_index.get(&player.id) else {
                    return PlayerRanking {
                        player_id: player.id,
                        rating: 0.0,
                        conservative_rating: Self::conservative_rating(0.0, prior_var.sqrt()),
                        uncertainty: prior_var.sqrt(),
                        rank: 1,
                        rank_range_90: (1, n as u32),
                        matches_played: 0,
                        total_score: 0,
                        prob_top_k: 1.0 / n as f64,
                        is_active: player.status == crate::models::PlayerStatus::Active,
                    };
                };

                let mut theta_mean = posterior.theta_map[idx];
                if !theta_mean.is_finite() {
                    theta_mean = 0.0;
                }

                let base_var = posterior.covariance[(idx, idx)];
                let inactive_rounds = Self::inactive_rounds_since_last_completed_match(
                    player,
                    results,
                    matches_by_id,
                );
                let mut theta_var =
                    Self::variance_with_inactivity_drift(base_var, inactive_rounds, prior_var);
                if !theta_var.is_finite() || theta_var < 0.0 {
                    theta_var = prior_var;
                }
                let theta_uncertainty = theta_var.sqrt();

                let samples = &rank_samples[idx];
                let mut sorted_ranks = samples.clone();
                sorted_ranks.sort_unstable();
                let p5 = sorted_ranks[n_samples / 20];
                let p50 = sorted_ranks[n_samples / 2];
                let p95 = sorted_ranks[19 * n_samples / 20];

                let (matches_played, total_score) =
                    results.iter().fold((0u32, 0u32), |(mp, total), result| {
                        if !result.player_played(&player.id) {
                            return (mp, total);
                        }
                        let score_value = match (&result.score_payload, matches_by_id) {
                            (MatchScorePayload::PointsPerPlayer { .. }, _) => result
                                .individual_points_for_player(&player.id)
                                .map(|points| points as u32)
                                .unwrap_or(0),
                            (_, Some(match_lookup)) => match_lookup
                                .get(&result.match_id)
                                .and_then(|scheduled_match| match &result.score_payload {
                                    MatchScorePayload::PointsPerTeam { .. } => result
                                        .team_points_for_player(&player.id, scheduled_match)
                                        .map(|points| points as u32),
                                    MatchScorePayload::WinDrawLose { .. } => result
                                        .team_outcome_value_for_player(&player.id, scheduled_match)
                                        .map(|outcome_value| {
                                            if outcome_value >= 1.0 {
                                                2
                                            } else if outcome_value > 0.0 {
                                                1
                                            } else {
                                                0
                                            }
                                        }),
                                    MatchScorePayload::PointsPerPlayer { .. } => None,
                                })
                                .unwrap_or(0),
                            _ => 0,
                        };
                        (mp + 1, total + score_value)
                    });

                let mut prob_top_k = samples.iter().filter(|&&r| r <= top_k as u32).count() as f64
                    / n_samples as f64;
                if !prob_top_k.is_finite() {
                    prob_top_k = 0.0;
                }

                PlayerRanking {
                    player_id: player.id,
                    rating: theta_mean,
                    conservative_rating: Self::conservative_rating(theta_mean, theta_uncertainty),
                    uncertainty: theta_uncertainty,
                    rank: p50,
                    rank_range_90: (p5, p95),
                    matches_played,
                    total_score,
                    prob_top_k,
                    is_active: player.status == crate::models::PlayerStatus::Active,
                }
            })
            .collect()
    }

    fn expected_info_gain(
        &self,
        team_a: &[&Player],
        team_b: &[&Player],
        current_ratings: &[PlayerRanking],
    ) -> f64 {
        // Greedy heuristic: sum of uncertainties of all players in the match,
        // weighted by closeness of expected skills between the two teams.
        let rating_map: std::collections::HashMap<PlayerId, &PlayerRanking> =
            current_ratings.iter().map(|r| (r.player_id, r)).collect();

        let team_uncertainty = |team: &[&Player]| -> f64 {
            team.iter()
                .filter_map(|p| rating_map.get(&p.id))
                .map(|r| r.uncertainty)
                .sum::<f64>()
        };

        let team_skill = |team: &[&Player]| -> f64 {
            team.iter()
                .filter_map(|p| rating_map.get(&p.id))
                .map(|r| r.rating)
                .sum::<f64>()
        };

        let total_uncertainty = team_uncertainty(team_a) + team_uncertainty(team_b);
        let skill_gap = (team_skill(team_a) - team_skill(team_b)).abs();

        // More uncertainty + closer skills = more information gain
        total_uncertainty - 0.5 * skill_gap
    }

    fn estimated_rounds_to_confidence(
        &self,
        current_ratings: &[PlayerRanking],
        target_spread: u32,
        config: &SessionConfig,
    ) -> u32 {
        if current_ratings.is_empty() {
            return 10; // Default estimate before any data
        }
        let avg_spread: f64 = current_ratings
            .iter()
            .map(|r| (r.rank_range_90.1 - r.rank_range_90.0) as f64)
            .sum::<f64>()
            / current_ratings.len() as f64;

        if avg_spread <= target_spread as f64 {
            return 0;
        }
        // Rough estimate: uncertainty decreases as ~1/sqrt(observations).
        let ratio = avg_spread / target_spread as f64;
        let player_count = current_ratings.len() as u32;
        let players_per_match = (2 * config.team_size as u32).max(1);
        let matches_per_round = (player_count / players_per_match).max(1);
        let observations_per_round = matches_per_round * players_per_match;
        let additional_observations =
            ((ratio * ratio - 1.0).max(0.0) * player_count as f64).ceil() as u32;
        additional_observations.div_ceil(observations_per_round)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;
    use std::collections::HashMap;

    fn make_player(id: u32, name: &str) -> Player {
        Player {
            id: PlayerId(id),
            name: name.to_string(),
            status: PlayerStatus::Active,
            joined_at_round: RoundNumber(1),
            deactivated_at_round: None,
        }
    }

    fn make_config() -> SessionConfig {
        SessionConfig::new(2, 1, Sport::Soccer)
    }

    fn make_config_with_team_size(team_size: u8) -> SessionConfig {
        SessionConfig::new(team_size, 1, Sport::Soccer)
    }

    fn legacy_result(match_id: u32, scores: Vec<(u32, u16)>) -> MatchResult {
        MatchResult::from_legacy_player_scores(
            MatchId(match_id),
            scores
                .into_iter()
                .map(|(player_id, points)| (PlayerId(player_id), PlayerMatchScore::scored(points)))
                .collect(),
            1.0,
            Role::Coach,
        )
    }

    fn win_draw_lose_result(
        match_id: u32,
        player_ids: &[u32],
        outcome: MatchOutcome,
        duration_multiplier: f64,
    ) -> MatchResult {
        MatchResult::new_win_draw_lose(
            MatchId(match_id),
            played_statuses(player_ids),
            outcome,
            duration_multiplier,
            Role::Coach,
        )
    }

    fn points_per_player_result(
        match_id: u32,
        scores: Vec<(u32, u16)>,
        duration_multiplier: f64,
    ) -> MatchResult {
        MatchResult::new_points_per_player(
            MatchId(match_id),
            played_statuses(
                &scores
                    .iter()
                    .map(|(player_id, _)| *player_id)
                    .collect::<Vec<_>>(),
            ),
            scores
                .into_iter()
                .map(|(player_id, points)| (PlayerId(player_id), points))
                .collect(),
            duration_multiplier,
            Role::Coach,
        )
    }

    fn scheduled_match(match_id: u32, team_a: Vec<u32>, team_b: Vec<u32>) -> ScheduledMatch {
        ScheduledMatch {
            id: MatchId(match_id),
            round: RoundNumber(1),
            field: 1,
            team_a: team_a.into_iter().map(PlayerId).collect(),
            team_b: team_b.into_iter().map(PlayerId).collect(),
            status: MatchStatus::Completed,
        }
    }

    fn played_statuses(player_ids: &[u32]) -> HashMap<PlayerId, ParticipationStatus> {
        player_ids
            .iter()
            .copied()
            .map(|player_id| (PlayerId(player_id), ParticipationStatus::Played))
            .collect()
    }

    #[test]
    fn default_rankings_with_no_results() {
        let engine = GoalModelEngine::default();
        let players = vec![make_player(1, "A"), make_player(2, "B")];
        let config = make_config();
        let rankings = engine.compute_ratings(&players, &[], None, &config);
        assert_eq!(rankings.len(), 2);
        // All tied at uniform uncertainty with no data
        assert_eq!(rankings[0].rating, 0.0);
        assert_eq!(rankings[1].rating, 0.0);
    }

    #[test]
    fn higher_scorer_ranks_better() {
        let engine = GoalModelEngine::default();
        let config = make_config();
        let players = vec![make_player(1, "Striker"), make_player(2, "Defender")];

        // 5 matches: player 1 scores 3, player 2 scores 0 each time
        let results: Vec<MatchResult> = (0..5)
            .map(|i| legacy_result(i, vec![(1, 3), (2, 0)]))
            .collect();

        let result_refs: Vec<&MatchResult> = results.iter().collect();
        let rankings = engine.compute_ratings(&players, &result_refs, None, &config);

        let r1 = rankings
            .iter()
            .find(|r| r.player_id == PlayerId(1))
            .unwrap();
        let r2 = rankings
            .iter()
            .find(|r| r.player_id == PlayerId(2))
            .unwrap();
        // High scorer should have higher (more positive) rating
        assert!(
            r1.rating > r2.rating,
            "scorer {} should beat defender {}",
            r1.rating,
            r2.rating
        );
    }

    #[test]
    fn player_who_sits_out_odd_player_round_keeps_neutral_stats_and_rank() {
        let engine = GoalModelEngine::default();
        let config = make_config();
        let players: Vec<Player> = (1..=9)
            .map(|id| make_player(id, &format!("P{id}")))
            .collect();

        let scheduled_matches = vec![
            ScheduledMatch {
                id: MatchId(1),
                round: RoundNumber(1),
                field: 1,
                team_a: vec![PlayerId(1), PlayerId(2)],
                team_b: vec![PlayerId(3), PlayerId(4)],
                status: crate::models::MatchStatus::Completed,
            },
            ScheduledMatch {
                id: MatchId(2),
                round: RoundNumber(1),
                field: 2,
                team_a: vec![PlayerId(5), PlayerId(6)],
                team_b: vec![PlayerId(7), PlayerId(8)],
                status: crate::models::MatchStatus::Completed,
            },
        ];
        let matches_by_id: HashMap<MatchId, ScheduledMatch> = scheduled_matches
            .into_iter()
            .map(|scheduled_match| (scheduled_match.id, scheduled_match))
            .collect();

        let results = [
            legacy_result(1, vec![(1, 2), (2, 1), (3, 0), (4, 0)]),
            legacy_result(2, vec![(5, 1), (6, 1), (7, 0), (8, 0)]),
        ];
        let result_refs: Vec<&MatchResult> = results.iter().collect();

        let rankings =
            engine.compute_ratings(&players, &result_refs, Some(&matches_by_id), &config);

        let benched_player_ranking = rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(9))
            .unwrap();

        assert_eq!(benched_player_ranking.matches_played, 0);
        assert_eq!(benched_player_ranking.total_score, 0);

        println!(
            "Benched player ranking after round one: rank={}, rating={:.3}, uncertainty={:.3}, range={:?}",
            benched_player_ranking.rank,
            benched_player_ranking.rating,
            benched_player_ranking.uncertainty,
            benched_player_ranking.rank_range_90
        );
    }

    #[test]
    fn no_result_default_rankings_do_not_assign_arbitrary_order() {
        let engine = GoalModelEngine::default();
        let config = make_config();
        let players: Vec<Player> = (1..=5)
            .map(|id| make_player(id, &format!("P{id}")))
            .collect();

        let rankings = engine.compute_ratings(&players, &[], None, &config);

        assert_eq!(rankings.len(), 5);
        assert!(rankings.iter().all(|ranking| ranking.rank == 1));
        assert!(rankings
            .iter()
            .all(|ranking| ranking.rank_range_90 == (1, 5)));
        assert!(rankings.iter().all(|ranking| {
            (ranking.conservative_rating - (ranking.rating - ranking.uncertainty)).abs() < 1e-9
        }));
    }

    #[test]
    fn points_per_team_higher_scoring_team_ranks_better() {
        let engine = GoalModelEngine::default();
        let config = make_config();
        let players = vec![
            make_player(1, "A1"),
            make_player(2, "A2"),
            make_player(3, "B1"),
            make_player(4, "B2"),
        ];
        let match_ctx = scheduled_match(1, vec![1, 2], vec![3, 4]);
        let matches_by_id = HashMap::from([(match_ctx.id, match_ctx)]);
        let results: Vec<MatchResult> = (0..6)
            .map(|_| {
                MatchResult::new_points_per_team(
                    MatchId(1),
                    played_statuses(&[1, 2, 3, 4]),
                    6,
                    2,
                    1.0,
                    Role::Coach,
                )
            })
            .collect();
        let result_refs: Vec<&MatchResult> = results.iter().collect();

        let rankings =
            engine.compute_ratings(&players, &result_refs, Some(&matches_by_id), &config);
        let team_a_rating: f64 = rankings
            .iter()
            .filter(|ranking| matches!(ranking.player_id, PlayerId(1) | PlayerId(2)))
            .map(|ranking| ranking.rating)
            .sum();
        let team_b_rating: f64 = rankings
            .iter()
            .filter(|ranking| matches!(ranking.player_id, PlayerId(3) | PlayerId(4)))
            .map(|ranking| ranking.rating)
            .sum();

        assert!(
            team_a_rating > team_b_rating,
            "team with repeated higher team scores should rank better: team_a={team_a_rating:.3}, team_b={team_b_rating:.3}"
        );
    }

    #[test]
    fn score_payload_branching_does_not_depend_on_session_config_mode() {
        let engine = GoalModelEngine::default();
        let players = vec![make_player(1, "Striker"), make_player(2, "Defender")];
        let results: Vec<MatchResult> = (0..5)
            .map(|match_id| legacy_result(match_id, vec![(1, 3), (2, 0)]))
            .collect();
        let result_refs: Vec<&MatchResult> = results.iter().collect();

        let mut mismatched_config = make_config();
        mismatched_config.score_entry_mode = ScoreEntryMode::WinDrawLose;
        let rankings = engine.compute_ratings(&players, &result_refs, None, &mismatched_config);

        let scorer = rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(1))
            .unwrap();
        let defender = rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(2))
            .unwrap();

        assert!(
            scorer.rating > defender.rating,
            "points-per-player payload should still use the count likelihood when config mode differs: scorer={:.3}, defender={:.3}",
            scorer.rating,
            defender.rating
        );
    }

    #[test]
    fn win_draw_lose_team_level_updates_keep_2v2_more_uncertain_than_1v1() {
        let engine = GoalModelEngine::default();

        let one_v_one_players = vec![make_player(1, "A"), make_player(2, "B")];
        let one_v_one_matches: HashMap<MatchId, ScheduledMatch> = (0..6)
            .map(|match_id| {
                let scheduled_match = scheduled_match(match_id, vec![1], vec![2]);
                (scheduled_match.id, scheduled_match)
            })
            .collect();
        let one_v_one_results: Vec<MatchResult> = (0..6)
            .map(|match_id| win_draw_lose_result(match_id, &[1, 2], MatchOutcome::TeamAWin, 1.0))
            .collect();
        let one_v_one_result_refs: Vec<&MatchResult> = one_v_one_results.iter().collect();
        let one_v_one_rankings = engine.compute_ratings(
            &one_v_one_players,
            &one_v_one_result_refs,
            Some(&one_v_one_matches),
            &make_config_with_team_size(1),
        );
        let one_v_one_winner_uncertainty = one_v_one_rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(1))
            .unwrap()
            .uncertainty;

        let two_v_two_players = vec![
            make_player(1, "A1"),
            make_player(2, "A2"),
            make_player(3, "B1"),
            make_player(4, "B2"),
        ];
        let two_v_two_matches: HashMap<MatchId, ScheduledMatch> = (100..106)
            .map(|match_id| {
                let scheduled_match = scheduled_match(match_id, vec![1, 2], vec![3, 4]);
                (scheduled_match.id, scheduled_match)
            })
            .collect();
        let two_v_two_results: Vec<MatchResult> = (100..106)
            .map(|match_id| {
                win_draw_lose_result(match_id, &[1, 2, 3, 4], MatchOutcome::TeamAWin, 1.0)
            })
            .collect();
        let two_v_two_result_refs: Vec<&MatchResult> = two_v_two_results.iter().collect();
        let two_v_two_rankings = engine.compute_ratings(
            &two_v_two_players,
            &two_v_two_result_refs,
            Some(&two_v_two_matches),
            &make_config(),
        );
        let two_v_two_winner_uncertainty = two_v_two_rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(1))
            .unwrap()
            .uncertainty;

        assert!(
            two_v_two_winner_uncertainty > one_v_one_winner_uncertainty,
            "shared 2v2 outcomes should leave each player less certain than independent 1v1 outcomes: 2v2={two_v_two_winner_uncertainty:.3}, 1v1={one_v_one_winner_uncertainty:.3}"
        );
    }

    #[test]
    fn points_per_team_opponent_context_rewards_scoring_against_stronger_opponents() {
        let engine = GoalModelEngine::default();
        let config = make_config_with_team_size(1);
        let players = vec![
            make_player(1, "FinisherVsStrongOpponent"),
            make_player(2, "StrongOpponent"),
            make_player(3, "FinisherVsWeakOpponent"),
            make_player(4, "WeakOpponent"),
            make_player(5, "Anchor"),
        ];

        let scheduled_matches = vec![
            scheduled_match(1, vec![2], vec![5]),
            scheduled_match(2, vec![1], vec![2]),
            scheduled_match(3, vec![4], vec![5]),
            scheduled_match(4, vec![3], vec![4]),
        ];
        let matches_by_id: HashMap<MatchId, ScheduledMatch> = scheduled_matches
            .into_iter()
            .map(|scheduled_match| (scheduled_match.id, scheduled_match))
            .collect();

        let mut results = Vec::new();
        for _ in 0..6 {
            results.push(MatchResult::new_points_per_team(
                MatchId(1),
                played_statuses(&[2, 5]),
                8,
                0,
                1.0,
                Role::Coach,
            ));
            results.push(MatchResult::new_points_per_team(
                MatchId(2),
                played_statuses(&[1, 2]),
                2,
                0,
                1.0,
                Role::Coach,
            ));
            results.push(MatchResult::new_points_per_team(
                MatchId(3),
                played_statuses(&[4, 5]),
                1,
                0,
                1.0,
                Role::Coach,
            ));
            results.push(MatchResult::new_points_per_team(
                MatchId(4),
                played_statuses(&[3, 4]),
                2,
                0,
                1.0,
                Role::Coach,
            ));
        }
        let result_refs: Vec<&MatchResult> = results.iter().collect();
        let rankings =
            engine.compute_ratings(&players, &result_refs, Some(&matches_by_id), &config);

        let stronger_opponent_finisher_rating = rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(1))
            .unwrap()
            .rating;
        let weaker_opponent_finisher_rating = rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(3))
            .unwrap()
            .rating;

        assert!(
            stronger_opponent_finisher_rating > weaker_opponent_finisher_rating,
            "scoring the same amount against a stronger opponent should rate higher: strong_opp={stronger_opponent_finisher_rating:.3}, weak_opp={weaker_opponent_finisher_rating:.3}"
        );
    }

    #[test]
    fn half_duration_scoring_rate_matches_full_duration_when_using_exposure() {
        let engine = GoalModelEngine::default();
        let config = make_config_with_team_size(1);
        let players = vec![
            make_player(1, "FullMatchScorer"),
            make_player(2, "HalfMatchScorer"),
            make_player(3, "SharedOpponent"),
        ];

        let scheduled_matches = (0..6)
            .map(|match_id| scheduled_match(match_id + 1, vec![1], vec![3]))
            .chain((0..6).map(|match_id| scheduled_match(match_id + 101, vec![2], vec![3])))
            .collect::<Vec<_>>();
        let matches_by_id: HashMap<MatchId, ScheduledMatch> = scheduled_matches
            .into_iter()
            .map(|scheduled_match| (scheduled_match.id, scheduled_match))
            .collect();

        let mut results = Vec::new();
        for match_id in 1..=6 {
            results.push(points_per_player_result(
                match_id,
                vec![(1, 2), (3, 0)],
                1.0,
            ));
        }
        for match_id in 101..=106 {
            results.push(points_per_player_result(
                match_id,
                vec![(2, 1), (3, 0)],
                0.5,
            ));
        }
        let result_refs: Vec<&MatchResult> = results.iter().collect();
        let rankings =
            engine.compute_ratings(&players, &result_refs, Some(&matches_by_id), &config);

        let full_match_rating = rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(1))
            .unwrap()
            .rating;
        let half_match_rating = rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(2))
            .unwrap()
            .rating;

        assert!(
            (full_match_rating - half_match_rating).abs() < 0.15,
            "equal scoring rates at different durations should stay close when duration is exposure: full={full_match_rating:.3}, half={half_match_rating:.3}"
        );
    }

    #[test]
    fn conservative_rating_is_mean_minus_uncertainty_penalty() {
        let engine = GoalModelEngine::default();
        let config = make_config_with_team_size(1);
        let players = vec![make_player(1, "Scorer"), make_player(2, "Opponent")];
        let results: Vec<MatchResult> = (0..6)
            .map(|match_id| legacy_result(match_id, vec![(1, 2), (2, 0)]))
            .collect();
        let result_refs: Vec<&MatchResult> = results.iter().collect();

        let rankings = engine.compute_ratings(&players, &result_refs, None, &config);
        let scorer = rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(1))
            .unwrap();

        assert!(
            (scorer.conservative_rating - (scorer.rating - scorer.uncertainty)).abs() < 1e-9,
            "conservative rating should be mean minus one uncertainty unit: conservative={:.6}, rating={:.6}, uncertainty={:.6}",
            scorer.conservative_rating,
            scorer.rating,
            scorer.uncertainty
        );
    }

    #[test]
    fn missing_completed_rounds_increase_uncertainty_for_stale_players() {
        let engine = GoalModelEngine::default();
        let config = make_config_with_team_size(1);
        let players = vec![
            make_player(1, "EarlyHotStart"),
            make_player(2, "AlwaysPlaying"),
            make_player(3, "Opponent"),
        ];
        let matches_by_id: HashMap<MatchId, ScheduledMatch> = vec![
            ScheduledMatch {
                id: MatchId(1),
                round: RoundNumber(1),
                field: 1,
                team_a: vec![PlayerId(1)],
                team_b: vec![PlayerId(3)],
                status: MatchStatus::Completed,
            },
            ScheduledMatch {
                id: MatchId(2),
                round: RoundNumber(2),
                field: 1,
                team_a: vec![PlayerId(2)],
                team_b: vec![PlayerId(3)],
                status: MatchStatus::Completed,
            },
            ScheduledMatch {
                id: MatchId(3),
                round: RoundNumber(3),
                field: 1,
                team_a: vec![PlayerId(2)],
                team_b: vec![PlayerId(3)],
                status: MatchStatus::Completed,
            },
            ScheduledMatch {
                id: MatchId(4),
                round: RoundNumber(4),
                field: 1,
                team_a: vec![PlayerId(2)],
                team_b: vec![PlayerId(3)],
                status: MatchStatus::Completed,
            },
        ]
        .into_iter()
        .map(|scheduled_match| (scheduled_match.id, scheduled_match))
        .collect();
        let results = vec![
            points_per_player_result(1, vec![(1, 3), (3, 0)], 1.0),
            points_per_player_result(2, vec![(2, 1), (3, 0)], 1.0),
            points_per_player_result(3, vec![(2, 1), (3, 0)], 1.0),
            points_per_player_result(4, vec![(2, 1), (3, 0)], 1.0),
        ];
        let result_refs: Vec<&MatchResult> = results.iter().collect();

        let rankings =
            engine.compute_ratings(&players, &result_refs, Some(&matches_by_id), &config);
        let stale_player = rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(1))
            .unwrap();
        let active_player = rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(2))
            .unwrap();

        assert!(
            stale_player.uncertainty > active_player.uncertainty,
            "players who miss completed rounds should get more uncertain: stale={:.3}, active={:.3}",
            stale_player.uncertainty,
            active_player.uncertainty
        );
        assert!(
            stale_player.conservative_rating < stale_player.rating,
            "conservative rating should discount stale players by their uncertainty"
        );
    }
}
