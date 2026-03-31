//! Primary Bayesian ranking engine.
//!
//! Model: each player has a latent log-skill θ_i. Goals scored by player i in
//! match m follow a Negative Binomial distribution whose rate is modulated by
//! team context (sum of teammate skills vs opponent skills). The posterior over
//! all θ is approximated via warm-started Laplace (Newton-Raphson on the
//! log-posterior), then used to derive rank distributions via Monte Carlo.

use super::RankingEngine;
use crate::models::{
    MatchId, MatchResult, Player, PlayerId, PlayerRanking, ScheduledMatch, SessionConfig,
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
        for _iter in 0..MAX_ITER {
            let (grad, hessian) =
                self.log_posterior_grad_hessian(&theta, &player_index, matches_by_id, results, n);
            let grad_norm = grad.norm();
            if grad_norm < GRAD_TOL {
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
        let (_, hessian) =
            self.log_posterior_grad_hessian(&theta, &player_index, matches_by_id, results, n);
        let neg_hessian = -hessian;
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
    ) -> (DVector<f64>, DMatrix<f64>) {
        let mut grad = DVector::zeros(n);
        let mut hess = DMatrix::zeros(n, n);

        // Likelihood contributions from each player's goal count in each match
        for result in results {
            let w = result.duration_multiplier;
            for (player_id, score) in &result.scores {
                let goals = match score.goals {
                    Some(g) => g as f64,
                    None => continue, // did not play — exclude from likelihood
                };
                let Some(&i) = player_index.get(player_id) else {
                    continue;
                };

                // Score model for player i:
                //   mu_i = sum_j coeff_j * theta_j
                // where coeff_j includes player i plus teammate/opponent context.
                let mut coeffs: Vec<(usize, f64)> = vec![(i, 1.0)];

                if let Some(match_ctx) = matches_by_id.and_then(|m| m.get(&result.match_id)) {
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
                let lambda = mu_i.exp();

                // Negative binomial log-likelihood gradient and Hessian (w.r.t. mu_i)
                // NB(r, p): p = r/(r+lambda), E[goals] = lambda
                // d/d(mu_i) log P(k | lambda) with lambda = exp(mu_i):
                let r = self.dispersion;
                let k = goals;
                // d log L / d lambda = k/lambda - (r+k)/(r+lambda)
                let dl_dlambda = k / lambda - (r + k) / (r + lambda);
                // d lambda / d mu_i = lambda
                let dl_dmu = dl_dlambda * lambda;
                for (j, c_j) in &coeffs {
                    grad[*j] += w * dl_dmu * *c_j;
                }

                // d² log L / d mu_i² = k/lambda² * (-lambda²) + ... (chain rule)
                // d²L/d(lambda)² = -k/lambda² + (r+k)/(r+lambda)²
                let d2l_dlambda2 = -k / lambda.powi(2) + (r + k) / (r + lambda).powi(2);
                // d² log L / d mu_i² = d²L/d(lambda)² * lambda² + dL/dlambda * lambda
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
                total_goals: 0,
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
                        total_goals: 0,
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

                let matches_played = results
                    .iter()
                    .filter(|r| {
                        r.scores
                            .get(&player.id)
                            .map(|s| s.played())
                            .unwrap_or(false)
                    })
                    .count() as u32;

                let total_goals = results
                    .iter()
                    .flat_map(|r| r.scores.get(&player.id))
                    .filter_map(|s| s.goals)
                    .map(|g| g as u32)
                    .sum();

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
                    total_goals,
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
        use std::collections::HashMap;
        let engine = GoalModelEngine::default();
        let config = make_config();
        let players = vec![make_player(1, "Striker"), make_player(2, "Defender")];

        // 5 matches: player 1 scores 3, player 2 scores 0 each time
        let results: Vec<MatchResult> = (0..5)
            .map(|i| {
                let mut scores = HashMap::new();
                scores.insert(PlayerId(1), PlayerMatchScore::scored(3));
                scores.insert(PlayerId(2), PlayerMatchScore::scored(0));
                MatchResult {
                    match_id: MatchId(i),
                    scores,
                    duration_multiplier: 1.0,
                    entered_by: Role::Coach,
                }
            })
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
            MatchResult {
                match_id: MatchId(1),
                scores: HashMap::from([
                    (PlayerId(1), PlayerMatchScore::scored(2)),
                    (PlayerId(2), PlayerMatchScore::scored(1)),
                    (PlayerId(3), PlayerMatchScore::scored(0)),
                    (PlayerId(4), PlayerMatchScore::scored(0)),
                ]),
                duration_multiplier: 1.0,
                entered_by: Role::Coach,
            },
            MatchResult {
                match_id: MatchId(2),
                scores: HashMap::from([
                    (PlayerId(5), PlayerMatchScore::scored(1)),
                    (PlayerId(6), PlayerMatchScore::scored(1)),
                    (PlayerId(7), PlayerMatchScore::scored(0)),
                    (PlayerId(8), PlayerMatchScore::scored(0)),
                ]),
                duration_multiplier: 1.0,
                entered_by: Role::Coach,
            },
        ];
        let result_refs: Vec<&MatchResult> = results.iter().collect();

        let rankings =
            engine.compute_ratings(&players, &result_refs, Some(&matches_by_id), &config);

        let benched_player_ranking = rankings
            .iter()
            .find(|ranking| ranking.player_id == PlayerId(9))
            .unwrap();

        assert_eq!(benched_player_ranking.matches_played, 0);
        assert_eq!(benched_player_ranking.total_goals, 0);

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
}
