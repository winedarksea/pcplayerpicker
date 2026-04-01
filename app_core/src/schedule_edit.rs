use crate::models::{
    MatchId, MatchStatus, PlayerId, PlayerStatus, RoundNumber, ScheduledMatch, SessionState,
};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RoundScheduleEditError {
    #[error("session config is missing")]
    MissingSessionConfig,
    #[error("round {round} has no editable matches")]
    RoundHasNoEditableMatches { round: u32 },
    #[error("match {match_id} is not editable in round {round}")]
    UnexpectedMatchId { round: u32, match_id: u32 },
    #[error("editable match {match_id} was not included in the round update")]
    MissingEditableMatch { match_id: u32 },
    #[error("match {match_id} moved from round {expected_round} to round {actual_round}")]
    RoundChanged {
        match_id: u32,
        expected_round: u32,
        actual_round: u32,
    },
    #[error("match {match_id} changed fields from {expected_field} to {actual_field}")]
    FieldChanged {
        match_id: u32,
        expected_field: u8,
        actual_field: u8,
    },
    #[error("match {match_id} changed status from {expected_status:?} to {actual_status:?}")]
    StatusChanged {
        match_id: u32,
        expected_status: MatchStatus,
        actual_status: MatchStatus,
    },
    #[error("match {match_id} team A has {actual_size} players; expected {expected_size}")]
    TeamSizeMismatch {
        match_id: u32,
        team_label: &'static str,
        actual_size: usize,
        expected_size: usize,
    },
    #[error("match {match_id} references unknown player {player_id}")]
    UnknownPlayer { match_id: u32, player_id: u32 },
    #[error("match {match_id} references inactive player {player_id}")]
    InactivePlayer { match_id: u32, player_id: u32 },
    #[error("player {player_id} appears twice in match {match_id}")]
    DuplicatePlayerInMatch { match_id: u32, player_id: u32 },
    #[error("player {player_id} appears in both match {first_match_id} and match {second_match_id} in round {round}")]
    DuplicatePlayerInRound {
        round: u32,
        player_id: u32,
        first_match_id: u32,
        second_match_id: u32,
    },
}

pub fn validate_round_schedule_update(
    state: &SessionState,
    round: RoundNumber,
    updated_matches: &[ScheduledMatch],
) -> Result<(), RoundScheduleEditError> {
    let Some(config) = state.config.as_ref() else {
        return Err(RoundScheduleEditError::MissingSessionConfig);
    };

    let editable_matches_by_id: HashMap<MatchId, &ScheduledMatch> = state
        .matches
        .values()
        .filter(|scheduled_match| {
            scheduled_match.round == round
                && scheduled_match.status != MatchStatus::Completed
                && scheduled_match.status != MatchStatus::Voided
        })
        .map(|scheduled_match| (scheduled_match.id, scheduled_match))
        .collect();

    if editable_matches_by_id.is_empty() {
        return Err(RoundScheduleEditError::RoundHasNoEditableMatches { round: round.0 });
    }

    let locked_matches: Vec<&ScheduledMatch> = state
        .matches
        .values()
        .filter(|scheduled_match| {
            scheduled_match.round == round && scheduled_match.status == MatchStatus::Completed
        })
        .collect();

    let expected_team_size = usize::from(config.team_size);
    let mut seen_updated_match_ids = HashSet::new();
    let mut players_seen_in_round: HashMap<PlayerId, MatchId> = HashMap::new();

    for locked_match in locked_matches {
        register_match_players_in_round(
            locked_match,
            state,
            round,
            expected_team_size,
            &mut players_seen_in_round,
        )?;
    }

    for updated_match in updated_matches {
        let Some(existing_match) = editable_matches_by_id.get(&updated_match.id).copied() else {
            return Err(RoundScheduleEditError::UnexpectedMatchId {
                round: round.0,
                match_id: updated_match.id.0,
            });
        };

        seen_updated_match_ids.insert(updated_match.id);

        if updated_match.round != existing_match.round {
            return Err(RoundScheduleEditError::RoundChanged {
                match_id: updated_match.id.0,
                expected_round: existing_match.round.0,
                actual_round: updated_match.round.0,
            });
        }

        if updated_match.field != existing_match.field {
            return Err(RoundScheduleEditError::FieldChanged {
                match_id: updated_match.id.0,
                expected_field: existing_match.field,
                actual_field: updated_match.field,
            });
        }

        if updated_match.status != existing_match.status {
            return Err(RoundScheduleEditError::StatusChanged {
                match_id: updated_match.id.0,
                expected_status: existing_match.status.clone(),
                actual_status: updated_match.status.clone(),
            });
        }

        register_match_players_in_round(
            updated_match,
            state,
            round,
            expected_team_size,
            &mut players_seen_in_round,
        )?;
    }

    for editable_match_id in editable_matches_by_id.keys() {
        if !seen_updated_match_ids.contains(editable_match_id) {
            return Err(RoundScheduleEditError::MissingEditableMatch {
                match_id: editable_match_id.0,
            });
        }
    }

    Ok(())
}

