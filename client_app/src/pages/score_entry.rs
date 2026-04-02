use app_core::models::{
    MatchId, MatchOutcome, MatchResult, ParticipationStatus, PlayerId, Role, ScoreEntryMode,
};
use leptos::prelude::*;
use std::collections::HashMap;
use std::rc::Rc;

const DURATION_OPTIONS: &[(f64, &str)] = &[
    (0.5, "½ match"),
    (0.75, "¾ match"),
    (1.0, "Full"),
    (1.25, "1¼×"),
    (1.5, "1½×"),
];

#[derive(Clone, Copy)]
enum PlayerAvailabilityDraft {
    Played,
    DidNotPlay,
}

impl PlayerAvailabilityDraft {
    fn from_result(result: Option<&MatchResult>, player_id: &PlayerId) -> Self {
        match result.map(|result| result.participation_status(player_id)) {
            Some(ParticipationStatus::Played) => Self::Played,
            _ => Self::DidNotPlay,
        }
    }

    fn as_participation_status(self) -> ParticipationStatus {
        match self {
            PlayerAvailabilityDraft::Played => ParticipationStatus::Played,
            PlayerAvailabilityDraft::DidNotPlay => ParticipationStatus::DidNotPlay,
        }
    }

    fn played(self) -> bool {
        matches!(self, PlayerAvailabilityDraft::Played)
    }
}

fn player_points_value(result: Option<&MatchResult>, player_id: &PlayerId) -> u16 {
    result
        .and_then(|result| result.individual_points_for_player(player_id))
        .unwrap_or(0)
}

fn parse_non_negative_u16(raw: &str) -> u16 {
    raw.trim().parse::<u16>().unwrap_or(0)
}

