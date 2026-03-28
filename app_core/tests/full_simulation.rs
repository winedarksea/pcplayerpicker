/// Full end-to-end offline simulation test.
///
/// 8 players with known latent skills. The test simulates 10 rounds of:
///   1. Generate schedule (round-robin cold start → info-max after first round)
///   2. Sample realistic goal counts from a negative binomial based on true skills
///   3. Enter results via SessionManager
///   4. Compute rankings via GoalModelEngine
///   5. Verify final Kendall tau correlation > 0.6 between true skill and estimated rank
///
/// This guards against regressions in the ranking engine, scheduler, and event system.
use app_core::models::*;
use app_core::ranking::goal_model::GoalModelEngine;
use app_core::ranking::RankingEngine;
use app_core::scheduler::select_scheduler;
use app_core::session::SessionManager;
use std::collections::HashMap;

// ── True skill parameters (log-scale) ─────────────────────────────────────────

const TRUE_SKILLS: [f64; 8] = [1.6, 1.2, 0.8, 0.4, 0.0, -0.4, -0.8, -1.2];
const N_PLAYERS: usize = 8;
const N_ROUNDS: usize = 10;
const TEAM_SIZE: u8 = 2;

/// Sample goals for a player using a simple Poisson approximation of the NB model.
/// rate = exp(skill + team_context) where team_context adjusts for opponent vs teammate.
fn sample_goals(player_idx: usize, teammates: &[usize], opponents: &[usize], rng_seed: u64) -> u16 {
    // Deterministic pseudo-random using a simple LCG derived from the seed
    let mut state = rng_seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);

    let skill_i = TRUE_SKILLS[player_idx];
    let teammate_boost: f64 = teammates.iter().map(|&j| TRUE_SKILLS[j] * 0.2).sum::<f64>();
    let opp_suppression: f64 = opponents
        .iter()
        .map(|&j| TRUE_SKILLS[j] * -0.3)
        .sum::<f64>();
    let eta = skill_i + teammate_boost + opp_suppression;
    let lambda = eta.exp().max(0.05).min(8.0);

    // Poisson sample via inverse CDF using the LCG random value in [0,1)
    state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let u = (state >> 33) as f64 / (u64::MAX >> 33) as f64;

    let mut k = 0u16;
    let mut cdf = (-lambda).exp();
    let mut acc = cdf;
    while acc < u && k < 20 {
        k += 1;
        cdf *= lambda / k as f64;
        acc += cdf;
    }
    k
}

/// Compute Kendall tau rank correlation between two orderings (represented as rank vectors).
/// Both slices must have the same length. Returns a value in [-1, 1].
fn kendall_tau(rank_a: &[u32], rank_b: &[u32]) -> f64 {
    let n = rank_a.len();
    let mut concordant = 0i64;
    let mut discordant = 0i64;
    for i in 0..n {
        for j in (i + 1)..n {
            let a_diff = rank_a[i].cmp(&rank_a[j]);
            let b_diff = rank_b[i].cmp(&rank_b[j]);
            if a_diff == b_diff {
                concordant += 1;
            } else if a_diff != std::cmp::Ordering::Equal && b_diff != std::cmp::Ordering::Equal {
                discordant += 1;
            }
        }
    }
    let denom = (n * (n - 1) / 2) as f64;
    (concordant - discordant) as f64 / denom
}

