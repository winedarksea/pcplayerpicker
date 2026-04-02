use crate::state::{save_session, AppContext};
use app_core::models::{MatchId, MatchStatus, PlayerId, RoundNumber, ScheduledMatch};
use app_core::schedule_edit::validate_round_schedule_update;
use app_core::session::SessionManager;
use leptos::prelude::*;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EditableTeamSide {
    TeamA,
    TeamB,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct EditablePlayerSlot {
    match_id: MatchId,
    team_side: EditableTeamSide,
    slot_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditableRoundMatchDraft {
    match_id: MatchId,
    round: RoundNumber,
    field: u8,
    status: MatchStatus,
    team_a: Vec<Option<PlayerId>>,
    team_b: Vec<Option<PlayerId>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RoundPlayerChangeDraft {
    round: RoundNumber,
    focused_match_id: MatchId,
    original_editable_matches: Vec<EditableRoundMatchDraft>,
    current_editable_matches: Vec<EditableRoundMatchDraft>,
    selected_slot: Option<EditablePlayerSlot>,
}

#[derive(Debug, Clone)]
struct LockedRoundAssignment {
    field: u8,
    status: MatchStatus,
}

#[derive(Debug, Clone)]
struct PlayerPickerOption {
    player_id: PlayerId,
    label: String,
    disabled: bool,
}

#[component]
pub fn RoundPlayerChangeSheet(open_match_id: RwSignal<Option<MatchId>>) -> impl IntoView {
    let ctx = use_context::<AppContext>().expect("AppContext missing");
    let round_change_draft = RwSignal::new(None::<RoundPlayerChangeDraft>);
    let selected_picker_player = RwSignal::new(None::<PlayerId>);
    let sheet_error_message = RwSignal::new(String::new());

    Effect::new(move |_| {
        let open_match = open_match_id.get();
        match open_match {
            Some(match_id) => {
                let draft_is_current = round_change_draft.with(|draft_opt| {
                    draft_opt
                        .as_ref()
                        .map(|draft| draft.focused_match_id == match_id)
                        .unwrap_or(false)
                });
                if draft_is_current {
                    return;
                }

                let next_draft = ctx
                    .session
                    .with(|session_opt| session_opt.as_ref().and_then(|manager| build_round_player_change_draft(manager, match_id)));

                if next_draft.is_some() {
                    round_change_draft.set(next_draft);
                    selected_picker_player.set(None);
                    sheet_error_message.set(String::new());
                } else {
                    open_match_id.set(None);
                    round_change_draft.set(None);
                }
            }
            None => {
                round_change_draft.set(None);
                selected_picker_player.set(None);
                sheet_error_message.set(String::new());
            }
        }
    });

    let close_sheet = move || {
        open_match_id.set(None);
        round_change_draft.set(None);
        selected_picker_player.set(None);
        sheet_error_message.set(String::new());
    };

    let on_select_slot = {
        move |slot: EditablePlayerSlot| {
            round_change_draft.update(|draft_opt| {
                if let Some(draft) = draft_opt {
                    draft.selected_slot = Some(slot);
                }
            });
            selected_picker_player.set(current_player_in_selected_slot(round_change_draft.get_untracked()));
            sheet_error_message.set(String::new());
        }
    };

    let on_pick_player = {
        let ctx = ctx.clone();
        move |player_id: PlayerId| {
            let session_snapshot = ctx.session.get_untracked();
            let Some(manager) = session_snapshot.as_ref() else {
                return;
            };

            let locked_assignments =
                collect_locked_assignments_by_player(&manager.state.matches, draft_round(round_change_draft.get_untracked()));
            let player_names = manager
                .state
                .players
                .iter()
                .map(|(player_id, player)| (*player_id, player.name.clone()))
                .collect::<HashMap<_, _>>();

            let mut next_error_message = None;
            round_change_draft.update(|draft_opt| {
                let Some(draft) = draft_opt else {
                    return;
                };
                let Some(selected_slot) = draft.selected_slot else {
                    next_error_message = Some("Choose a player slot first.".to_string());
                    return;
                };

                match assign_player_to_slot(
                    draft,
                    selected_slot,
                    player_id,
                    &locked_assignments,
                    &player_names,
                ) {
                    Ok(()) => {
                        draft.selected_slot = first_open_slot(draft).or(Some(selected_slot));
                    }
                    Err(error_message) => {
                        next_error_message = Some(error_message);
                    }
                }
            });

            selected_picker_player.set(None);
            sheet_error_message.set(next_error_message.unwrap_or_default());
        }
    };

    let on_save = {
        let ctx = ctx.clone();
        move |_| {
            let session_snapshot = ctx.session.get_untracked();
            let Some(mut manager) = session_snapshot else {
                return;
            };
            let Some(draft) = round_change_draft.get_untracked() else {
                return;
            };

            let Some(updated_matches) = build_updated_matches_from_draft(&draft) else {
                sheet_error_message.set("Every affected match needs a full roster before saving.".to_string());
                return;
            };

            if let Err(error) = validate_round_schedule_update(&manager.state, draft.round, &updated_matches) {
                sheet_error_message.set(error.to_string());
                return;
            }

            if let Err(error) = manager.apply_round_schedule_update(draft.round, updated_matches) {
                sheet_error_message.set(error.to_string());
                return;
            }

            ctx.session.set(Some(manager.clone()));
            save_session(&manager);
            close_sheet();
        }
    };

    view! {
        {move || {
            let draft = round_change_draft.get()?;

            let session_snapshot = ctx.session.get();
            let manager = session_snapshot.as_ref()?;

            let player_names = manager
                .state
                .players
                .iter()
                .map(|(player_id, player)| (*player_id, player.name.clone()))
                .collect::<HashMap<_, _>>();
            let picker_options = build_player_picker_options(manager, &draft, &player_names);
            let visible_matches = build_visible_match_cards(&draft, &player_names);
            let affected_match_summaries = build_affected_match_summaries(&draft, &player_names);
            let selected_slot_label = build_selected_slot_label(&draft, &player_names);
            let can_save = build_updated_matches_from_draft(&draft)
                .and_then(|updated_matches| {
                    validate_round_schedule_update(&manager.state, draft.round, &updated_matches)
                        .ok()
                        .map(|_| ())
                })
                .is_some();
            let missing_slot_count = count_open_slots(&draft);
            let picker_value = selected_picker_player
                .get()
                .map(|player_id| player_id.0.to_string())
                .unwrap_or_default();
            let error_message = sheet_error_message.get();

            Some(view! {
                <div class="fixed inset-0 z-50 flex items-end bg-black/70 backdrop-blur-sm">
                    <button
                        class="absolute inset-0 cursor-default"
                        on:click=move |_| close_sheet()
                        aria-label="Close player change sheet"
                    ></button>
                    <div class="relative max-h-[88vh] w-full overflow-y-auto rounded-t-[28px] border border-white/10 bg-slate-950 px-4 pb-6 pt-4 shadow-[0_-24px_80px_rgba(0,0,0,0.55)]">
                        <div class="mx-auto mb-4 h-1.5 w-12 rounded-full bg-white/20"></div>
                        <div class="mb-4 flex items-start justify-between gap-4">
                            <div>
                                <p class="text-sm font-semibold text-white">"Change Players"</p>
                                <p class="mt-1 text-xs text-gray-400">
                                    "Round "{draft.round.0}" roster edits stay local until you save."
                                </p>
                            </div>
                            <button
                                class="min-h-[40px] rounded-lg border border-white/10 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-white/5"
                                on:click=move |_| close_sheet()
                            >
                                "Cancel"
                            </button>
                        </div>

                        <div class="space-y-3">
                            <div class="rounded-2xl border border-white/10 bg-white/5 p-4">
                                <p class="text-xs font-semibold uppercase tracking-[0.16em] text-gray-500">
                                    "Step 1"
                                </p>
                                <p class="mt-2 text-sm text-white">
                                    {selected_slot_label}
                                </p>
                                <p class="mt-1 text-xs text-gray-400">
                                    "Tap a player chip or empty slot to choose where the next assignment goes."
                                </p>
                            </div>

                            <div class="rounded-2xl border border-white/10 bg-white/5 p-4">
                                <p class="text-xs font-semibold uppercase tracking-[0.16em] text-gray-500">
                                    "Step 2"
                                </p>
                                <label class="mt-2 block text-xs text-gray-400">
                                    "Pick any active player"
                                </label>
                                <select
                                    class="mt-2 min-h-[48px] w-full rounded-xl border border-white/10 bg-slate-900 px-3 py-2 text-sm text-white"
                                    prop:value=move || picker_value.clone()
                                    on:change=move |ev| {
                                        let raw_value = event_target_value(&ev);
                                        if raw_value.is_empty() {
                                            selected_picker_player.set(None);
                                            return;
                                        }
                                        if let Ok(player_number) = raw_value.parse::<u32>() {
                                            let player_id = PlayerId(player_number);
                                            selected_picker_player.set(Some(player_id));
                                            on_pick_player(player_id);
                                        }
                                    }
                                >
                                    <option value="">"Select a player…"</option>
                                    {picker_options.into_iter().map(|picker_option| {
                                        let option_value = picker_option.player_id.0.to_string();
                                        view! {
                                            <option value=option_value disabled=picker_option.disabled>
                                                {picker_option.label}
                                            </option>
                                        }
                                    }).collect_view()}
                                </select>
                                <p class="mt-2 text-xs text-gray-400">
                                    {if missing_slot_count == 0 {
                                        "Save is enabled once the round stays valid."
                                    } else {
                                        "Any moved player leaves a vacancy that must be filled before saving."
                                    }}
                                </p>
                            </div>

                            {(!error_message.is_empty()).then(|| view! {
                                <div class="rounded-2xl border border-red-500/30 bg-red-950/40 p-3 text-sm text-red-200">
                                    {error_message.clone()}
                                </div>
                            })}

                            <div class="space-y-3">
                                {visible_matches.into_iter().map(|match_card| {
                                    let field = match_card.field;
                                    let status = match_card.status.clone();
                                    let team_a_slots = match_card.team_a_slots.clone();
                                    let team_b_slots = match_card.team_b_slots.clone();
                                    view! {
                                        <div class="rounded-2xl border border-white/10 bg-white/5 p-4">
                                            <div class="mb-3 flex items-center justify-between">
                                                <span class="text-xs font-semibold uppercase tracking-[0.16em] text-gray-500">
                                                    "Field "{field}
                                                </span>
                                                {match status {
                                                    MatchStatus::Completed =>
                                                        view! { <span class="text-xs font-medium text-green-400">"Done"</span> }.into_any(),
                                                    MatchStatus::InProgress =>
                                                        view! { <span class="text-xs font-medium text-yellow-300">"In Progress"</span> }.into_any(),
                                                    MatchStatus::Scheduled =>
                                                        view! { <span class="text-xs font-medium text-gray-300">"Scheduled"</span> }.into_any(),
                                                    MatchStatus::Voided =>
                                                        view! { <span class="text-xs font-medium text-red-300">"Voided"</span> }.into_any(),
                                                }}
                                            </div>
                                            <div class="grid gap-3 md:grid-cols-[1fr_auto_1fr] md:items-start">
                                                <div class="space-y-2">
                                                    {team_a_slots.into_iter().map(|slot_view| {
                                                        let button_class = slot_button_class(slot_view.is_selected, slot_view.is_empty);
                                                        let slot = slot_view.slot;
                                                        let label = slot_view.label;
                                                        view! {
                                                            <button
                                                                class=button_class
                                                                on:click=move |_| on_select_slot(slot)
                                                            >
                                                                {label}
                                                            </button>
                                                        }
                                                    }).collect_view()}
                                                </div>
                                                <div class="flex items-center justify-center text-sm font-semibold text-gray-500">
                                                    "vs"
                                                </div>
                                                <div class="space-y-2">
                                                    {team_b_slots.into_iter().map(|slot_view| {
                                                        let button_class = slot_button_class(slot_view.is_selected, slot_view.is_empty);
                                                        let slot = slot_view.slot;
                                                        let label = slot_view.label;
                                                        view! {
                                                            <button
                                                                class=button_class
                                                                on:click=move |_| on_select_slot(slot)
                                                            >
                                                                {label}
                                                            </button>
                                                        }
                                                    }).collect_view()}
                                                </div>
                                            </div>
                                        </div>
                                    }
                                }).collect_view()}
                            </div>

                            {(!affected_match_summaries.is_empty()).then(|| view! {
                                <div class="rounded-2xl border border-white/10 bg-white/5 p-4">
                                    <p class="text-xs font-semibold uppercase tracking-[0.16em] text-gray-500">
                                        "Preview"
                                    </p>
                                    <div class="mt-2 space-y-2">
                                        {affected_match_summaries.into_iter().map(|summary| view! {
                                            <p class="text-sm text-gray-200">{summary}</p>
                                        }).collect_view()}
                                    </div>
                                </div>
                            })}

                            <button
                                class="min-h-[48px] w-full rounded-xl bg-blue-600 px-4 py-3 text-sm font-semibold text-white transition-colors hover:bg-blue-500 disabled:cursor-not-allowed disabled:opacity-50"
                                disabled=move || !can_save
                                on:click=on_save
                            >
                                "Save Player Changes"
                            </button>
                        </div>
                    </div>
                </div>
            })
        }}
    }
}

#[derive(Debug, Clone)]
struct SlotButtonViewModel {
    slot: EditablePlayerSlot,
    label: String,
    is_selected: bool,
    is_empty: bool,
}

#[derive(Debug, Clone)]
struct VisibleMatchCardViewModel {
    field: u8,
    status: MatchStatus,
    team_a_slots: Vec<SlotButtonViewModel>,
    team_b_slots: Vec<SlotButtonViewModel>,
}

fn build_round_player_change_draft(
    manager: &SessionManager,
    focused_match_id: MatchId,
) -> Option<RoundPlayerChangeDraft> {
    let focused_match = manager.state.matches.get(&focused_match_id)?;
    if focused_match.status == MatchStatus::Completed || focused_match.status == MatchStatus::Voided {
        return None;
    }

    let mut editable_matches: Vec<EditableRoundMatchDraft> = manager
        .state
        .matches
        .values()
        .filter(|scheduled_match| {
            scheduled_match.round == focused_match.round
                && scheduled_match.status != MatchStatus::Completed
                && scheduled_match.status != MatchStatus::Voided
        })
        .map(build_editable_match_draft)
        .collect();
    editable_matches.sort_by_key(|editable_match| editable_match.field);

    Some(RoundPlayerChangeDraft {
        round: focused_match.round,
        focused_match_id,
        original_editable_matches: editable_matches.clone(),
        current_editable_matches: editable_matches,
        selected_slot: None,
    })
}

fn build_editable_match_draft(scheduled_match: &ScheduledMatch) -> EditableRoundMatchDraft {
    EditableRoundMatchDraft {
        match_id: scheduled_match.id,
        round: scheduled_match.round,
        field: scheduled_match.field,
        status: scheduled_match.status.clone(),
        team_a: scheduled_match.team_a.iter().map(|player_id| Some(*player_id)).collect(),
        team_b: scheduled_match.team_b.iter().map(|player_id| Some(*player_id)).collect(),
    }
}

fn build_player_picker_options(
    manager: &SessionManager,
    draft: &RoundPlayerChangeDraft,
    player_names: &HashMap<PlayerId, String>,
) -> Vec<PlayerPickerOption> {
    let current_assignments = collect_editable_assignments_by_player(draft);
    let locked_assignments = collect_locked_assignments_by_player(&manager.state.matches, Some(draft.round));

    let mut picker_options: Vec<PlayerPickerOption> = manager
        .state
        .active_players()
        .map(|player| {
            let assignment_label = if let Some(locked_assignment) = locked_assignments.get(&player.id) {
                format!(
                    "Field {} {}",
                    locked_assignment.field,
                    match locked_assignment.status {
                        MatchStatus::Completed => "(done)",
                        MatchStatus::InProgress => "(in progress)",
                        MatchStatus::Scheduled => "(scheduled)",
                        MatchStatus::Voided => "(voided)",
                    }
                )
            } else if let Some(editable_match) = current_assignments.get(&player.id) {
                format!("Field {}", editable_match.field)
            } else {
                "bench".to_string()
            };

            PlayerPickerOption {
                player_id: player.id,
                label: format!(
                    "{} ({assignment_label})",
                    player_names
                        .get(&player.id)
                        .cloned()
                        .unwrap_or_else(|| format!("#{}", player.id.0))
                ),
                disabled: locked_assignments.contains_key(&player.id),
            }
        })
        .collect();

    picker_options.sort_by_cached_key(|picker_option| picker_option.label.to_ascii_lowercase());
    picker_options
}

fn collect_editable_assignments_by_player(
    draft: &RoundPlayerChangeDraft,
) -> HashMap<PlayerId, EditableRoundMatchDraft> {
    let mut assignments = HashMap::new();
    for editable_match in &draft.current_editable_matches {
        for player_id in editable_match
            .team_a
            .iter()
            .chain(editable_match.team_b.iter())
            .flatten()
        {
            assignments.insert(*player_id, editable_match.clone());
        }
    }
    assignments
}

fn collect_locked_assignments_by_player(
    matches_by_id: &HashMap<MatchId, ScheduledMatch>,
    round: Option<RoundNumber>,
) -> HashMap<PlayerId, LockedRoundAssignment> {
    let mut assignments = HashMap::new();
    for scheduled_match in matches_by_id.values() {
        if scheduled_match.status != MatchStatus::Completed {
            continue;
        }
        if round.is_some_and(|round_number| scheduled_match.round != round_number) {
            continue;
        }
        for player_id in scheduled_match
            .team_a
            .iter()
            .chain(scheduled_match.team_b.iter())
        {
            assignments.insert(
                *player_id,
                LockedRoundAssignment {
                    field: scheduled_match.field,
                    status: scheduled_match.status.clone(),
                },
            );
        }
    }
    assignments
}

fn assign_player_to_slot(
    draft: &mut RoundPlayerChangeDraft,
    selected_slot: EditablePlayerSlot,
    player_id: PlayerId,
    locked_assignments: &HashMap<PlayerId, LockedRoundAssignment>,
    player_names: &HashMap<PlayerId, String>,
) -> Result<(), String> {
    if let Some(locked_assignment) = locked_assignments.get(&player_id) {
        let player_name = player_names
            .get(&player_id)
            .cloned()
            .unwrap_or_else(|| format!("#{}", player_id.0));
        return Err(format!(
            "{player_name} is locked on Field {} because that match is already complete.",
            locked_assignment.field
        ));
    }

    let existing_slot_for_player = find_slot_for_player(&draft.current_editable_matches, player_id);

    if let Some(editable_match) = draft
        .current_editable_matches
        .iter_mut()
        .find(|editable_match| editable_match.match_id == selected_slot.match_id)
    {
        if let Some(slot_value) = slot_value_mut(editable_match, selected_slot.team_side, selected_slot.slot_index) {
            *slot_value = Some(player_id);
        }
    }

    if let Some(existing_slot) = existing_slot_for_player {
        if existing_slot != selected_slot {
            if let Some(existing_match) = draft
                .current_editable_matches
                .iter_mut()
                .find(|editable_match| editable_match.match_id == existing_slot.match_id)
            {
                if let Some(slot_value) =
                    slot_value_mut(existing_match, existing_slot.team_side, existing_slot.slot_index)
                {
                    *slot_value = None;
                }
            }
        }
    }

    Ok(())
}

fn build_visible_match_cards(
    draft: &RoundPlayerChangeDraft,
    player_names: &HashMap<PlayerId, String>,
) -> Vec<VisibleMatchCardViewModel> {
    let match_ids_to_show = visible_match_ids(draft);
    let mut visible_match_cards: Vec<VisibleMatchCardViewModel> = draft
        .current_editable_matches
        .iter()
        .filter(|editable_match| match_ids_to_show.contains(&editable_match.match_id))
        .map(|editable_match| VisibleMatchCardViewModel {
            field: editable_match.field,
            status: editable_match.status.clone(),
            team_a_slots: build_slot_view_models(
                draft,
                editable_match,
                EditableTeamSide::TeamA,
                player_names,
            ),
            team_b_slots: build_slot_view_models(
                draft,
                editable_match,
                EditableTeamSide::TeamB,
                player_names,
            ),
        })
        .collect();
    visible_match_cards.sort_by_key(|visible_match| visible_match.field);
    visible_match_cards
}

fn visible_match_ids(draft: &RoundPlayerChangeDraft) -> HashSet<MatchId> {
    let mut visible_match_ids = HashSet::from([draft.focused_match_id]);
    for editable_match in &draft.current_editable_matches {
        let original_match = draft
            .original_editable_matches
            .iter()
            .find(|original_match| original_match.match_id == editable_match.match_id);
        let roster_changed = original_match
            .map(|original_match| {
                original_match.team_a != editable_match.team_a || original_match.team_b != editable_match.team_b
            })
            .unwrap_or(false);
        let has_open_slot = editable_match
            .team_a
            .iter()
            .chain(editable_match.team_b.iter())
            .any(|player_id| player_id.is_none());
        if roster_changed || has_open_slot {
            visible_match_ids.insert(editable_match.match_id);
        }
    }
    visible_match_ids
}

fn build_slot_view_models(
    draft: &RoundPlayerChangeDraft,
    editable_match: &EditableRoundMatchDraft,
    team_side: EditableTeamSide,
    player_names: &HashMap<PlayerId, String>,
) -> Vec<SlotButtonViewModel> {
    let slots = match team_side {
        EditableTeamSide::TeamA => &editable_match.team_a,
        EditableTeamSide::TeamB => &editable_match.team_b,
    };

    slots
        .iter()
        .enumerate()
        .map(|(slot_index, player_id)| {
            let slot = EditablePlayerSlot {
                match_id: editable_match.match_id,
                team_side,
                slot_index,
            };
            let player_label = player_id
                .and_then(|player_id| player_names.get(&player_id).cloned())
                .unwrap_or_else(|| "Open slot".to_string());
            SlotButtonViewModel {
                slot,
                label: player_label,
                is_selected: draft.selected_slot == Some(slot),
                is_empty: player_id.is_none(),
            }
        })
        .collect()
}

fn build_selected_slot_label(
    draft: &RoundPlayerChangeDraft,
    player_names: &HashMap<PlayerId, String>,
) -> String {
    let Some(selected_slot) = draft.selected_slot else {
        return "Select the player slot you want to edit.".to_string();
    };
    let Some(selected_match) = draft
        .current_editable_matches
        .iter()
        .find(|editable_match| editable_match.match_id == selected_slot.match_id)
    else {
        return "Select the player slot you want to edit.".to_string();
    };
    let slot_player_name = slot_player_id(selected_match, selected_slot.team_side, selected_slot.slot_index)
        .and_then(|player_id| player_names.get(&player_id).cloned())
        .unwrap_or_else(|| "open slot".to_string());
    format!(
        "Field {} is selected. The current slot contains {}.",
        selected_match.field, slot_player_name
    )
}

fn build_affected_match_summaries(
    draft: &RoundPlayerChangeDraft,
    player_names: &HashMap<PlayerId, String>,
) -> Vec<String> {
    let mut summaries = Vec::new();
    for current_match in &draft.current_editable_matches {
        let Some(original_match) = draft
            .original_editable_matches
            .iter()
            .find(|original_match| original_match.match_id == current_match.match_id)
        else {
            continue;
        };

        if original_match.team_a == current_match.team_a && original_match.team_b == current_match.team_b {
            continue;
        }

        let current_names = current_match
            .team_a
            .iter()
            .chain(current_match.team_b.iter())
            .map(|player_id| {
                player_id
                    .and_then(|player_id| player_names.get(&player_id).cloned())
                    .unwrap_or_else(|| "Open slot".to_string())
            })
            .collect::<Vec<_>>();
        let open_slot_count = current_match
            .team_a
            .iter()
            .chain(current_match.team_b.iter())
            .filter(|player_id| player_id.is_none())
            .count();

        if open_slot_count == 0 {
            summaries.push(format!(
                "Field {} will become {}.",
                current_match.field,
                current_names.join(", ")
            ));
        } else {
            summaries.push(format!(
                "Field {} now needs {} more player{}.",
                current_match.field,
                open_slot_count,
                if open_slot_count == 1 { "" } else { "s" }
            ));
        }
    }
    summaries
}

fn build_updated_matches_from_draft(draft: &RoundPlayerChangeDraft) -> Option<Vec<ScheduledMatch>> {
    let mut updated_matches = Vec::with_capacity(draft.current_editable_matches.len());
    for editable_match in &draft.current_editable_matches {
        let team_a = editable_match.team_a.iter().copied().collect::<Option<Vec<_>>>()?;
        let team_b = editable_match.team_b.iter().copied().collect::<Option<Vec<_>>>()?;
        updated_matches.push(ScheduledMatch {
            id: editable_match.match_id,
            round: editable_match.round,
            field: editable_match.field,
            team_a,
            team_b,
            status: editable_match.status.clone(),
        });
    }
    Some(updated_matches)
}

fn count_open_slots(draft: &RoundPlayerChangeDraft) -> usize {
    draft
        .current_editable_matches
        .iter()
        .flat_map(|editable_match| editable_match.team_a.iter().chain(editable_match.team_b.iter()))
        .filter(|player_id| player_id.is_none())
        .count()
}

fn first_open_slot(draft: &RoundPlayerChangeDraft) -> Option<EditablePlayerSlot> {
    for editable_match in &draft.current_editable_matches {
        if let Some(slot_index) = editable_match.team_a.iter().position(|player_id| player_id.is_none()) {
            return Some(EditablePlayerSlot {
                match_id: editable_match.match_id,
                team_side: EditableTeamSide::TeamA,
                slot_index,
            });
        }
        if let Some(slot_index) = editable_match.team_b.iter().position(|player_id| player_id.is_none()) {
            return Some(EditablePlayerSlot {
                match_id: editable_match.match_id,
                team_side: EditableTeamSide::TeamB,
                slot_index,
            });
        }
    }
    None
}

fn find_slot_for_player(
    editable_matches: &[EditableRoundMatchDraft],
    player_id: PlayerId,
) -> Option<EditablePlayerSlot> {
    for editable_match in editable_matches {
        if let Some(slot_index) = editable_match.team_a.iter().position(|slot_player| *slot_player == Some(player_id)) {
            return Some(EditablePlayerSlot {
                match_id: editable_match.match_id,
                team_side: EditableTeamSide::TeamA,
                slot_index,
            });
        }
        if let Some(slot_index) = editable_match.team_b.iter().position(|slot_player| *slot_player == Some(player_id)) {
            return Some(EditablePlayerSlot {
                match_id: editable_match.match_id,
                team_side: EditableTeamSide::TeamB,
                slot_index,
            });
        }
    }
    None
}

fn slot_value_mut(
    editable_match: &mut EditableRoundMatchDraft,
    team_side: EditableTeamSide,
    slot_index: usize,
) -> Option<&mut Option<PlayerId>> {
    match team_side {
        EditableTeamSide::TeamA => editable_match.team_a.get_mut(slot_index),
        EditableTeamSide::TeamB => editable_match.team_b.get_mut(slot_index),
    }
}

fn slot_player_id(
    editable_match: &EditableRoundMatchDraft,
    team_side: EditableTeamSide,
    slot_index: usize,
) -> Option<PlayerId> {
    match team_side {
        EditableTeamSide::TeamA => editable_match.team_a.get(slot_index).copied().flatten(),
        EditableTeamSide::TeamB => editable_match.team_b.get(slot_index).copied().flatten(),
    }
}

fn current_player_in_selected_slot(draft: Option<RoundPlayerChangeDraft>) -> Option<PlayerId> {
    let draft = draft?;
    let selected_slot = draft.selected_slot?;
    let editable_match = draft
        .current_editable_matches
        .iter()
        .find(|editable_match| editable_match.match_id == selected_slot.match_id)?;
    slot_player_id(editable_match, selected_slot.team_side, selected_slot.slot_index)
}

fn draft_round(draft: Option<RoundPlayerChangeDraft>) -> Option<RoundNumber> {
    draft.map(|draft| draft.round)
}

fn slot_button_class(is_selected: bool, is_empty: bool) -> &'static str {
    match (is_selected, is_empty) {
        (true, true) => {
            "min-h-[44px] w-full rounded-xl border border-amber-300 bg-amber-500/20 px-3 py-2 text-left text-sm font-medium text-amber-100 transition-colors"
        }
        (true, false) => {
            "min-h-[44px] w-full rounded-xl border border-blue-300 bg-blue-500/20 px-3 py-2 text-left text-sm font-medium text-blue-50 transition-colors"
        }
        (false, true) => {
            "min-h-[44px] w-full rounded-xl border border-dashed border-amber-500/60 bg-amber-950/30 px-3 py-2 text-left text-sm font-medium text-amber-200 transition-colors hover:bg-amber-900/40"
        }
        (false, false) => {
            "min-h-[44px] w-full rounded-xl border border-white/10 bg-slate-900 px-3 py-2 text-left text-sm font-medium text-white transition-colors hover:bg-slate-800"
        }
    }
}
