/// SessionManager orchestrates the core loop:
/// receive results → re-rank → generate next schedule → emit events.
///
/// This is a pure logic layer — no I/O. The caller (client_app state module)
/// persists events and rankings via the DataStore trait.
use crate::events::{Event, EventLog};
use crate::models::{
    MatchId, MatchResult, Player, PlayerId, PlayerStatus, Role, SessionConfig, SessionState,
};
use crate::rng::SessionRng;

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
        self.log.append(
            Event::ScoreCorrected {
                match_id: result.match_id,
                result,
            },
            Role::Coach,
        );
        self.state = crate::events::materialize(&self.log);
    }

    pub fn swap_player(&mut self, match_id: MatchId, old_player: PlayerId, new_player: PlayerId) {
        self.log.append(
            Event::PlayerSwapped {
                match_id,
                old_player,
                new_player,
            },
            Role::Coach,
        );
        self.state = crate::events::materialize(&self.log);
    }

    pub fn void_match(&mut self, match_id: MatchId) {
        self.log
            .append(Event::MatchVoided { match_id }, Role::Coach);
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