#[test]
fn full_offline_simulation_converges() {
    // Create session
    let config = SessionConfig::new(TEAM_SIZE, 1, Sport::Soccer);
    let mut manager = SessionManager::new(config);

    // Add 8 players
    let mut player_ids = Vec::new();
    for i in 0..N_PLAYERS {
        let id = manager.add_player(format!("P{}", i + 1));
        player_ids.push(id);
    }

    let mut match_id_counter = 0u32;

    for round in 0..N_ROUNDS {
        // Get current state for scheduling — only active players, matching the Scheduler trait contract
        let players: Vec<Player> = manager.state.active_players().cloned().collect();
        let rankings = manager.state.rankings.clone();
        let config = manager.state.config.clone().unwrap();

        let scheduler = select_scheduler(&rankings);
        let start_round = RoundNumber(round as u32 + 1);
        let scheduled = scheduler.generate_schedule(
            &players,
            &rankings,
            &config,
            &mut manager.rng,
            start_round,
            1,
        );

        // Inject generated matches into state via ScheduleGenerated event
        use app_core::events::Event;
        manager.log.append(
            Event::ScheduleGenerated {
                round: start_round,
                matches: scheduled.clone(),
            },
            Role::Coach,
        );
        manager.state = app_core::events::materialize(&manager.log);

        // Enter results for each scheduled match
        for sched_match in &scheduled {
            let mut scores = HashMap::new();
            let team_a: Vec<usize> = sched_match
                .team_a
                .iter()
                .filter_map(|pid| player_ids.iter().position(|id| id == pid))
                .collect();
            let team_b: Vec<usize> = sched_match
                .team_b
                .iter()
                .filter_map(|pid| player_ids.iter().position(|id| id == pid))
                .collect();

            for &idx in &team_a {
                let seed = (round as u64 * 1000 + match_id_counter as u64) * 100 + idx as u64;
                let goals = sample_goals(idx, &team_a, &team_b, seed);
                scores.insert(player_ids[idx], PlayerMatchScore::scored(goals));
            }
            for &idx in &team_b {
                let seed = (round as u64 * 1000 + match_id_counter as u64) * 100 + 50 + idx as u64;
                let goals = sample_goals(idx, &team_b, &team_a, seed);
                scores.insert(player_ids[idx], PlayerMatchScore::scored(goals));
            }

            match_id_counter += 1;
            manager.enter_score(
                MatchResult {
                    match_id: sched_match.id,
                    scores,
                    duration_multiplier: 1.0,
                    entered_by: Role::Coach,
                },
                Role::Coach,
            );
        }

        // Update rankings after each round
        let all_players: Vec<Player> = manager.state.players.values().cloned().collect();
        let completed_results: Vec<&MatchResult> = manager
            .state
            .results
            .values()
            .filter(|r| {
                manager
                    .state
                    .matches
                    .get(&r.match_id)
                    .map(|m| m.status == MatchStatus::Completed)
                    .unwrap_or(false)
            })
            .collect();

        if !completed_results.is_empty() {
            let engine = GoalModelEngine::default();
            let config = manager.state.config.clone().unwrap();
            let new_rankings = engine.compute_ratings(
                &all_players,
                &completed_results,
                Some(&manager.state.matches),
                &config,
            );
            use app_core::events::Event;
            let current_round = manager.state.current_round;
            manager.log.append(
                Event::RankingsComputed {
                    round: current_round,
                    rankings: new_rankings,
                },
                Role::Coach,
            );
            manager.state = app_core::events::materialize(&manager.log);
        }
    }

    // ── Verify convergence ─────────────────────────────────────────────────
    let final_rankings = &manager.state.rankings;
    assert!(
        !final_rankings.is_empty(),
        "rankings should be non-empty after simulation"
    );
    assert_eq!(
        final_rankings.len(),
        N_PLAYERS,
        "all players should be ranked"
    );

    // True skill rank: player with highest TRUE_SKILL = rank 1
    let mut true_order: Vec<(usize, f64)> = TRUE_SKILLS.iter().copied().enumerate().collect();
    true_order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let mut true_rank = vec![0u32; N_PLAYERS];
    for (rank, (idx, _)) in true_order.iter().enumerate() {
        true_rank[*idx] = rank as u32 + 1;
    }

    // Estimated rank from the engine
    let mut est_rank = vec![0u32; N_PLAYERS];
    for ranking in final_rankings {
        let idx = player_ids
            .iter()
            .position(|id| id == &ranking.player_id)
            .unwrap();
        est_rank[idx] = ranking.rank;
    }

    let tau = kendall_tau(&true_rank, &est_rank);
    println!(
        "Kendall tau after {} rounds: {:.3}  (true: {:?}, est: {:?})",
        N_ROUNDS, tau, true_rank, est_rank
    );
    assert!(
        tau > 0.6,
        "Kendall tau should be > 0.6 after {} rounds, got {:.3}\n  true_rank: {:?}\n  est_rank:  {:?}",
        N_ROUNDS, tau, true_rank, est_rank
    );

    // Also verify each player has played at least 1 match
    for ranking in final_rankings {
        assert!(
            ranking.matches_played > 0,
            "every player should have played at least 1 match"
        );
    }

    // Uncertainty should shrink after 10 rounds relative to initial sigma
    let avg_uncertainty: f64 =
        final_rankings.iter().map(|r| r.uncertainty).sum::<f64>() / N_PLAYERS as f64;
    assert!(
        avg_uncertainty < 1.5,
        "average uncertainty should shrink after many matches, got {:.3}",
        avg_uncertainty
    );
}

