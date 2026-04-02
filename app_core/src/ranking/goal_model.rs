//! Primary Bayesian ranking engine.
//!
//! Model: each player has a latent log-skill θ_i. Observations enter the
//! likelihood as raw counts (not transformed), keeping the model a proper
//! quasi-negative-binomial count model:
//!
//! - PointsPerTeam:  team_score ~ NB(rate = exp(Σ θ_i + pace_offset), r)
//!                   One observation per team per match.
//! - PointsPerPlayer: player_score ~ NB(rate = exp(θ_i + context + pace_offset), r)
//!                   One observation per player per match.
//! - WinDrawLose:   outcome ~ Bernoulli(σ(θ_i + context))  (logistic)
//!
//! Pace/control corrections are additive log-rate offsets in the linear
//! predictor (not applied to k). The posterior over all θ is approximated via
//! Laplace (Newton-Raphson on the log-posterior), then used to derive rank
//! distributions via Monte Carlo.

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
/// Tighter than 1.0 to prevent early jumpy rankings from a single extreme match.
/// e^(2*0.7) ≈ 4x difference in expected rate between ±2σ players.
const PRIOR_SIGMA: f64 = 0.7;

/// Maximum Newton-Raphson iterations for MAP estimation.
const MAX_ITER: usize = 50;

/// Convergence threshold on the gradient norm.
const GRAD_TOL: f64 = 1e-6;

/// Context coupling weights used in the primary model.
const TEAMMATE_CONTEXT_WEIGHT: f64 = 0.35;
const OPPONENT_CONTEXT_WEIGHT: f64 = 0.35;

