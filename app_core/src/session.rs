/// SessionManager orchestrates the core loop:
/// receive results → re-rank → generate next schedule → emit events.
///
/// This is a pure logic layer — no I/O. The caller (client_app state module)
/// persists events and rankings via the DataStore trait.
use crate::events::{Event, EventLog};
use crate::io::csv::ImportedResults;
use crate::models::{
    MatchId, MatchOutcome, MatchResult, MatchStatus, ParticipationStatus, Player, PlayerId,
    PlayerStatus, Role, RoundNumber, ScheduledMatch, ScoreEntryMode, SessionConfig, SessionState,
    Sport,
};
use crate::rng::SessionRng;
use crate::schedule_edit::{validate_round_schedule_update, RoundScheduleEditError};

fn parse_sport(s: &str) -> Sport {
    match s.trim() {
        "Soccer" => Sport::Soccer,
        "Pickleball" => Sport::Pickleball,
        "Table Tennis" => Sport::TableTennis,
        "Volleyball" => Sport::Volleyball,
        "Badminton" => Sport::Badminton,
        "Basketball" => Sport::Basketball,
        "Chess" => Sport::Chess,
        "Other" => Sport::Other,
        other => Sport::Custom(other.to_string()),
    }
}

#[derive(Clone)]
pub struct SessionManager {
    pub log: EventLog,
    pub state: SessionState,
    pub rng: SessionRng,
}

impl SessionManager {
    pub fn new(config: SessionConfig) -> Self {
        let mut log = EventLog::new();
        let rng = SessionRng::with_reseed_count(&config.id, config.reseed_count);
        log.append(Event::SessionCreated(config), Role::Coach);
        let state = crate::events::materialize(&log);
        Self { log, state, rng }
    }

    pub fn from_log(log: EventLog) -> Self {
        let state = crate::events::materialize(&log);
        let rng = match &state.config {
            Some(cfg) => SessionRng::with_reseed_count(&cfg.id, cfg.reseed_count),
            None => {
                // Fallback: use a zero seed (should not happen in practice)
                use uuid::Uuid;
                SessionRng::new(&crate::models::SessionId(Uuid::nil()))
            }
        };
        Self { log, state, rng }
    }

    /// Reconstruct a session from a results CSV produced by `export_results`.
    ///
    /// Creates a fresh session with the same players and match history, ready for
    /// "Update Rankings" and continued scheduling on a new device.
    pub fn from_results_csv(imported: &ImportedResults) -> Self {
        let team_size = imported.team_size.unwrap_or(2);
        let scheduling_frequency = imported.scheduling_frequency.unwrap_or(1);
        let sport = imported
            .sport
            .as_deref()
            .map(parse_sport)
            .unwrap_or(Sport::Soccer);
        let score_entry_mode = imported
            .score_entry_mode
            .unwrap_or(ScoreEntryMode::PointsPerPlayer);

        let mut config = SessionConfig::new(team_size, scheduling_frequency, sport);
        config.score_entry_mode = score_entry_mode;
        config.match_duration_minutes = imported.match_duration_minutes;

        let mut manager = SessionManager::new(config);

        // Register all players; build name→id lookup.
        let mut name_to_id: std::collections::HashMap<String, PlayerId> =
            std::collections::HashMap::new();
        for name in &imported.players {
            let id = manager.add_player(name.clone());
            name_to_id.insert(name.clone(), id);
        }

        // Group imported matches by round (preserving encounter order).
        let mut round_order: Vec<u32> = Vec::new();
        let mut round_groups: std::collections::HashMap<u32, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, m) in imported.matches.iter().enumerate() {
            if !round_groups.contains_key(&m.round) {
                round_order.push(m.round);
            }
            round_groups.entry(m.round).or_default().push(i);
        }

        let mut next_match_id = 1u32;