#[test]
fn event_replay_is_deterministic() {
    // Replay the same event log twice and verify identical state
    let config = SessionConfig::new(2, 1, Sport::Soccer);
    let mut manager = SessionManager::new(config);
    for i in 0..4 {
        manager.add_player(format!("P{}", i + 1));
    }

    // Replay from saved log
    let saved_log = manager.log.clone();
    let replayed = SessionManager::from_log(saved_log);

    assert_eq!(manager.state.players.len(), replayed.state.players.len());
    assert_eq!(
        manager.state.config.as_ref().map(|c| c.id.to_string()),
        replayed.state.config.as_ref().map(|c| c.id.to_string()),
    );
}

#[test]
fn rng_is_deterministic() {
    let config = SessionConfig::new(2, 1, Sport::Soccer);
    let mut m1 = SessionManager::new(config.clone());
    let mut m2 = SessionManager::new(config);

    // Override second manager's RNG to have same seed as first
    let id = m1.state.config.as_ref().unwrap().id.clone();
    m2.rng = app_core::rng::SessionRng::new(&id);
    m1.rng = app_core::rng::SessionRng::new(&id);

    // Add players to both
    for i in 0..6 {
        m1.add_player(format!("P{}", i + 1));
        m2.add_player(format!("P{}", i + 1));
    }

    let players: Vec<Player> = m1.state.players.values().cloned().collect();
    let config1 = m1.state.config.clone().unwrap();
    let config2 = m2.state.config.clone().unwrap();

    let scheduler = select_scheduler(&[]);
    let sched1 =
        scheduler.generate_schedule(&players, &[], &config1, &mut m1.rng, RoundNumber(1), 1);
    let sched2 =
        scheduler.generate_schedule(&players, &[], &config2, &mut m2.rng, RoundNumber(1), 1);

    // Same seed → same schedule
    assert_eq!(
        sched1.len(),
        sched2.len(),
        "same seed should produce same number of matches"
    );
    for (a, b) in sched1.iter().zip(sched2.iter()) {
        assert_eq!(a.team_a, b.team_a, "team_a should match for same seed");
        assert_eq!(a.team_b, b.team_b, "team_b should match for same seed");
    }
}

#[test]
fn dropout_excluded_from_scheduling() {
    let config = SessionConfig::new(2, 1, Sport::Soccer);
    let mut manager = SessionManager::new(config);
    let ids: Vec<PlayerId> = (0..6)
        .map(|i| manager.add_player(format!("P{}", i + 1)))
        .collect();

    // Deactivate one player
    manager.deactivate_player(ids[0]);

    let active: Vec<PlayerId> = manager.state.active_players().map(|p| p.id).collect();
    assert_eq!(
        active.len(),
        5,
        "deactivated player should not appear in active list"
    );
    assert!(
        !active.contains(&ids[0]),
        "deactivated player should not be in active set"
    );

    // Rankings should still include the inactive player
    let all_players_count = manager.state.players.len();
    assert_eq!(all_players_count, 6, "all players retained in state");
}

