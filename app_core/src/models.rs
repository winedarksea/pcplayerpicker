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
    #[serde(default)]
    pub ranking_method: RankingMethod,
    #[serde(default)]
    pub scheduling_method: SchedulingMethod,
    #[serde(default)]
    pub score_entry_mode: ScoreEntryMode,
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
        let score_entry_mode = sport.profile().default_score_entry_mode;
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
            ranking_method: RankingMethod::default(),
            scheduling_method: SchedulingMethod::default(),
            score_entry_mode,
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
    Pickleball,
    TableTennis,
    Volleyball,
    Badminton,
    Basketball,
    Chess,
    Other,
    Custom(String),
}

impl std::fmt::Display for Sport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Sport::Soccer => write!(f, "Soccer"),
            Sport::Pickleball => write!(f, "Pickleball"),
            Sport::TableTennis => write!(f, "Table Tennis"),
            Sport::Volleyball => write!(f, "Volleyball"),
            Sport::Badminton => write!(f, "Badminton"),
            Sport::Basketball => write!(f, "Basketball"),
            Sport::Chess => write!(f, "Chess"),
            Sport::Other => write!(f, "Other"),
            Sport::Custom(s) => write!(f, "{}", s),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ScoreEntryMode {
    PointsPerTeam,
    #[default]
    PointsPerPlayer,
    WinDrawLose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RankingMethod {
    #[default]
    GoalModelV1,
}

impl RankingMethod {
    pub fn label(self) -> &'static str {
        match self {
            RankingMethod::GoalModelV1 => "Goal Model v1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SchedulingMethod {
    #[default]
    AdaptiveV1,
    RoundRobinV1,
    InfoMaxV1,
}

impl SchedulingMethod {
    pub fn label(self) -> &'static str {
        match self {
            SchedulingMethod::AdaptiveV1 => "Adaptive v1",
            SchedulingMethod::RoundRobinV1 => "Round Robin v1",
            SchedulingMethod::InfoMaxV1 => "Info Max v1",
        }
    }
}

impl std::fmt::Display for ScoreEntryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScoreEntryMode::PointsPerTeam => write!(f, "Points Per Team"),
            ScoreEntryMode::PointsPerPlayer => write!(f, "Points Per Player"),
            ScoreEntryMode::WinDrawLose => write!(f, "Win / Draw / Lose"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SportProfile {
    pub label: String,
    pub default_team_size: u8,
    pub default_score_entry_mode: ScoreEntryMode,
    pub supports_attack_defense_teamwork: bool,
    pub supports_synergy_analysis: bool,
}

impl Sport {
    pub const BUILT_IN_SPORTS: [Sport; 8] = [
        Sport::Soccer,
        Sport::Pickleball,
        Sport::TableTennis,
        Sport::Volleyball,
        Sport::Chess,
        Sport::Badminton,
        Sport::Basketball,
        Sport::Other,
    ];

    pub fn built_in_sports() -> &'static [Sport] {
        &Self::BUILT_IN_SPORTS
    }

    pub fn profile(&self) -> SportProfile {
        match self {
            Sport::Soccer => SportProfile {
                label: "Soccer".to_string(),
                default_team_size: 2,
                default_score_entry_mode: ScoreEntryMode::PointsPerPlayer,
                supports_attack_defense_teamwork: true,
                supports_synergy_analysis: true,
            },
            Sport::Pickleball => SportProfile {
                label: "Pickleball".to_string(),
                default_team_size: 2,
                default_score_entry_mode: ScoreEntryMode::PointsPerTeam,
                supports_attack_defense_teamwork: false,
                supports_synergy_analysis: true,
            },
            Sport::TableTennis => SportProfile {
                label: "Table Tennis".to_string(),
                default_team_size: 1,
                default_score_entry_mode: ScoreEntryMode::PointsPerTeam,
                supports_attack_defense_teamwork: false,
                supports_synergy_analysis: false,
            },
            Sport::Volleyball => SportProfile {
                label: "Volleyball".to_string(),
                default_team_size: 2,
                default_score_entry_mode: ScoreEntryMode::PointsPerTeam,
                supports_attack_defense_teamwork: false,
                supports_synergy_analysis: true,
            },
            Sport::Chess => SportProfile {
                label: "Chess".to_string(),
                default_team_size: 1,
                default_score_entry_mode: ScoreEntryMode::WinDrawLose,
                supports_attack_defense_teamwork: false,
                supports_synergy_analysis: false,
            },
            Sport::Badminton => SportProfile {
                label: "Badminton".to_string(),
                default_team_size: 1,
                default_score_entry_mode: ScoreEntryMode::PointsPerTeam,
                supports_attack_defense_teamwork: false,
                supports_synergy_analysis: false,
            },
            Sport::Basketball => SportProfile {
                label: "Basketball".to_string(),
                default_team_size: 3,
                default_score_entry_mode: ScoreEntryMode::PointsPerPlayer,
                supports_attack_defense_teamwork: false,
                supports_synergy_analysis: true,
            },
            Sport::Other => SportProfile {
                label: "Other".to_string(),
                default_team_size: 2,
                default_score_entry_mode: ScoreEntryMode::PointsPerTeam,
                supports_attack_defense_teamwork: false,
                supports_synergy_analysis: true,
            },
            Sport::Custom(label) => SportProfile {
                label: label.clone(),
                default_team_size: 2,
                default_score_entry_mode: ScoreEntryMode::PointsPerTeam,
                supports_attack_defense_teamwork: false,
                supports_synergy_analysis: true,
            },
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
    #[serde(default)]
    pub scheduling_method: SchedulingMethod,
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
#[serde(from = "MatchResultSerde", into = "MatchResultSerde")]
pub struct MatchResult {
    pub match_id: MatchId,
    pub participation_by_player: HashMap<PlayerId, ParticipationStatus>,
    pub score_payload: MatchScorePayload,
    /// 1.0 = full match; 0.5 = half duration, etc. Applied to all outcome weights.
    pub duration_multiplier: f64,
    pub entered_by: Role,
    /// Wall-clock time when this result was first submitted. None for results
    /// recorded before this field was introduced (legacy records).
    pub submitted_at: Option<DateTime<Utc>>,
}

pub const MIN_DURATION_MULTIPLIER: f64 = 0.5;
pub const MAX_DURATION_MULTIPLIER: f64 = 1.5;

pub fn clamp_duration_multiplier(duration_multiplier: f64) -> f64 {
    if !duration_multiplier.is_finite() {
        return 1.0;
    }

    duration_multiplier.clamp(MIN_DURATION_MULTIPLIER, MAX_DURATION_MULTIPLIER)
}

impl MatchResult {
    pub fn matches_saved_score_content(&self, other: &Self) -> bool {
        self.match_id == other.match_id
            && self.participation_by_player == other.participation_by_player
            && self.score_payload == other.score_payload
            && self.entered_by == other.entered_by
            && (self.normalized_duration_multiplier() - other.normalized_duration_multiplier())
                .abs()
                < 0.01
    }

    pub fn new_points_per_player(
        match_id: MatchId,
        participation_by_player: HashMap<PlayerId, ParticipationStatus>,
        player_points: HashMap<PlayerId, u16>,
        duration_multiplier: f64,
        entered_by: Role,
    ) -> Self {
        Self {
            match_id,
            participation_by_player,
            score_payload: MatchScorePayload::PointsPerPlayer { player_points },
            duration_multiplier,
            entered_by,
            submitted_at: Some(Utc::now()),
        }
    }

    pub fn from_legacy_player_scores(
        match_id: MatchId,
        scores: HashMap<PlayerId, PlayerMatchScore>,
        duration_multiplier: f64,
        entered_by: Role,
    ) -> Self {
        let participation_by_player = scores
            .iter()
            .map(|(player_id, score)| {
                (
                    *player_id,
                    if score.played() {
                        ParticipationStatus::Played
                    } else {
                        ParticipationStatus::DidNotPlay
                    },
                )
            })
            .collect();
        let player_points = scores
            .into_iter()
            .filter_map(|(player_id, score)| score.goals.map(|points| (player_id, points)))
            .collect();
        Self::new_points_per_player(
            match_id,
            participation_by_player,
            player_points,
            duration_multiplier,
            entered_by,
        )
    }

    pub fn new_points_per_team(
        match_id: MatchId,
        participation_by_player: HashMap<PlayerId, ParticipationStatus>,
        team_a_points: u16,
        team_b_points: u16,
        duration_multiplier: f64,
        entered_by: Role,
    ) -> Self {
        Self {
            match_id,
            participation_by_player,
            score_payload: MatchScorePayload::PointsPerTeam {
                team_a_points,
                team_b_points,
            },
            duration_multiplier,
            entered_by,
            submitted_at: Some(Utc::now()),
        }
    }

    pub fn new_win_draw_lose(
        match_id: MatchId,
        participation_by_player: HashMap<PlayerId, ParticipationStatus>,
        outcome: MatchOutcome,
        duration_multiplier: f64,
        entered_by: Role,
    ) -> Self {
        Self {
            match_id,
            participation_by_player,
            score_payload: MatchScorePayload::WinDrawLose { outcome },
            duration_multiplier,
            entered_by,
            submitted_at: Some(Utc::now()),
        }
    }

    pub fn normalized_duration_multiplier(&self) -> f64 {
        clamp_duration_multiplier(self.duration_multiplier)
    }

    pub fn clamp_duration_multiplier_in_place(&mut self) {
        self.duration_multiplier = self.normalized_duration_multiplier();
    }

    pub fn score_entry_mode(&self) -> ScoreEntryMode {
        self.score_payload.score_entry_mode()
    }

    pub fn participation_status(&self, player_id: &PlayerId) -> ParticipationStatus {
        self.participation_by_player
            .get(player_id)
            .copied()
            .unwrap_or_default()
    }

    pub fn player_played(&self, player_id: &PlayerId) -> bool {
        self.participation_status(player_id).played()
    }

    pub fn individual_points_for_player(&self, player_id: &PlayerId) -> Option<u16> {
        if !self.player_played(player_id) {
            return None;
        }
        match &self.score_payload {
            MatchScorePayload::PointsPerPlayer { player_points } => {
                Some(*player_points.get(player_id).unwrap_or(&0))
            }
            _ => None,
        }
    }

    pub fn numeric_team_points(&self, scheduled_match: &ScheduledMatch) -> Option<(u16, u16)> {
        match &self.score_payload {
            MatchScorePayload::PointsPerPlayer { player_points } => {
                let team_points = |team: &[PlayerId]| -> u16 {
                    team.iter()
                        .filter(|player_id| self.player_played(player_id))
                        .map(|player_id| player_points.get(player_id).copied().unwrap_or(0))
                        .sum()
                };
                Some((
                    team_points(&scheduled_match.team_a),
                    team_points(&scheduled_match.team_b),
                ))
            }
            MatchScorePayload::PointsPerTeam {
                team_a_points,
                team_b_points,
            } => Some((*team_a_points, *team_b_points)),
            MatchScorePayload::WinDrawLose { .. } => None,
        }
    }

    pub fn team_points_for_player(
        &self,
        player_id: &PlayerId,
        scheduled_match: &ScheduledMatch,
    ) -> Option<u16> {
        if !self.player_played(player_id) {
            return None;
        }
        let (team_a_points, team_b_points) = self.numeric_team_points(scheduled_match)?;
        if scheduled_match.team_a.contains(player_id) {
            Some(team_a_points)
        } else if scheduled_match.team_b.contains(player_id) {
            Some(team_b_points)
        } else {
            None
        }
    }

    pub fn opponent_team_points_for_player(
        &self,
        player_id: &PlayerId,
        scheduled_match: &ScheduledMatch,
    ) -> Option<u16> {
        if !self.player_played(player_id) {
            return None;
        }
        let (team_a_points, team_b_points) = self.numeric_team_points(scheduled_match)?;
        if scheduled_match.team_a.contains(player_id) {
            Some(team_b_points)
        } else if scheduled_match.team_b.contains(player_id) {
            Some(team_a_points)
        } else {
            None
        }
    }

    pub fn team_outcome_value_for_player(
        &self,
        player_id: &PlayerId,
        scheduled_match: &ScheduledMatch,
    ) -> Option<f64> {
        if !self.player_played(player_id) {
            return None;
        }

        match &self.score_payload {
            MatchScorePayload::WinDrawLose { outcome } => {
                if scheduled_match.team_a.contains(player_id) {
                    Some(match outcome {
                        MatchOutcome::TeamAWin => 1.0,
                        MatchOutcome::Draw => 0.5,
                        MatchOutcome::TeamBWin => 0.0,
                    })
                } else if scheduled_match.team_b.contains(player_id) {
                    Some(match outcome {
                        MatchOutcome::TeamAWin => 0.0,
                        MatchOutcome::Draw => 0.5,
                        MatchOutcome::TeamBWin => 1.0,
                    })
                } else {
                    None
                }
            }
            _ => {
                let own_points = self.team_points_for_player(player_id, scheduled_match)? as f64;
                let opp_points =
                    self.opponent_team_points_for_player(player_id, scheduled_match)? as f64;
                Some(if own_points > opp_points {
                    1.0
                } else if own_points < opp_points {
                    0.0
                } else {
                    0.5
                })
            }
        }
    }

    pub fn replace_player_id(&mut self, old_player: PlayerId, new_player: PlayerId) {
        if let Some(status) = self.participation_by_player.remove(&old_player) {
            self.participation_by_player.insert(new_player, status);
        }
        if let MatchScorePayload::PointsPerPlayer { player_points } = &mut self.score_payload {
            if let Some(points) = player_points.remove(&old_player) {
                player_points.insert(new_player, points);
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerMatchScore {
    /// Legacy compatibility for older event logs and CSV imports.
    /// None = did not play; Some(0) = played and scored zero.
    pub goals: Option<u16>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ParticipationStatus {
    #[default]
    DidNotPlay,
    Played,
}

impl ParticipationStatus {
    pub fn played(self) -> bool {
        matches!(self, ParticipationStatus::Played)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MatchScorePayload {
    PointsPerPlayer {
        player_points: HashMap<PlayerId, u16>,
    },
    PointsPerTeam {
        team_a_points: u16,
        team_b_points: u16,
    },
    WinDrawLose {
        outcome: MatchOutcome,
    },
}

impl MatchScorePayload {
    pub fn score_entry_mode(&self) -> ScoreEntryMode {
        match self {
            MatchScorePayload::PointsPerTeam { .. } => ScoreEntryMode::PointsPerTeam,
            MatchScorePayload::PointsPerPlayer { .. } => ScoreEntryMode::PointsPerPlayer,
            MatchScorePayload::WinDrawLose { .. } => ScoreEntryMode::WinDrawLose,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchOutcome {
    TeamAWin,
    Draw,
    TeamBWin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MatchResultSerde {
    match_id: MatchId,
    #[serde(default)]
    participation_by_player: HashMap<PlayerId, ParticipationStatus>,
    #[serde(default)]
    score_payload: Option<MatchScorePayload>,
    #[serde(default = "default_duration_multiplier")]
    duration_multiplier: f64,
    entered_by: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scores: Option<HashMap<PlayerId, PlayerMatchScore>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    submitted_at: Option<DateTime<Utc>>,
}

fn default_duration_multiplier() -> f64 {
    1.0
}

impl From<MatchResultSerde> for MatchResult {
    fn from(value: MatchResultSerde) -> Self {
        let mut participation_by_player = value.participation_by_player;
        let score_payload = if let Some(score_payload) = value.score_payload {
            if participation_by_player.is_empty() {
                if let MatchScorePayload::PointsPerPlayer { player_points } = &score_payload {
                    participation_by_player.extend(
                        player_points
                            .keys()
                            .copied()
                            .map(|player_id| (player_id, ParticipationStatus::Played)),
                    );
                }
            }
            score_payload
        } else {
            let legacy_scores = value.scores.unwrap_or_default();
            let mut player_points = HashMap::new();
            for (player_id, score) in legacy_scores {
                let status = if score.played() {
                    ParticipationStatus::Played
                } else {
                    ParticipationStatus::DidNotPlay
                };
                participation_by_player.insert(player_id, status);
                if let Some(points) = score.goals {
                    player_points.insert(player_id, points);
                }
            }
            MatchScorePayload::PointsPerPlayer { player_points }
        };

        Self {
            match_id: value.match_id,
            participation_by_player,
            score_payload,
            duration_multiplier: value.duration_multiplier,
            entered_by: value.entered_by,
            submitted_at: value.submitted_at,
        }
    }
}

impl From<MatchResult> for MatchResultSerde {
    fn from(value: MatchResult) -> Self {
        Self {
            match_id: value.match_id,
            participation_by_player: value.participation_by_player,
            score_payload: Some(value.score_payload),
            duration_multiplier: value.duration_multiplier,
            entered_by: value.entered_by,
            scores: None,
            submitted_at: value.submitted_at,
        }
    }
}

// ── Rankings ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerRanking {
    pub player_id: PlayerId,
    #[serde(default)]
    pub ranking_method: RankingMethod,
    /// Posterior mean skill (log-scale)
    pub rating: f64,
    /// Conservative display score: posterior mean minus a fixed uncertainty penalty.
    #[serde(default)]
    pub conservative_rating: f64,
    /// Posterior standard deviation
    pub uncertainty: f64,
    /// 1-indexed median rank
    pub rank: u32,
    /// 90% credible rank interval (5th to 95th percentile)
    pub rank_range_90: (u32, u32),
    pub matches_played: u32,
    #[serde(default, alias = "total_goals")]
    pub total_score: u32,
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
    pub latest_ranking_method: Option<RankingMethod>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_multiplier_is_clamped_to_supported_range() {
        assert_eq!(clamp_duration_multiplier(0.1), MIN_DURATION_MULTIPLIER);
        assert_eq!(clamp_duration_multiplier(0.5), 0.5);
        assert_eq!(clamp_duration_multiplier(1.0), 1.0);
        assert_eq!(clamp_duration_multiplier(1.5), 1.5);
        assert_eq!(clamp_duration_multiplier(2.0), MAX_DURATION_MULTIPLIER);
    }

    #[test]
    fn non_finite_duration_multiplier_defaults_to_full_match() {
        assert_eq!(clamp_duration_multiplier(f64::NAN), 1.0);
        assert_eq!(clamp_duration_multiplier(f64::INFINITY), 1.0);
        assert_eq!(clamp_duration_multiplier(f64::NEG_INFINITY), 1.0);
    }
}
