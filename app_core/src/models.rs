use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// ── Identity ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerId(pub u32);

impl std::fmt::Display for PlayerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// Serialize as string so PlayerId can be used as a JSON object key
// (JSON map keys must be strings; serde_json rejects numeric keys).
impl Serialize for PlayerId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for PlayerId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = PlayerId;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("integer or string-encoded integer")
            }
            // Accept plain numbers (e.g. in arrays / non-key positions)
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<PlayerId, E> {
                Ok(PlayerId(v as u32))
            }
            // Accept strings (e.g. when deserialized as a map key)
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<PlayerId, E> {
                v.parse::<u32>().map(PlayerId).map_err(E::custom)
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatchId(pub u32);

impl std::fmt::Display for MatchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for MatchId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for MatchId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = MatchId;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("integer or string-encoded integer")
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<MatchId, E> {
                Ok(MatchId(v as u32))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<MatchId, E> {
                v.parse::<u32>().map(MatchId).map_err(E::custom)
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct RoundNumber(pub u32);

// ── Session Configuration ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub id: SessionId,
    /// Players per team: 1..=11, default 2
    pub team_size: u8,
    /// Re-schedule every N rounds (1 = every round)
    pub scheduling_frequency: u8,
    pub sport: Sport,
    /// Fixed match duration in minutes (None = untimed)
    pub match_duration_minutes: Option<u16>,
    pub created_at: DateTime<Utc>,
    /// Base RNG seed derived from session ID
    pub seed: u64,
    /// Incremented by coach explicit reseed action
    pub reseed_count: u32,
}

impl SessionConfig {
    pub fn new(team_size: u8, scheduling_frequency: u8, sport: Sport) -> Self {
        let id = SessionId::new();
        let seed = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            id.0.hash(&mut h);
            h.finish()
        };
        Self {
            id,
            team_size,
            scheduling_frequency,
            sport,
            match_duration_minutes: None,
            created_at: Utc::now(),
            seed,
            reseed_count: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Sport {
    Soccer,
    Basketball,
    Chess,
    Custom(String),
}

impl std::fmt::Display for Sport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Sport::Soccer => write!(f, "Soccer"),
            Sport::Basketball => write!(f, "Basketball"),
            Sport::Chess => write!(f, "Chess"),
            Sport::Custom(s) => write!(f, "{}", s),
        }
    }
}

// ── Players ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Player {
    pub id: PlayerId,
    /// Unique within a session; defaults to numeric ID string
    pub name: String,
    pub status: PlayerStatus,
    pub joined_at_round: RoundNumber,
    pub deactivated_at_round: Option<RoundNumber>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerStatus {
    Active,
    Inactive,
}

// ── Matches ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledMatch {
    pub id: MatchId,
    pub round: RoundNumber,
    pub field: u8,
    pub team_a: Vec<PlayerId>,
    pub team_b: Vec<PlayerId>,
    pub status: MatchStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchStatus {
    Scheduled,
    InProgress,
    Completed,
    Voided,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchResult {
    pub match_id: MatchId,
    pub scores: HashMap<PlayerId, PlayerMatchScore>,
    /// 1.0 = full match; 0.5 = half duration, etc. Applied to all outcome weights.
    pub duration_multiplier: f64,
    pub entered_by: Role,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerMatchScore {
    /// None = did not play (default for all match slots).
    /// Some(0) = played and scored zero. Never conflate with did-not-play.
    pub goals: Option<u16>,
    // Future extension: advanced_stats: Option<AdvancedStats>
    // AdvancedStats { conquered_balls, received_balls, lost_balls, attacking_passes }
}

impl PlayerMatchScore {
    pub fn did_not_play() -> Self {
        Self { goals: None }
    }
    pub fn scored(goals: u16) -> Self {
        Self { goals: Some(goals) }
    }
    pub fn played(&self) -> bool {
        self.goals.is_some()
    }
}

// ── Rankings ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerRanking {
    pub player_id: PlayerId,
    /// Posterior mean skill (log-scale)
    pub rating: f64,
    /// Posterior standard deviation
    pub uncertainty: f64,
    /// 1-indexed median rank
    pub rank: u32,
    /// 90% credible rank interval (5th to 95th percentile)
    pub rank_range_90: (u32, u32),
    pub matches_played: u32,
    pub total_goals: u32,
    /// P(rank ≤ K) for some configured K
    pub prob_top_k: f64,
    /// Whether this player is still active; inactive = retained with "as of last match" note
    pub is_active: bool,
}

// ── Roles ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Coach,
    Assistant,
    Player,
}

// ── Session State (materialized from events) ─────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct SessionState {
    pub config: Option<SessionConfig>,
    pub players: HashMap<PlayerId, Player>,
    pub matches: HashMap<MatchId, ScheduledMatch>,
    pub results: HashMap<MatchId, MatchResult>,
    pub rankings: Vec<PlayerRanking>,
    pub current_round: RoundNumber,
}

impl SessionState {
    pub fn active_players(&self) -> impl Iterator<Item = &Player> {
        self.players
            .values()
            .filter(|p| p.status == PlayerStatus::Active)
    }

    pub fn player_count(&self) -> usize {
        self.players.len()
    }
}

impl Default for RoundNumber {
    fn default() -> Self {
        RoundNumber(1)
    }
}
