use crate::models::{
    EventId, MatchId, MatchResult, MatchStatus, Player, PlayerId, PlayerRanking, PlayerStatus,
    Role, RoundNumber, ScheduledMatch, SessionConfig, SessionState,
};
use serde::{Deserialize, Serialize};

// ── Event Definitions ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub id: EventId,
    pub session_version: u64,
    pub entered_by: Role,
    pub payload: Event,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    SessionCreated(SessionConfig),
    PlayerAdded(Player),
    PlayerDeactivated {
        player_id: PlayerId,
        at_round: RoundNumber,
    },
    PlayerReactivated {
        player_id: PlayerId,
        at_round: RoundNumber,
    },
    ScheduleGenerated {
        round: RoundNumber,
        matches: Vec<ScheduledMatch>,
    },
    ScoreEntered {
        match_id: MatchId,
        result: MatchResult,
    },
    ScoreCorrected {
        match_id: MatchId,
        result: MatchResult,
    },
    PlayerSwapped {
        match_id: MatchId,
        old_player: PlayerId,
        new_player: PlayerId,
    },
    MatchVoided {
        match_id: MatchId,
    },
    SessionReseeded {
        new_count: u32,
    },
    RankingsComputed {
        round: RoundNumber,
        rankings: Vec<PlayerRanking>,
    },
}

// ── Event Log ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct EventLog {
    events: Vec<EventEnvelope>,
    next_id: u64,
}

impl EventLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, payload: Event, entered_by: Role) -> EventId {
        let id = EventId(self.next_id);
        self.next_id += 1;
        let version = self.events.len() as u64;
        self.events.push(EventEnvelope {
            id,
            session_version: version,
            entered_by,
            payload,
        });
        id
    }

    pub fn events_since(&self, since: Option<EventId>) -> &[EventEnvelope] {
        match since {
            None => &self.events,
            Some(EventId(n)) => {
                let start = self.events.partition_point(|e| e.id.0 <= n);
                &self.events[start..]
            }
        }
    }

    pub fn all(&self) -> &[EventEnvelope] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Reconstruct an EventLog from a previously-serialized slice of envelopes
    /// (e.g. loaded from localStorage). The `next_id` is set to one past the
    /// highest seen event id so new appends stay unique.
    pub fn from_saved(events: Vec<EventEnvelope>) -> Self {
        let next_id = events.iter().map(|e| e.id.0 + 1).max().unwrap_or(0);
        Self { events, next_id }
    }
}

// ── Materialization ──────────────────────────────────────────────────────────

/// Replay the full event log to derive current session state.
/// Coach-entered scores override assistant-entered scores for the same match.
pub fn materialize(log: &EventLog) -> SessionState {
    let mut state = SessionState::default();

    for envelope in log.all() {
        apply_event(&mut state, &envelope.payload, &envelope.entered_by);
    }

    state.current_round = derive_current_round(&state.matches);
    state
}

fn derive_current_round(
    matches: &std::collections::HashMap<MatchId, ScheduledMatch>,
) -> RoundNumber {
    if matches.is_empty() {
        return RoundNumber(1);
    }

    let mut rounds: Vec<RoundNumber> = matches.values().map(|m| m.round).collect();
    rounds.sort_unstable();
    rounds.dedup();

    for round in &rounds {
        let pending = matches.values().any(|m| {
            m.round == *round
                && m.status != MatchStatus::Completed
                && m.status != MatchStatus::Voided
        });
        if pending {
            return *round;
        }
    }

    RoundNumber(rounds.last().map(|r| r.0).unwrap_or(0) + 1)
}

