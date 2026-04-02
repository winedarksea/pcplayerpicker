//! Primary Bayesian ranking engine.
//!
//! Model: each player has a latent log-skill θ_i. Raw goals/points are first
//! converted into a tempered scoring signal that soft-caps individual volume,
//! downweights unusually high-tempo matches, and gives a small shared bonus for
//! team control. That signal then follows a Negative Binomial likelihood whose
//! rate is modulated by team context (sum of teammate skills vs opponent
//! skills). The posterior over all θ is approximated via warm-started Laplace
//! (Newton-Raphson on the log-posterior), then used to derive rank
//! distributions via Monte Carlo.

use super::RankingEngine;
use crate::models::{
    MatchId, MatchResult, MatchScorePayload, Player, PlayerId, PlayerRanking, ScheduledMatch,
    ScoreEntryMode, SessionConfig,
};
use nalgebra::{DMatrix, DVector};
use std::collections::HashMap;

/// Negative binomial dispersion parameter. Controls overdispersion of goal counts.
/// r > 0; higher r = less overdispersion (approaches Poisson as r → ∞).
const DEFAULT_DISPERSION: f64 = 3.0;

/// Prior standard deviation on latent skill (log-scale).
/// Roughly: e^(2*sigma) ≈ 7x difference in expected goal rate between ±2σ players.
const PRIOR_SIGMA: f64 = 1.0;

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
    fn tempered_scoring_signal(
        raw_goals: f64,
        own_team_goals: f64,
        opponent_team_goals: f64,
        players_in_match: usize,
        own_team_size: usize,
    ) -> f64 {
        let softened_goals = raw_goals.ln_1p();
        let total_points = own_team_goals + opponent_team_goals;
        let reference_points = players_in_match.max(1) as f64;
        let pace_factor = (reference_points / total_points.max(reference_points)).sqrt();
        let match_control_bonus = if own_team_goals > opponent_team_goals {
            MATCH_CONTROL_BONUS_WEIGHT
                * ((own_team_goals + 1.0) / (opponent_team_goals + 1.0)).ln()
                / own_team_size.max(1) as f64
        } else {
            0.0
        };

        pace_factor * (softened_goals + match_control_bonus)
    }

    /// Fit the Laplace posterior given all completed, non-voided results.
    /// Returns None if there are no results yet (no update possible).
    fn fit(
        &self,
        players: &[Player],
        results: &[&MatchResult],
        matches_by_id: Option<&HashMap<MatchId, ScheduledMatch>>,
        config: &SessionConfig,
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
                config,
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
                config,
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
    fn log_posterior_grad_hessian(
        &self,
        theta: &DVector<f64>,
        player_index: &HashMap<PlayerId, usize>,
        matches_by_id: Option<&HashMap<MatchId, ScheduledMatch>>,
        results: &[&MatchResult],
        n: usize,
        config: &SessionConfig,
    ) -> (DVector<f64>, DMatrix<f64>) {
        let mut grad = DVector::zeros(n);
        let mut hess = DMatrix::zeros(n, n);

        // Likelihood contributions from each player's observed contribution in each match.
        for result in results {
            let w = result.normalized_duration_multiplier();
            for (player_id, participation_status) in &result.participation_by_player {
                if !participation_status.played() {
                    continue;
                }
                let Some(&i) = player_index.get(player_id) else {
                    continue;
                };

                let mut coeffs: Vec<(usize, f64)> = vec![(i, 1.0)];
                let match_ctx = matches_by_id.and_then(|m| m.get(&result.match_id));

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

                let mu_i: f64 = coeffs.iter().map(|(idx, c)| theta[*idx] * *c).sum();

                if config.score_entry_mode == ScoreEntryMode::WinDrawLose {
                    let Some(match_ctx) = match_ctx else {
                        continue;
                    };
                    let Some(observed_outcome) =
                        result.team_outcome_value_for_player(player_id, match_ctx)
                    else {
                        continue;
                    };
                    let win_probability = 1.0 / (1.0 + (-mu_i).exp());
                    let dl_dmu = observed_outcome - win_probability;
                    let d2l_dmu2 = -win_probability * (1.0 - win_probability);
                    for (j, c_j) in &coeffs {
                        grad[*j] += w * dl_dmu * *c_j;
                    }
                    for (j, c_j) in &coeffs {
                        for (k, c_k) in &coeffs {
                            hess[(*j, *k)] += w * d2l_dmu2 * *c_j * *c_k;
                        }
                    }
                    continue;
                }

                let raw_points = match (&result.score_payload, match_ctx) {
                    (MatchScorePayload::PointsPerPlayer { .. }, _) => {
                        result.individual_points_for_player(player_id).map(|points| points as f64)
                    }
                    (MatchScorePayload::PointsPerTeam { .. }, Some(match_ctx)) => result
                        .team_points_for_player(player_id, match_ctx)
                        .map(|points| points as f64),
                    _ => None,
                };
                let Some(raw_points) = raw_points else {
                    continue;
                };

                let mut effective_points = raw_points.ln_1p();
                if let Some(match_ctx) = match_ctx {
                    let own_team_points = result
                        .team_points_for_player(player_id, match_ctx)
                        .map(|points| points as f64)
                        .unwrap_or(raw_points);
                    let opponent_team_points = result
                        .opponent_team_points_for_player(player_id, match_ctx)
                        .map(|points| points as f64)
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
                    effective_points = Self::tempered_scoring_signal(
                        raw_points,
                        own_team_points,
                        opponent_team_points,
                        own_team_size + opponent_team_size,
                        own_team_size,
                    );
                }

                let lambda = mu_i.exp();
                let r = self.dispersion;
                let k = effective_points;
                let dl_dlambda = k / lambda - (r + k) / (r + lambda);
                let dl_dmu = dl_dlambda * lambda;
                for (j, c_j) in &coeffs {
                    grad[*j] += w * dl_dmu * *c_j;
                }

                let d2l_dlambda2 = -k / lambda.powi(2) + (r + k) / (r + lambda).powi(2);
                let d2l_dmu2 = d2l_dlambda2 * lambda.powi(2) + dl_dlambda * lambda;
                for (j, c_j) in &coeffs {
                    for (k, c_k) in &coeffs {
                        hess[(*j, *k)] += w * d2l_dmu2 * *c_j * *c_k;
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

        let posterior = match self.fit(players, results, matches_by_id, config) {
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
    fn tempered_scoring_signal_downweights_high_scoring_close_matches() {
        let low_total = GoalModelEngine::tempered_scoring_signal(5.0, 10.0, 8.0, 4, 2);
        let high_total = GoalModelEngine::tempered_scoring_signal(10.0, 20.0, 16.0, 4, 2);
        assert!(
            high_total < low_total * 1.35,
            "high-scoring close matches should not create proportionally larger evidence: low={low_total:.3}, high={high_total:.3}"
        );
    }

    #[test]
    fn tempered_scoring_signal_gives_small_credit_for_team_control() {
        let scoreless_winner = GoalModelEngine::tempered_scoring_signal(0.0, 5.0, 0.0, 4, 2);
        let scoreless_loser = GoalModelEngine::tempered_scoring_signal(0.0, 0.0, 5.0, 4, 2);
        assert!(scoreless_winner > 0.0);
        assert_eq!(scoreless_loser, 0.0);
    }

    #[test]
    fn high_scoring_point_marathons_do_not_expand_rating_gaps_like_raw_counts() {
        use std::collections::HashMap;

        let engine = GoalModelEngine::default();
        let config = make_config();
        let players = vec![
            make_player(1, "A"),
            make_player(2, "B"),
            make_player(3, "C"),
            make_player(4, "D"),
        ];
        let scheduled_match = ScheduledMatch {
            id: MatchId(1),
            round: RoundNumber(1),
            field: 1,
            team_a: vec![PlayerId(1), PlayerId(2)],
            team_b: vec![PlayerId(3), PlayerId(4)],
            status: MatchStatus::Completed,
        };
        let matches_by_id = HashMap::from([(scheduled_match.id, scheduled_match)]);

        let low_scoring_results: Vec<MatchResult> = (0..6)
            .map(|_| legacy_result(1, vec![(1, 6), (2, 5), (3, 5), (4, 4)]))
            .collect();
        let high_scoring_results: Vec<MatchResult> = (0..6)
            .map(|_| legacy_result(1, vec![(1, 12), (2, 10), (3, 10), (4, 8)]))
            .collect();

        let low_refs: Vec<_> = low_scoring_results.iter().collect();
        let high_refs: Vec<_> = high_scoring_results.iter().collect();
        let low_rankings =
            engine.compute_ratings(&players, &low_refs, Some(&matches_by_id), &config);
        let high_rankings =
            engine.compute_ratings(&players, &high_refs, Some(&matches_by_id), &config);

        let low_gap = low_rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(1))
            .unwrap()
            .rating
            - low_rankings
                .iter()
                .find(|ranking| ranking.player_id == PlayerId(4))
                .unwrap()
                .rating;
        let high_gap = high_rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(1))
            .unwrap()
            .rating
            - high_rankings
                .iter()
                .find(|ranking| ranking.player_id == PlayerId(4))
                .unwrap()
                .rating;

        assert!(
            high_gap <= low_gap * 1.20,
            "high-scoring marathons should not create much larger rating gaps: low={low_gap:.3}, high={high_gap:.3}"
        );
    }
}