fn register_match_players_in_round(
    scheduled_match: &ScheduledMatch,
    state: &SessionState,
    round: RoundNumber,
    expected_team_size: usize,
    players_seen_in_round: &mut HashMap<PlayerId, MatchId>,
) -> Result<(), RoundScheduleEditError> {
    validate_team_size(
        scheduled_match.id,
        "A",
        scheduled_match.team_a.len(),
        expected_team_size,
    )?;
    validate_team_size(
        scheduled_match.id,
        "B",
        scheduled_match.team_b.len(),
        expected_team_size,
    )?;

    let mut players_seen_in_match = HashSet::new();
    for player_id in scheduled_match.team_a.iter().chain(scheduled_match.team_b.iter()) {
        let Some(player) = state.players.get(player_id) else {
            return Err(RoundScheduleEditError::UnknownPlayer {
                match_id: scheduled_match.id.0,
                player_id: player_id.0,
            });
        };

        if player.status != PlayerStatus::Active {
            return Err(RoundScheduleEditError::InactivePlayer {
                match_id: scheduled_match.id.0,
                player_id: player_id.0,
            });
        }

        if !players_seen_in_match.insert(*player_id) {
            return Err(RoundScheduleEditError::DuplicatePlayerInMatch {
                match_id: scheduled_match.id.0,
                player_id: player_id.0,
            });
        }

        if let Some(first_match_id) = players_seen_in_round.insert(*player_id, scheduled_match.id) {
            return Err(RoundScheduleEditError::DuplicatePlayerInRound {
                round: round.0,
                player_id: player_id.0,
                first_match_id: first_match_id.0,
                second_match_id: scheduled_match.id.0,
            });
        }
    }

    Ok(())
}