        for round_num in round_order {
            let indices = &round_groups[&round_num];
            let mut scheduled: Vec<ScheduledMatch> = Vec::new();
            let mut to_score: Vec<(MatchId, MatchResult)> = Vec::new();

            for (field, &mi) in indices.iter().enumerate() {
                let raw = &imported.matches[mi];
                let mid = MatchId(next_match_id);
                next_match_id += 1;

                let team_a: Vec<PlayerId> = raw
                    .team_a
                    .iter()
                    .filter_map(|participant| name_to_id.get(&participant.name).copied())
                    .collect();
                let team_b: Vec<PlayerId> = raw
                    .team_b
                    .iter()
                    .filter_map(|participant| name_to_id.get(&participant.name).copied())
                    .collect();

                scheduled.push(ScheduledMatch {
                    id: mid,
                    round: RoundNumber(round_num),
                    field: (field as u8) + 1,
                    team_a: team_a.clone(),
                    team_b: team_b.clone(),
                    status: MatchStatus::Scheduled,
                });

                let participation_by_player: std::collections::HashMap<
                    PlayerId,
                    ParticipationStatus,
                > = raw
                    .team_a
                    .iter()
                    .chain(raw.team_b.iter())
                    .filter_map(|participant| {
                        name_to_id
                            .get(&participant.name)
                            .map(|&id| (id, participant.participation_status))
                    })
                    .collect();
                let player_points: std::collections::HashMap<PlayerId, u16> = raw
                    .team_a
                    .iter()
                    .chain(raw.team_b.iter())
                    .filter_map(|participant| {
                        name_to_id
                            .get(&participant.name)
                            .and_then(|&id| participant.player_points.map(|points| (id, points)))
                    })
                    .collect();
                let duration_multiplier =
                    crate::models::clamp_duration_multiplier(raw.duration_multiplier);
                let match_result = match score_entry_mode {
                    ScoreEntryMode::PointsPerPlayer => MatchResult::new_points_per_player(
                        mid,
                        participation_by_player,
                        player_points,
                        duration_multiplier,
                        Role::Coach,
                    ),
                    ScoreEntryMode::PointsPerTeam => {
                        let team_a_points = raw.team_a_points.unwrap_or_else(|| {
                            raw.team_a
                                .iter()
                                .filter(|participant| participant.participation_status.played())
                                .filter_map(|participant| participant.player_points)
                                .sum()
                        });
                        let team_b_points = raw.team_b_points.unwrap_or_else(|| {
                            raw.team_b
                                .iter()
                                .filter(|participant| participant.participation_status.played())
                                .filter_map(|participant| participant.player_points)
                                .sum()
                        });
                        MatchResult::new_points_per_team(
                            mid,
                            participation_by_player,
                            team_a_points,
                            team_b_points,
                            duration_multiplier,
                            Role::Coach,
                        )
                    }
                    ScoreEntryMode::WinDrawLose => {
                        let outcome = raw.outcome.unwrap_or_else(|| {
                            let team_a_points = raw.team_a_points.unwrap_or(0);
                            let team_b_points = raw.team_b_points.unwrap_or(0);
                            if team_a_points > team_b_points {
                                MatchOutcome::TeamAWin
                            } else if team_b_points > team_a_points {
                                MatchOutcome::TeamBWin
                            } else {
                                MatchOutcome::Draw
                            }
                        });
                        MatchResult::new_win_draw_lose(
                            mid,
                            participation_by_player,
                            outcome,
                            duration_multiplier,
                            Role::Coach,
                        )
                    }
                };

                to_score.push((mid, match_result));
            }

            // Batch all events for this round without intermediate materializations.
            manager.log.append(
                Event::ScheduleGenerated {
                    round: RoundNumber(round_num),
                    matches: scheduled,
                },
                Role::Coach,
            );
            for (mid, result) in to_score {
                manager.log.append(
                    Event::ScoreEntered {
                        match_id: mid,
                        result,
                    },
                    Role::Coach,
                );
            }
        }

        // Single rematerialization once the full event log is assembled.
        manager.state = crate::events::materialize(&manager.log);
        manager
    }

    pub fn add_player(&mut self, name: String) -> PlayerId {
        let next_id = self
            .state
            .players
            .keys()
            .map(|pid| pid.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let id = PlayerId(next_id);
        let player = Player {
            id,
            name,
            status: PlayerStatus::Active,
            joined_at_round: self.state.current_round,
            deactivated_at_round: None,
        };
        self.log.append(Event::PlayerAdded(player), Role::Coach);
        self.state = crate::events::materialize(&self.log);
        id
    }

    pub fn deactivate_player(&mut self, player_id: PlayerId) {
        self.log.append(
            Event::PlayerDeactivated {
                player_id,
                at_round: self.state.current_round,
            },
            Role::Coach,
        );
        self.state = crate::events::materialize(&self.log);
    }

    pub fn reactivate_player(&mut self, player_id: PlayerId) {
        self.log.append(
            Event::PlayerReactivated {
                player_id,
                at_round: self.state.current_round,
            },
            Role::Coach,
        );
        self.state = crate::events::materialize(&self.log);
    }

    pub fn enter_score(&mut self, result: MatchResult, entered_by: Role) {
        let mut result = result;
        result.clamp_duration_multiplier_in_place();
        self.log.append(
            Event::ScoreEntered {
                match_id: result.match_id,
                result,
            },
            entered_by,
        );
        self.state = crate::events::materialize(&self.log);
    }

    pub fn correct_score(&mut self, result: MatchResult) {
        let mut result = result;
        result.clamp_duration_multiplier_in_place();
        self.log.append(
            Event::ScoreCorrected {
                match_id: result.match_id,
                result,
            },
            Role::Coach,
        );
        self.state = crate::events::materialize(&self.log);
    }

    pub fn apply_round_schedule_update(
        &mut self,
        round: RoundNumber,
        updated_matches: Vec<ScheduledMatch>,
    ) -> Result<(), RoundScheduleEditError> {
        validate_round_schedule_update(&self.state, round, &updated_matches)?;
        self.log.append(
            Event::RoundScheduleUpdated {
                round,
                matches: updated_matches,
            },
            Role::Coach,
        );
        self.state = crate::events::materialize(&self.log);
        Ok(())
    }

    pub fn void_match(&mut self, match_id: MatchId) {
        self.log
            .append(Event::MatchVoided { match_id }, Role::Coach);
        self.state = crate::events::materialize(&self.log);
    }

    pub fn lock_round(&mut self, round: RoundNumber) {
        self.log.append(Event::RoundLocked { round }, Role::Coach);
        self.state = crate::events::materialize(&self.log);
    }

    pub fn reseed(&mut self) -> u32 {
        let new_count = self.rng.reseed();
        self.log
            .append(Event::SessionReseeded { new_count }, Role::Coach);
        self.state = crate::events::materialize(&self.log);
        new_count
    }

    pub fn active_player_ids(&self) -> Vec<PlayerId> {
        self.state.active_players().map(|p| p.id).collect()
    }

    pub fn completed_match_results(&self) -> Vec<&MatchResult> {
        use crate::models::MatchStatus;
        self.state
            .matches
            .values()
            .filter(|m| m.status == MatchStatus::Completed)
            .filter_map(|m| self.state.results.get(&m.id))
            .collect()
    }
}
