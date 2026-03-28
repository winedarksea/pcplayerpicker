pub mod queries;

use crate::events::EventEnvelope;
use crate::models::{
    EventId, MatchId, MatchResult, PlayerRanking, RoundNumber, ScheduledMatch, SessionConfig,
    SessionId, SessionState,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("session not found: {0}")]
    NotFound(SessionId),
    #[error("database error: {0}")]
    Backend(String),
}

pub type DbResult<T> = Result<T, DbError>;

/// Storage abstraction for future normalized query layer.
///
/// No implementations exist yet — the app currently uses an append-only
/// event log (localStorage on the client, D1 on the worker). When an OPFS
/// SQLite runtime is wired up, `queries.rs` provides the schema and this
/// trait defines the interface each backend must satisfy.
pub trait DataStore {
    fn create_session(&self, config: &SessionConfig) -> DbResult<()>;
    fn get_session(&self, id: &SessionId) -> DbResult<Option<SessionState>>;
    fn append_event(&self, session_id: &SessionId, event: EventEnvelope) -> DbResult<EventId>;
    fn get_events(
        &self,
        session_id: &SessionId,
        since: Option<EventId>,
    ) -> DbResult<Vec<EventEnvelope>>;
    fn save_rankings(
        &self,
        session_id: &SessionId,
        round: RoundNumber,
        rankings: &[PlayerRanking],
    ) -> DbResult<()>;
    fn get_rankings(&self, session_id: &SessionId) -> DbResult<Vec<PlayerRanking>>;
    fn save_schedule(&self, session_id: &SessionId, matches: &[ScheduledMatch]) -> DbResult<()>;
    fn get_schedule(
        &self,
        session_id: &SessionId,
        round: Option<RoundNumber>,
    ) -> DbResult<Vec<ScheduledMatch>>;
    fn save_result(&self, session_id: &SessionId, result: &MatchResult) -> DbResult<()>;
    fn get_result(
        &self,
        session_id: &SessionId,
        match_id: MatchId,
    ) -> DbResult<Option<MatchResult>>;
    fn list_sessions(&self) -> DbResult<Vec<SessionConfig>>;
}