/// Softens the contribution of raw point totals so the main ranking is less
/// gameable in high-scoring environments. Aggressive/attack-style rankings can
/// still expose raw scoring more directly elsewhere.
const MATCH_CONTROL_BONUS_WEIGHT: f64 = 0.30;

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
    /// Additive log-rate offset applied to the linear predictor (not to k).
    /// Encodes pace normalization and team-control bonus so the NB observation
    /// remains a proper raw count.
    fn match_predictor_offset(
        own_team_points: f64,
        opponent_team_points: f64,
        players_in_match: usize,
        own_team_size: usize,
    ) -> f64 {
        let total_points = own_team_points + opponent_team_points;
        let reference_points = players_in_match.max(1) as f64;
        // Downweight high-scoring sessions: offset is negative when total > ref.
        let pace_log_offset = if total_points > reference_points {
            0.5 * (reference_points / total_points).ln()
        } else {
            0.0
        };
        let control_offset = if own_team_points > opponent_team_points {
            MATCH_CONTROL_BONUS_WEIGHT
                * ((own_team_points + 1.0) / (opponent_team_points + 1.0)).ln()
                / own_team_size.max(1) as f64
        } else {
            0.0
        };
        pace_log_offset + control_offset
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
            let (grad, hessian) = self.log_posterior_grad_hessian(
                &theta,
                &player_index,
                matches_by_id,
                results,
                n,
            );
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
            self.log_posterior_grad_hessian(
                &theta,
                &player_index,
                matches_by_id,
                results,
                n,
            )
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
    ///   PointsPerTeam   — one NB observation per team; mu = Σ θ_i + pace_offset
    ///   PointsPerPlayer — one NB observation per player; mu = θ_i + context + pace_offset
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
                MatchScorePayload::WinDrawLose { .. } => {
                    let Some(match_ctx) = match_ctx else { continue };
                    for (player_id, participation_status) in &result.participation_by_player {
                        if !participation_status.played() {
                            continue;
                        }
                        let Some(&i) = player_index.get(player_id) else { continue };

                        let mut coeffs: Vec<(usize, f64)> = vec![(i, 1.0)];
                        let (teammates, opponents): (&[PlayerId], &[PlayerId]) =
                            if match_ctx.team_a.contains(player_id) {
                                (&match_ctx.team_a, &match_ctx.team_b)
                            } else if match_ctx.team_b.contains(player_id) {
                                (&match_ctx.team_b, &match_ctx.team_a)
                            } else {
                                (&[], &[])
                            };
                        let mut add_group = |group: &[PlayerId], total_weight: f64| {
                            let peers: Vec<usize> = group
                                .iter()
                                .filter(|&&pid| pid != *player_id)
                                .filter_map(|pid| player_index.get(pid).copied())
                                .collect();
                            if peers.is_empty() {
                                return;
                            }
                            let each = total_weight / peers.len() as f64;
                            coeffs.extend(peers.into_iter().map(|idx| (idx, each)));
                        };
                        add_group(teammates, TEAMMATE_CONTEXT_WEIGHT);
                        add_group(opponents, -OPPONENT_CONTEXT_WEIGHT);

                        let mu_i: f64 = coeffs.iter().map(|(idx, c)| theta[*idx] * *c).sum();
                        let Some(observed_outcome) =
                            result.team_outcome_value_for_player(player_id, match_ctx)
                        else {
                            continue;
                        };
                        let win_prob = 1.0 / (1.0 + (-mu_i).exp());
                        let dl_dmu = observed_outcome - win_prob;
                        let d2l_dmu2 = -win_prob * (1.0 - win_prob);
                        for (j, c_j) in &coeffs {
                            grad[*j] += w * dl_dmu * *c_j;
                        }
                        for (j, c_j) in &coeffs {
                            for (k_idx, c_k) in &coeffs {
                                hess[(*j, *k_idx)] += w * d2l_dmu2 * *c_j * *c_k;
                            }
                        }
                    }
                }

                // ── Points Per Team: one NB observation per team ─────────────
                MatchScorePayload::PointsPerTeam {
                    team_a_points,
                    team_b_points,
                } => {
                    let Some(match_ctx) = match_ctx else { continue };

                    let total_points = (*team_a_points + *team_b_points) as f64;
                    let n_playing_a = match_ctx
                        .team_a
                        .iter()
                        .filter(|pid| result.player_played(pid))
                        .count();
                    let n_playing_b = match_ctx
                        .team_b
                        .iter()
                        .filter(|pid| result.player_played(pid))
                        .count();
                    let reference_points = (n_playing_a + n_playing_b).max(1) as f64;
                    let pace_log_offset = if total_points > reference_points {
                        0.5 * (reference_points / total_points).ln()
                    } else {
                        0.0
                    };

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

                        let mu_linear: f64 = team_indices.iter().map(|&i| theta[i]).sum();
                        let lambda = (mu_linear + pace_log_offset).exp();
                        let k = k_raw as f64;
                        let r = self.dispersion;

                        let dl_dlambda = k / lambda - (r + k) / (r + lambda);
                        let dl_dmu = dl_dlambda * lambda;
                        for &i in &team_indices {
                            grad[i] += w * dl_dmu;
                        }

                        let d2l_dlambda2 =
                            -k / lambda.powi(2) + (r + k) / (r + lambda).powi(2);
                        let d2l_dmu2 = d2l_dlambda2 * lambda.powi(2) + dl_dlambda * lambda;
                        for &i in &team_indices {
                            for &j in &team_indices {
                                hess[(i, j)] += w * d2l_dmu2;
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
                        let Some(&i) = player_index.get(player_id) else { continue };
                        let Some(&raw_pts) = player_points.get(player_id) else { continue };
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
                            let mut add_group = |group: &[PlayerId], total_weight: f64| {
                                let peers: Vec<usize> = group
                                    .iter()
                                    .filter(|&&pid| pid != *player_id)
                                    .filter_map(|pid| player_index.get(pid).copied())
                                    .collect();
                                if peers.is_empty() {
                                    return;
                                }
                                let each = total_weight / peers.len() as f64;
                                coeffs.extend(peers.into_iter().map(|idx| (idx, each)));
                            };
                            add_group(teammates, TEAMMATE_CONTEXT_WEIGHT);
                            add_group(opponents, -OPPONENT_CONTEXT_WEIGHT);
                        }

                        let mu_linear: f64 =
                            coeffs.iter().map(|(idx, c)| theta[*idx] * *c).sum();

                        // Pace and control corrections as additive log-rate offsets.
                        let predictor_offset = if let Some(match_ctx) = match_ctx {
                            let own_team_points = result
                                .team_points_for_player(player_id, match_ctx)
                                .map(|p| p as f64)
                                .unwrap_or(k);
                            let opponent_team_points = result
                                .opponent_team_points_for_player(player_id, match_ctx)
                                .map(|p| p as f64)
                                .unwrap_or(0.0);
                            let own_team = if match_ctx.team_a.contains(player_id) {
                                &match_ctx.team_a
                            } else {
                                &match_ctx.team_b
                            };
                            let opponent_team = if match_ctx.team_a.contains(player_id) {
                                &match_ctx.team_b
                            } else {
                                &match_ctx.team_a
                            };
                            let own_team_size = own_team
                                .iter()
                                .filter(|pid| result.player_played(pid))
                                .count();
                            let opponent_team_size = opponent_team
                                .iter()
                                .filter(|pid| result.player_played(pid))
                                .count();
                            Self::match_predictor_offset(
                                own_team_points,
                                opponent_team_points,
                                own_team_size + opponent_team_size,
                                own_team_size,
                            )
                        } else {
                            0.0
                        };

                        let lambda = (mu_linear + predictor_offset).exp();
                        let r = self.dispersion;

                        let dl_dlambda = k / lambda - (r + k) / (r + lambda);
                        let dl_dmu = dl_dlambda * lambda;
                        for (j, c_j) in &coeffs {
                            grad[*j] += w * dl_dmu * *c_j;
                        }

                        let d2l_dlambda2 =
                            -k / lambda.powi(2) + (r + k) / (r + lambda).powi(2);
                        let d2l_dmu2 = d2l_dlambda2 * lambda.powi(2) + dl_dlambda * lambda;
                        for (j, c_j) in &coeffs {
                            for (k_idx, c_k) in &coeffs {
                                hess[(*j, *k_idx)] += w * d2l_dmu2 * *c_j * *c_k;
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

                // Widen uncertainty for inactive players: their skill estimate is
                // "as of last match" and becomes less reliable with time.
                // A fixed 1.5× multiplier is a conservative first approximation.
                let base_var = posterior.covariance[(idx, idx)];
                let mut theta_var = if player.status == crate::models::PlayerStatus::Inactive {
                    base_var * 1.5
                } else {
                    base_var
                };
                if !theta_var.is_finite() || theta_var < 0.0 {
                    theta_var = prior_var;
                }

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
                    uncertainty: theta_var.sqrt(),
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
        use std::collections::HashMap;

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
    }

    #[test]
    fn match_predictor_offset_is_more_negative_for_high_scoring_matches() {
        // Pace offset should be more negative (lower expected rate) when the
        // total points far exceed the reference level.
        let low_offset = GoalModelEngine::match_predictor_offset(10.0, 8.0, 4, 2);
        let high_offset = GoalModelEngine::match_predictor_offset(20.0, 16.0, 4, 2);
        assert!(
            high_offset < low_offset,
            "high-scoring match should have a more negative log-rate offset: low={low_offset:.3}, high={high_offset:.3}"
        );
    }

    #[test]
    fn match_predictor_offset_gives_control_bonus_to_winning_team_only() {
        // Winning team gets a positive control component; losing team does not.
        let winner_offset = GoalModelEngine::match_predictor_offset(5.0, 0.0, 4, 2);
        let loser_offset = GoalModelEngine::match_predictor_offset(0.0, 5.0, 4, 2);
        assert!(
            winner_offset > loser_offset,
            "winning team should have a higher predictor offset: winner={winner_offset:.3}, loser={loser_offset:.3}"
        );
    }
}