#[component]
pub fn SharedScoreEntryCard<F>(
    match_id: MatchId,
    field: u8,
    round: u32,
    team_a: Vec<PlayerId>,
    team_b: Vec<PlayerId>,
    player_names: HashMap<PlayerId, String>,
    score_entry_mode: ScoreEntryMode,
    existing_result: Option<MatchResult>,
    on_submit: F,
    #[prop(default = true)] show_duration_picker: bool,
    #[prop(default = Role::Coach)] entered_by: Role,
    #[prop(default = "Save Scores")] submit_label: &'static str,
    #[prop(default = "Saved ✓")] saved_label: &'static str,
    #[prop(default = false)] auto_save: bool,
    #[prop(default = Signal::derive(|| false))] is_submitting: Signal<bool>,
    #[prop(optional)] footer_error: Option<Signal<String>>,
) -> impl IntoView
where
    F: Fn(MatchResult) + 'static + Clone,
{
    let all_ids: Vec<PlayerId> = team_a.iter().chain(team_b.iter()).copied().collect();
    let initial_result = existing_result.as_ref();
    let availability_draft = RwSignal::new(
        all_ids
            .iter()
            .map(|player_id| {
                (
                    *player_id,
                    PlayerAvailabilityDraft::from_result(initial_result, player_id),
                )
            })
            .collect::<HashMap<_, _>>(),
    );
    let player_points_draft = RwSignal::new(
        all_ids
            .iter()
            .map(|player_id| (*player_id, player_points_value(initial_result, player_id)))
            .collect::<HashMap<_, _>>(),
    );
    let team_a_points_draft = RwSignal::new(
        initial_result
            .as_ref()
            .and_then(|result| {
                let scheduled_match = app_core::models::ScheduledMatch {
                    id: result.match_id,
                    round: app_core::models::RoundNumber(round),
                    field,
                    team_a: team_a.clone(),
                    team_b: team_b.clone(),
                    status: app_core::models::MatchStatus::Scheduled,
                };
                result
                    .numeric_team_points(&scheduled_match)
                    .map(|(team_a_points, _)| team_a_points)
            })
            .unwrap_or(0)
            .to_string(),
    );
    let team_b_points_draft = RwSignal::new(
        initial_result
            .as_ref()
            .and_then(|result| {
                let scheduled_match = app_core::models::ScheduledMatch {
                    id: result.match_id,
                    round: app_core::models::RoundNumber(round),
                    field,
                    team_a: team_a.clone(),
                    team_b: team_b.clone(),
                    status: app_core::models::MatchStatus::Scheduled,
                };
                result
                    .numeric_team_points(&scheduled_match)
                    .map(|(_, team_b_points)| team_b_points)
            })
            .unwrap_or(0)
            .to_string(),
    );
    let outcome_draft = RwSignal::new(
        match existing_result.as_ref().map(|result| &result.score_payload) {
            Some(app_core::models::MatchScorePayload::WinDrawLose { outcome }) => *outcome,
            _ => MatchOutcome::Draw,
        },
    );
    let duration_mult = RwSignal::new(
        existing_result
            .as_ref()
            .map(|result| result.duration_multiplier)
            .unwrap_or(1.0),
    );
    let is_saved = RwSignal::new(existing_result.is_some());
    let last_auto_saved_result = RwSignal::new(existing_result.clone());

    let build_result = {
        let all_ids = all_ids.clone();
        move || {
            let participation_by_player: HashMap<PlayerId, ParticipationStatus> =
                availability_draft.with(|draft| {
                    all_ids
                        .iter()
                        .map(|player_id| {
                            (
                                *player_id,
                                draft
                                    .get(player_id)
                                    .copied()
                                    .unwrap_or(PlayerAvailabilityDraft::DidNotPlay)
                                    .as_participation_status(),
                            )
                        })
                        .collect()
                });
            let duration_multiplier = duration_mult.get_untracked();
            match score_entry_mode {
                ScoreEntryMode::PointsPerPlayer => {
                    let player_points = player_points_draft.with(|draft| {
                        all_ids
                            .iter()
                            .filter_map(|player_id| {
                                let played = availability_draft.with(|availability| {
                                    availability
                                        .get(player_id)
                                        .copied()
                                        .unwrap_or(PlayerAvailabilityDraft::DidNotPlay)
                                        .played()
                                });
                                played.then(|| (*player_id, *draft.get(player_id).unwrap_or(&0)))
                            })
                            .collect()
                    });
                    MatchResult::new_points_per_player(
                        match_id,
                        participation_by_player,
                        player_points,
                        duration_multiplier,
                        entered_by.clone(),
                    )
                }
                ScoreEntryMode::PointsPerTeam => MatchResult::new_points_per_team(
                    match_id,
                    participation_by_player,
                    parse_non_negative_u16(&team_a_points_draft.get_untracked()),
                    parse_non_negative_u16(&team_b_points_draft.get_untracked()),
                    duration_multiplier,
                    entered_by.clone(),
                ),
                ScoreEntryMode::WinDrawLose => MatchResult::new_win_draw_lose(
                    match_id,
                    participation_by_player,
                    outcome_draft.get_untracked(),
                    duration_multiplier,
                    entered_by.clone(),
                ),
            }
        }
    };
    let submit_current_result: Rc<dyn Fn()> = {
        let on_submit = on_submit.clone();
        Rc::new(move || {
            let next_result = build_result();
            if auto_save
                && last_auto_saved_result
                    .get_untracked()
                    .as_ref()
                    .is_some_and(|saved_result| {
                        saved_result.matches_saved_score_content(&next_result)
                    })
            {
                is_saved.set(true);
                return;
            }
            on_submit(next_result.clone());
            if auto_save {
                last_auto_saved_result.set(Some(next_result));
            }
            is_saved.set(true);
        })
    };
    let maybe_auto_save_current_result: Rc<dyn Fn()> = if auto_save {
        Rc::clone(&submit_current_result)
    } else {
        Rc::new(|| {})
    };

    view! {
        <div class="bg-gray-900 border border-gray-700/50 rounded-xl overflow-hidden">
            <div class="px-4 pt-4 pb-3 border-b border-gray-700/30 flex items-center justify-between">
                <span class="text-sm font-semibold text-white">
                    "Field "{field}" · Rd "{round}
                </span>
                {move || is_saved.get().then(|| view! {
                    <span class="text-xs text-green-400 font-medium">{saved_label}</span>
                })}
            </div>

            <div class="px-4 py-3 space-y-3">
                {all_ids.iter().map(|&player_id| {
                    let name = player_names
                        .get(&player_id)
                        .cloned()
                        .unwrap_or_else(|| format!("Player {}", player_id.0));
                    let on_team_a = team_a.contains(&player_id);
                    let auto_save_after_played_click = Rc::clone(&maybe_auto_save_current_result);
                    let auto_save_after_dnp_click = Rc::clone(&maybe_auto_save_current_result);
                    let auto_save_after_points_input = Rc::clone(&maybe_auto_save_current_result);
                    view! {
                        <div class="flex items-center gap-2 flex-wrap">
                            <span class=move || format!(
                                "w-2 h-2 rounded-full shrink-0 {}",
                                if on_team_a { "bg-blue-400" } else { "bg-orange-400" }
                            ) />
                            <span class="flex-1 min-w-[80px] text-sm text-white truncate">{name}</span>
                            <div class="flex gap-1 items-center flex-wrap">
                                <button
                                    class=move || {
                                        let active = availability_draft.with(|draft| {
                                            matches!(
                                                draft.get(&player_id).copied().unwrap_or(PlayerAvailabilityDraft::DidNotPlay),
                                                PlayerAvailabilityDraft::Played
                                            )
                                        });
                                        format!(
                                            "px-2 py-1 rounded min-h-[36px] text-xs {}",
                                            if active { "bg-blue-600 text-white font-semibold" }
                                            else { "bg-gray-800 text-gray-400 hover:bg-gray-700" }
                                        )
                                    }
                                    on:click=move |_| {
                                        availability_draft.update(|draft| {
                                            draft.insert(player_id, PlayerAvailabilityDraft::Played);
                                        });
                                        is_saved.set(false);
                                        auto_save_after_played_click();
                                    }
                                >
                                    "Played"
                                </button>
                                <button
                                    class=move || {
                                        let active = availability_draft.with(|draft| {
                                            matches!(
                                                draft.get(&player_id).copied().unwrap_or(PlayerAvailabilityDraft::DidNotPlay),
                                                PlayerAvailabilityDraft::DidNotPlay
                                            )
                                        });
                                        format!(
                                            "px-2 py-1 rounded min-h-[36px] text-xs {}",
                                            if active { "bg-gray-600 text-white font-semibold" }
                                            else { "bg-gray-800 text-gray-400 hover:bg-gray-700" }
                                        )
                                    }
                                    on:click=move |_| {
                                        availability_draft.update(|draft| {
                                            draft.insert(player_id, PlayerAvailabilityDraft::DidNotPlay);
                                        });
                                        is_saved.set(false);
                                        auto_save_after_dnp_click();
                                    }
                                >
                                    "DNP"
                                </button>
                                {(score_entry_mode == ScoreEntryMode::PointsPerPlayer).then(|| view! {
                                    <input
                                        type="number"
                                        min="0"
                                        class="w-20 bg-gray-950 border border-gray-700 rounded-lg px-2 py-1.5 text-sm text-white"
                                        prop:value=move || player_points_draft.with(|draft| draft.get(&player_id).copied().unwrap_or(0).to_string())
                                        on:input=move |ev| {
                                            let new_points = parse_non_negative_u16(&event_target_value(&ev));
                                            availability_draft.update(|draft| {
                                                draft.insert(player_id, PlayerAvailabilityDraft::Played);
                                            });
                                            player_points_draft.update(|draft| {
                                                draft.insert(player_id, new_points);
                                            });
                                            is_saved.set(false);
                                            auto_save_after_points_input();
                                        }
                                    />
                                })}
                            </div>
                        </div>
                    }
                }).collect_view()}
            </div>

            {(score_entry_mode == ScoreEntryMode::PointsPerTeam).then({
                let auto_save_after_team_a_input = Rc::clone(&maybe_auto_save_current_result);
                let auto_save_after_team_b_input = Rc::clone(&maybe_auto_save_current_result);
                move || view! {
                    <div class="px-4 pb-3 border-t border-gray-700/20 pt-3 space-y-3">
                        <p class="text-xs text-gray-500">"Team points"</p>
                        <div class="grid grid-cols-2 gap-3">
                            <input
                                type="number"
                                min="0"
                                class="bg-gray-950 border border-gray-700 rounded-lg px-3 py-2 text-sm text-white"
                                prop:value=move || team_a_points_draft.get()
                                on:input=move |ev| {
                                    team_a_points_draft.set(event_target_value(&ev));
                                    is_saved.set(false);
                                    auto_save_after_team_a_input();
                                }
                            />
                            <input
                                type="number"
                                min="0"
                                class="bg-gray-950 border border-gray-700 rounded-lg px-3 py-2 text-sm text-white"
                                prop:value=move || team_b_points_draft.get()
                                on:input=move |ev| {
                                    team_b_points_draft.set(event_target_value(&ev));
                                    is_saved.set(false);
                                    auto_save_after_team_b_input();
                                }
                            />
                        </div>
                    </div>
                }
            })}

            {(score_entry_mode == ScoreEntryMode::WinDrawLose).then({
                let maybe_auto_save_current_result = Rc::clone(&maybe_auto_save_current_result);
                move || view! {
                    <div class="px-4 pb-3 border-t border-gray-700/20 pt-3">
                        <p class="text-xs text-gray-500 mb-2">"Match outcome"</p>
                        <div class="flex gap-2 flex-wrap">
                            {[
                                (MatchOutcome::TeamAWin, "Team A"),
                                (MatchOutcome::Draw, "Draw"),
                                (MatchOutcome::TeamBWin, "Team B"),
                            ].into_iter().map(|(outcome, label)| {
                                let auto_save_after_outcome_click = Rc::clone(&maybe_auto_save_current_result);
                                view! {
                                    <button
                                        class=move || {
                                            let active = outcome_draft.get() == outcome;
                                            format!(
                                                "px-3 py-2 rounded text-sm font-medium min-h-[36px] {}",
                                                if active { "bg-blue-600 text-white" }
                                                else { "bg-gray-800 text-gray-400 hover:bg-gray-700" }
                                            )
                                        }
                                        on:click=move |_| {
                                            outcome_draft.set(outcome);
                                            is_saved.set(false);
                                            auto_save_after_outcome_click();
                                        }
                                    >
                                        {label}
                                    </button>
                                }
                            }).collect_view()}
                        </div>
                    </div>
                }
            })}

            {show_duration_picker.then({
                let maybe_auto_save_current_result = Rc::clone(&maybe_auto_save_current_result);
                move || view! {
                    <div class="px-4 pb-3 border-t border-gray-700/20 pt-3">
                        <p class="text-xs text-gray-500 mb-2">"Match duration"</p>
                        <div class="flex gap-1 flex-wrap">
                            {DURATION_OPTIONS.iter().map(|&(value, label)| {
                                let auto_save_after_duration_click = Rc::clone(&maybe_auto_save_current_result);
                                view! {
                                    <button
                                        class=move || {
                                            let active = (duration_mult.get() - value).abs() < 0.01;
                                            format!(
                                                "px-3 py-1 rounded text-xs font-medium min-h-[32px] {}",
                                                if active { "bg-blue-600 text-white" }
                                                else { "bg-gray-800 text-gray-400 hover:bg-gray-700" }
                                            )
                                        }
                                        on:click=move |_| {
                                            duration_mult.set(value);
                                            is_saved.set(false);
                                            auto_save_after_duration_click();
                                        }
                                    >
                                        {label}
                                    </button>
                                }
                            }).collect_view()}
                        </div>
                    </div>
                }
            })}

            {move || footer_error.map(|error_signal| {
                let error_text = error_signal.get();
                (!error_text.is_empty()).then(|| view! {
                    <p class="px-4 pb-2 text-xs text-red-400">{error_text}</p>
                })
            })}

            {(!auto_save).then(|| view! {
                <div class="px-4 pb-4">
                <button
                    class="w-full py-3 bg-blue-600 hover:bg-blue-500 text-white font-semibold rounded-lg transition-colors min-h-[48px]"
                    disabled=move || is_submitting.get()
                    on:click={
                        let submit_current_result = Rc::clone(&submit_current_result);
                        move |_| {
                            submit_current_result();
                        }
                    }
                >
                    {submit_label}
                </button>
                </div>
            })}
        </div>
    }
}