#[test]
fn voided_match_excluded_from_rankings() {
    use app_core::events::Event;
    use app_core::models::{MatchId, MatchStatus, RoundNumber, ScheduledMatch};

    let config = SessionConfig::new(2, 1, Sport::Soccer);
    let mut manager = SessionManager::new(config);
    let ids: Vec<PlayerId> = (0..4)
        .map(|i| manager.add_player(format!("P{}", i + 1)))
        .collect();

    // Schedule one match manually
    let m = ScheduledMatch {
        id: MatchId(1001),
        round: RoundNumber(1),
        field: 1,
        team_a: vec![ids[0], ids[1]],
        team_b: vec![ids[2], ids[3]],
        status: MatchStatus::Scheduled,
    };
    manager.log.append(
        Event::ScheduleGenerated {
            round: RoundNumber(1),
            matches: vec![m],
        },
        Role::Coach,
    );
    manager.state = app_core::events::materialize(&manager.log);

    // Enter a score so the match is completed
    let mut scores = HashMap::new();
    for &id in &ids {
        scores.insert(id, PlayerMatchScore::scored(2));
    }
    manager.enter_score(
        MatchResult {
            match_id: MatchId(1001),
            scores,
            duration_multiplier: 1.0,
            entered_by: Role::Coach,
        },
        Role::Coach,
    );
    assert_eq!(
        manager.completed_match_results().len(),
        1,
        "match should be completed before voiding"
    );

    // Void the match
    manager.void_match(MatchId(1001));

    // Voided match must not appear in completed results fed to the ranking engine
    assert_eq!(
        manager.completed_match_results().len(),
        0,
        "voided match should be excluded from completed results"
    );
    assert_eq!(
        manager.state.matches[&MatchId(1001)].status,
        MatchStatus::Voided,
        "match status should be Voided"
    );
}

#[test]
fn score_correction_overrides_original() {
    use app_core::events::Event;
    use app_core::models::{MatchId, MatchStatus, RoundNumber, ScheduledMatch};

    let config = SessionConfig::new(2, 1, Sport::Soccer);
    let mut manager = SessionManager::new(config);
    let ids: Vec<PlayerId> = (0..4)
        .map(|i| manager.add_player(format!("P{}", i + 1)))
        .collect();

    let m = ScheduledMatch {
        id: MatchId(1001),
        round: RoundNumber(1),
        field: 1,
        team_a: vec![ids[0], ids[1]],
        team_b: vec![ids[2], ids[3]],
        status: MatchStatus::Scheduled,
    };
    manager.log.append(
        Event::ScheduleGenerated {
            round: RoundNumber(1),
            matches: vec![m],
        },
        Role::Coach,
    );
    manager.state = app_core::events::materialize(&manager.log);

    // Enter initial score
    let mut scores = HashMap::new();
    scores.insert(ids[0], PlayerMatchScore::scored(1));
    scores.insert(ids[1], PlayerMatchScore::scored(1));
    scores.insert(ids[2], PlayerMatchScore::scored(0));
    scores.insert(ids[3], PlayerMatchScore::scored(0));
    manager.enter_score(
        MatchResult {
            match_id: MatchId(1001),
            scores,
            duration_multiplier: 1.0,
            entered_by: Role::Coach,
        },
        Role::Coach,
    );

    // Correct the score
    let mut corrected = HashMap::new();
    corrected.insert(ids[0], PlayerMatchScore::scored(5));
    corrected.insert(ids[1], PlayerMatchScore::scored(5));
    corrected.insert(ids[2], PlayerMatchScore::scored(0));
    corrected.insert(ids[3], PlayerMatchScore::scored(0));
    manager.correct_score(MatchResult {
        match_id: MatchId(1001),
        scores: corrected,
        duration_multiplier: 1.0,
        entered_by: Role::Coach,
    });

    let result = manager.state.results.get(&MatchId(1001)).unwrap();
    assert_eq!(
        result.scores[&ids[0]].goals,
        Some(5),
        "corrected score should replace original"
    );
    assert_eq!(
        result.scores[&ids[2]].goals,
        Some(0),
        "opponent score should be as corrected"
    );
}

#[test]
fn reactivated_player_returns_to_active() {
    let config = SessionConfig::new(2, 1, Sport::Soccer);
    let mut manager = SessionManager::new(config);
    let id = manager.add_player("Alice".into());

    manager.deactivate_player(id);
    assert_eq!(
        manager.state.active_players().count(),
        0,
        "deactivated player should not be active"
    );

    manager.reactivate_player(id);
    assert_eq!(
        manager.state.active_players().count(),
        1,
        "reactivated player should be active again"
    );
    assert!(
        manager.active_player_ids().contains(&id),
        "reactivated player should appear in active_player_ids()"
    );
    assert!(
        manager.state.players[&id].deactivated_at_round.is_none(),
        "deactivated_at_round should be cleared on reactivation"
    );
}