fn validate_team_size(
    match_id: MatchId,
    team_label: &'static str,
    actual_size: usize,
    expected_size: usize,
) -> Result<(), RoundScheduleEditError> {
    if actual_size == expected_size {
        return Ok(());
    }

    Err(RoundScheduleEditError::TeamSizeMismatch {
        match_id: match_id.0,
        team_label,
        actual_size,
        expected_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Event;
    use crate::models::{MatchResult, Role, RoundNumber, SessionConfig, Sport};
    use crate::session::SessionManager;
    use std::collections::HashMap;

    fn build_manager_with_two_matches() -> SessionManager {
        let mut manager = SessionManager::new(SessionConfig::new(2, 1, Sport::Soccer));
        for name in ["Alice", "Bea", "Cara", "Dani", "Elle", "Fran", "Gia", "Hana"] {
            manager.add_player(name.to_string());
        }
        manager.log.append(
            Event::ScheduleGenerated {
                round: RoundNumber(1),
                matches: vec![
                    ScheduledMatch {
                        id: MatchId(1),
                        round: RoundNumber(1),
                        field: 1,
                        team_a: vec![PlayerId(1), PlayerId(2)],
                        team_b: vec![PlayerId(3), PlayerId(4)],
                        status: MatchStatus::Scheduled,
                    },
                    ScheduledMatch {
                        id: MatchId(2),
                        round: RoundNumber(1),
                        field: 2,
                        team_a: vec![PlayerId(5), PlayerId(6)],
                        team_b: vec![PlayerId(7), PlayerId(8)],
                        status: MatchStatus::Scheduled,
                    },
                ],
            },
            Role::Coach,
        );
        manager.state = crate::events::materialize(&manager.log);
        manager
    }

    #[test]
    fn benched_player_update_is_valid() {
        let mut manager = build_manager_with_two_matches();
        manager.add_player("Ivy".to_string());

        let updated_matches = vec![
            ScheduledMatch {
                id: MatchId(1),
                round: RoundNumber(1),
                field: 1,
                team_a: vec![PlayerId(9), PlayerId(2)],
                team_b: vec![PlayerId(3), PlayerId(4)],
                status: MatchStatus::Scheduled,
            },
            manager.state.matches[&MatchId(2)].clone(),
        ];

        manager
            .apply_round_schedule_update(RoundNumber(1), updated_matches)
            .expect("valid benched swap should apply");

        let updated_match = &manager.state.matches[&MatchId(1)];
        assert_eq!(updated_match.team_a[0], PlayerId(9));
    }

    #[test]
    fn round_update_can_move_player_between_matches_without_duplicates() {
        let mut manager = build_manager_with_two_matches();
        manager.add_player("Ivy".to_string());

        let updated_matches = vec![
            ScheduledMatch {
                id: MatchId(1),
                round: RoundNumber(1),
                field: 1,
                team_a: vec![PlayerId(5), PlayerId(2)],
                team_b: vec![PlayerId(3), PlayerId(4)],
                status: MatchStatus::Scheduled,
            },
            ScheduledMatch {
                id: MatchId(2),
                round: RoundNumber(1),
                field: 2,
                team_a: vec![PlayerId(9), PlayerId(6)],
                team_b: vec![PlayerId(7), PlayerId(8)],
                status: MatchStatus::Scheduled,
            },
        ];

        manager
            .apply_round_schedule_update(RoundNumber(1), updated_matches)
            .expect("valid move should apply");

        let round_players: Vec<PlayerId> = manager
            .state
            .matches
            .values()
            .filter(|scheduled_match| scheduled_match.round == RoundNumber(1))
            .flat_map(|scheduled_match| {
                scheduled_match
                    .team_a
                    .iter()
                    .chain(scheduled_match.team_b.iter())
                    .copied()
                    .collect::<Vec<_>>()
            })
            .collect();
        let unique_players: HashSet<PlayerId> = round_players.iter().copied().collect();
        assert_eq!(round_players.len(), unique_players.len());
    }

    #[test]
    fn duplicate_player_in_round_is_rejected() {
        let manager = build_manager_with_two_matches();
        let updated_matches = vec![
            ScheduledMatch {
                id: MatchId(1),
                round: RoundNumber(1),
                field: 1,
                team_a: vec![PlayerId(5), PlayerId(2)],
                team_b: vec![PlayerId(3), PlayerId(4)],
                status: MatchStatus::Scheduled,
            },
            manager.state.matches[&MatchId(2)].clone(),
        ];

        let error = validate_round_schedule_update(&manager.state, RoundNumber(1), &updated_matches)
            .expect_err("duplicate player should be rejected");

        assert!(matches!(
            error,
            RoundScheduleEditError::DuplicatePlayerInRound {
                round: 1,
                player_id: 5,
                first_match_id: _,
                second_match_id: _
            }
        ));
    }

    #[test]
    fn incomplete_match_roster_is_rejected() {
        let manager = build_manager_with_two_matches();
        let updated_matches = vec![
            ScheduledMatch {
                id: MatchId(1),
                round: RoundNumber(1),
                field: 1,
                team_a: vec![PlayerId(1)],
                team_b: vec![PlayerId(3), PlayerId(4)],
                status: MatchStatus::Scheduled,
            },
            manager.state.matches[&MatchId(2)].clone(),
        ];

        let error = validate_round_schedule_update(&manager.state, RoundNumber(1), &updated_matches)
            .expect_err("incomplete roster should be rejected");

        assert!(matches!(
            error,
            RoundScheduleEditError::TeamSizeMismatch {
                match_id: 1,
                team_label: "A",
                actual_size: 1,
                expected_size: 2,
            }
        ));
    }

    #[test]
    fn completed_match_duplicates_are_rejected() {
        let mut manager = build_manager_with_two_matches();
        let completed_result = MatchResult {
            match_id: MatchId(2),
            scores: HashMap::new(),
            duration_multiplier: 1.0,
            entered_by: Role::Coach,
        };
        manager.enter_score(completed_result, Role::Coach);

        let updated_matches = vec![ScheduledMatch {
            id: MatchId(1),
            round: RoundNumber(1),
            field: 1,
            team_a: vec![PlayerId(5), PlayerId(2)],
            team_b: vec![PlayerId(3), PlayerId(4)],
            status: MatchStatus::Scheduled,
        }];

        let error = validate_round_schedule_update(&manager.state, RoundNumber(1), &updated_matches)
            .expect_err("players locked into completed matches must remain unique");

        assert!(matches!(
            error,
            RoundScheduleEditError::DuplicatePlayerInRound {
                round: 1,
                player_id: 5,
                first_match_id: 2,
                second_match_id: 1,
            }
        ));
    }
}