fn apply_event(state: &mut SessionState, event: &Event, entered_by: &Role) {
    match event {
        Event::SessionCreated(config) => {
            state.config = Some(config.clone());
        }

        Event::PlayerAdded(player) => {
            state.players.insert(player.id, player.clone());
        }

        Event::PlayerDeactivated {
            player_id,
            at_round,
        } => {
            if let Some(player) = state.players.get_mut(player_id) {
                player.status = PlayerStatus::Inactive;
                player.deactivated_at_round = Some(*at_round);
            }
        }

        Event::PlayerReactivated {
            player_id,
            at_round: _,
        } => {
            if let Some(player) = state.players.get_mut(player_id) {
                player.status = PlayerStatus::Active;
                player.deactivated_at_round = None;
            }
        }

        Event::ScheduleGenerated { round, matches } => {
            state.current_round = *round;
            for m in matches {
                state.matches.insert(m.id, m.clone());
            }
        }

        Event::ScoreEntered { match_id, result } => {
            // Last write wins for equal roles; coach always beats assistant.
            let should_apply = match state.results.get(match_id) {
                None => true,
                Some(existing) if existing.entered_by == *entered_by => true,
                Some(existing) => *entered_by == Role::Coach && existing.entered_by != Role::Coach,
            };
            if should_apply {
                state.results.insert(*match_id, result.clone());
                if let Some(m) = state.matches.get_mut(match_id) {
                    m.status = MatchStatus::Completed;
                }
            }
        }

        Event::ScoreCorrected { match_id, result } => {
            // Corrections always override (coach-only operation)
            state.results.insert(*match_id, result.clone());
        }

        Event::PlayerSwapped {
            match_id,
            old_player,
            new_player,
        } => {
            if let Some(m) = state.matches.get_mut(match_id) {
                for team in [&mut m.team_a, &mut m.team_b] {
                    for pid in team.iter_mut() {
                        if *pid == *old_player {
                            *pid = *new_player;
                        }
                    }
                }
            }
            // Also update result if present
            if let Some(result) = state.results.get_mut(match_id) {
                if let Some(score) = result.scores.remove(old_player) {
                    result.scores.insert(*new_player, score);
                }
            }
        }

        Event::MatchVoided { match_id } => {
            if let Some(m) = state.matches.get_mut(match_id) {
                m.status = MatchStatus::Voided;
            }
            // Result is retained in the log but excluded from ranking by checking match status
        }

        Event::SessionReseeded { new_count } => {
            if let Some(config) = &mut state.config {
                config.reseed_count = *new_count;
            }
        }

        Event::RankingsComputed { rankings, .. } => {
            state.rankings = rankings.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;

    fn make_config() -> SessionConfig {
        SessionConfig::new(2, 1, Sport::Soccer)
    }

    #[test]
    fn materialize_session_created() {
        let mut log = EventLog::new();
        let config = make_config();
        log.append(Event::SessionCreated(config.clone()), Role::Coach);
        let state = materialize(&log);
        assert!(state.config.is_some());
        assert_eq!(state.config.unwrap().team_size, 2);
    }

    #[test]
    fn materialize_players() {
        let mut log = EventLog::new();
        log.append(Event::SessionCreated(make_config()), Role::Coach);
        log.append(
            Event::PlayerAdded(Player {
                id: PlayerId(1),
                name: "Alice".into(),
                status: PlayerStatus::Active,
                joined_at_round: RoundNumber(1),
                deactivated_at_round: None,
            }),
            Role::Coach,
        );
        let state = materialize(&log);
        assert_eq!(state.players.len(), 1);
        assert_eq!(state.players[&PlayerId(1)].name, "Alice");
    }

    #[test]
    fn coach_score_beats_assistant_score() {
        use crate::models::PlayerMatchScore;
        use std::collections::HashMap;

        let mut log = EventLog::new();
        log.append(Event::SessionCreated(make_config()), Role::Coach);

        let assistant_result = MatchResult {
            match_id: MatchId(1),
            scores: {
                let mut m = HashMap::new();
                m.insert(PlayerId(1), PlayerMatchScore::scored(1));
                m
            },
            duration_multiplier: 1.0,
            entered_by: Role::Assistant,
        };
        let coach_result = MatchResult {
            match_id: MatchId(1),
            scores: {
                let mut m = HashMap::new();
                m.insert(PlayerId(1), PlayerMatchScore::scored(3));
                m
            },
            duration_multiplier: 1.0,
            entered_by: Role::Coach,
        };

        log.append(
            Event::ScoreEntered {
                match_id: MatchId(1),
                result: assistant_result,
            },
            Role::Assistant,
        );
        log.append(
            Event::ScoreEntered {
                match_id: MatchId(1),
                result: coach_result,
            },
            Role::Coach,
        );

        let state = materialize(&log);
        let result = &state.results[&MatchId(1)];
        assert_eq!(result.scores[&PlayerId(1)].goals, Some(3)); // coach value wins
    }

    #[test]
    fn player_deactivation() {
        let mut log = EventLog::new();
        log.append(Event::SessionCreated(make_config()), Role::Coach);
        log.append(
            Event::PlayerAdded(Player {
                id: PlayerId(1),
                name: "Bob".into(),
                status: PlayerStatus::Active,
                joined_at_round: RoundNumber(1),
                deactivated_at_round: None,
            }),
            Role::Coach,
        );
        log.append(
            Event::PlayerDeactivated {
                player_id: PlayerId(1),
                at_round: RoundNumber(3),
            },
            Role::Coach,
        );
        let state = materialize(&log);
        assert_eq!(state.players[&PlayerId(1)].status, PlayerStatus::Inactive);
        assert_eq!(state.active_players().count(), 0);
    }

    #[test]
    fn assistant_last_write_wins_for_equal_role() {
        use crate::models::PlayerMatchScore;
        use std::collections::HashMap;

        let mut log = EventLog::new();
        log.append(Event::SessionCreated(make_config()), Role::Coach);

        let assistant_result_1 = MatchResult {
            match_id: MatchId(1),
            scores: {
                let mut m = HashMap::new();
                m.insert(PlayerId(1), PlayerMatchScore::scored(1));
                m
            },
            duration_multiplier: 1.0,
            entered_by: Role::Assistant,
        };
        let assistant_result_2 = MatchResult {
            match_id: MatchId(1),
            scores: {
                let mut m = HashMap::new();
                m.insert(PlayerId(1), PlayerMatchScore::scored(2));
                m
            },
            duration_multiplier: 1.0,
            entered_by: Role::Assistant,
        };

        log.append(
            Event::ScoreEntered {
                match_id: MatchId(1),
                result: assistant_result_1,
            },
            Role::Assistant,
        );
        log.append(
            Event::ScoreEntered {
                match_id: MatchId(1),
                result: assistant_result_2,
            },
            Role::Assistant,
        );

        let state = materialize(&log);
        let result = &state.results[&MatchId(1)];
        assert_eq!(result.scores[&PlayerId(1)].goals, Some(2));
    }

    #[test]
    fn completed_round_advances_to_next_scheduled_round() {
        use crate::models::PlayerMatchScore;
        use std::collections::HashMap;

        let mut log = EventLog::new();
        log.append(Event::SessionCreated(make_config()), Role::Coach);
        log.append(
            Event::ScheduleGenerated {
                round: RoundNumber(1),
                matches: vec![
                    ScheduledMatch {
                        id: MatchId(1),
                        round: RoundNumber(1),
                        field: 1,
                        team_a: vec![PlayerId(1)],
                        team_b: vec![PlayerId(2)],
                        status: MatchStatus::Scheduled,
                    },
                    ScheduledMatch {
                        id: MatchId(2),
                        round: RoundNumber(2),
                        field: 1,
                        team_a: vec![PlayerId(1)],
                        team_b: vec![PlayerId(2)],
                        status: MatchStatus::Scheduled,
                    },
                ],
            },
            Role::Coach,
        );

        let completed_round_1 = MatchResult {
            match_id: MatchId(1),
            scores: {
                let mut m = HashMap::new();
                m.insert(PlayerId(1), PlayerMatchScore::scored(1));
                m.insert(PlayerId(2), PlayerMatchScore::scored(0));
                m
            },
            duration_multiplier: 1.0,
            entered_by: Role::Coach,
        };
        log.append(
            Event::ScoreEntered {
                match_id: MatchId(1),
                result: completed_round_1,
            },
            Role::Coach,
        );

        let state = materialize(&log);
        assert_eq!(state.current_round, RoundNumber(2));
    }

    #[test]
    fn all_completed_rounds_advance_to_next_unscheduled_round() {
        use crate::models::PlayerMatchScore;
        use std::collections::HashMap;

        let mut log = EventLog::new();
        log.append(Event::SessionCreated(make_config()), Role::Coach);
        log.append(
            Event::ScheduleGenerated {
                round: RoundNumber(1),
                matches: vec![ScheduledMatch {
                    id: MatchId(1),
                    round: RoundNumber(1),
                    field: 1,
                    team_a: vec![PlayerId(1)],
                    team_b: vec![PlayerId(2)],
                    status: MatchStatus::Scheduled,
                }],
            },
            Role::Coach,
        );

        let completed_round_1 = MatchResult {
            match_id: MatchId(1),
            scores: {
                let mut m = HashMap::new();
                m.insert(PlayerId(1), PlayerMatchScore::scored(1));
                m.insert(PlayerId(2), PlayerMatchScore::scored(0));
                m
            },
            duration_multiplier: 1.0,
            entered_by: Role::Coach,
        };
        log.append(
            Event::ScoreEntered {
                match_id: MatchId(1),
                result: completed_round_1,
            },
            Role::Coach,
        );

        let state = materialize(&log);
        assert_eq!(state.current_round, RoundNumber(2));
    }
}
