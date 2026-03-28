//! Player page — accessible via /p/:token
//!
//! Minimalist itinerary style: shows the player's upcoming matches for the current
//! and next rounds. Read-only — players cannot enter scores.
//!
//! After loading the schedule the player selects their name from a list; the page
//! then filters to only their matches. The selection is persisted in localStorage
//! so it survives a refresh.

use crate::meta::use_page_meta;
use crate::sync::{auth_token, pull_events, resolve_token};
use app_core::events::{materialize, EventLog};
use app_core::models::{MatchStatus, PlayerId, PlayerStatus};
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;

const POLL_INTERVAL_MS: u32 = 10_000;

fn storage_key(token: &str) -> String {
    format!("pcpp_player_{token}")
}

fn read_player_selection(token: &str) -> Option<PlayerId> {
    let storage = web_sys::window()
        .and_then(|w| w.local_storage().ok())
        .flatten()?;
    let raw = storage.get_item(&storage_key(token)).ok()??;
    raw.parse::<u32>().ok().map(PlayerId)
}

fn write_player_selection(token: &str, id: PlayerId) {
    if let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok())
        .flatten()
    {
        let _ = storage.set_item(&storage_key(token), &id.0.to_string());
    }
}

fn clear_player_selection(token: &str) {
    if let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok())
        .flatten()
    {
        let _ = storage.remove_item(&storage_key(token));
    }
}

async fn pull_latest_into_log(
    session_id: &str,
    log: RwSignal<Option<EventLog>>,
    error_msg: RwSignal<String>,
    show_errors: bool,
) -> Result<(), String> {
    let since = log
        .get_untracked()
        .as_ref()
        .and_then(|l| l.all().last().map(|ev| ev.session_version as u32))
        .unwrap_or(0);

    let resp = pull_events(session_id, since).await?;
    if resp.events.is_empty() {
        if show_errors {
            error_msg.set(String::new());
        }
        return Ok(());
    }

    let new_events = resp.events;
    log.update(|slot| {
        let mut merged = slot
            .as_ref()
            .map(|existing| existing.all().to_vec())
            .unwrap_or_default();
        merged.extend(new_events);
        *slot = Some(EventLog::from_saved(merged));
    });
    if show_errors {
        error_msg.set(String::new());
    }
    Ok(())
}

#[component]
pub fn PlayerPage() -> impl IntoView {
    use_page_meta(
        "Your Schedule · PCPlayerPicker",
        "Read-only player itinerary for current and upcoming session rounds.",
    );

    let params = use_params_map();
    let token = move || params.with(|p| p.get("token").unwrap_or_default());

    let log: RwSignal<Option<EventLog>> = RwSignal::new(None);
    let error_msg: RwSignal<String> = RwSignal::new(String::new());
    let is_loading = RwSignal::new(true);
    let session_id = RwSignal::new(String::new());

    // Player selection
    let selected_player: RwSignal<Option<PlayerId>> = RwSignal::new(None);
    let resolved_token = RwSignal::new(String::new());

    // PIN gate signals
    let requires_pin = RwSignal::new(false);
    let pin_input = RwSignal::new(String::new());
    let pin_error = RwSignal::new(String::new());
    let tok_for_auth = RwSignal::new(String::new());

    let tok = token();
    leptos::task::spawn_local(async move {
        let info = match resolve_token(&tok).await {
            Ok(i) => i,
            Err(e) => {
                error_msg.set(format!("Invalid link: {e}"));
                is_loading.set(false);
                return;
            }
        };
        if info.requires_pin {
            tok_for_auth.set(tok);
            requires_pin.set(true);
            is_loading.set(false);
            return;
        }
        if info.role != "player" {
            error_msg.set("This link is for players only.".to_string());
            is_loading.set(false);
            return;
        }
        resolved_token.set(tok.clone());
        // Restore saved player selection before rendering
        if let Some(pid) = read_player_selection(&tok) {
            selected_player.set(Some(pid));
        }
        session_id.set(info.session_id.clone());
        match pull_latest_into_log(&info.session_id, log, error_msg, true).await {
            Ok(_) => is_loading.set(false),
            Err(e) => {
                error_msg.set(format!("Failed to load schedule: {e}"));
                is_loading.set(false);
            }
        }
    });

    let stop_polling = RwSignal::new(false);
    on_cleanup(move || {
        stop_polling.set(true);
    });
    leptos::task::spawn_local(async move {
        loop {
            TimeoutFuture::new(POLL_INTERVAL_MS).await;
            if stop_polling.get_untracked() {
                break;
            }
            if is_loading.get_untracked() || requires_pin.get_untracked() {
                continue;
            }
            let sid = session_id.get_untracked();
            if sid.is_empty() {
                continue;
            }
            let _ = pull_latest_into_log(&sid, log, error_msg, false).await;
        }
    });

    view! {
        <div class="min-h-screen bg-gray-950 text-white">
            <header class="px-4 pt-8 pb-5 text-center">
                <div class="flex items-center justify-center gap-3">
                    <div>
                        <h1 class="text-2xl font-bold">"Your Schedule"</h1>
                        <p class="text-gray-400 text-sm mt-1">"PCPlayerPicker"</p>
                    </div>
                    <button
                        class="rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-xs font-semibold text-gray-300 transition-colors hover:border-gray-500 hover:text-white"
                        on:click=move |_| {
                            let sid = session_id.get_untracked();
                            if sid.is_empty() || is_loading.get_untracked() {
                                return;
                            }
                            leptos::task::spawn_local(async move {
                                let _ = pull_latest_into_log(&sid, log, error_msg, true).await;
                            });
                        }
                    >
                        "Refresh"
                    </button>
                </div>
            </header>

            <main class="px-4 pb-8">
                {move || {
                    if is_loading.get() {
                        return view! {
                            <p class="text-center py-12 text-gray-400">"Loading…"</p>
                        }.into_any();
                    }
                    // PIN gate
                    if requires_pin.get() {
                        let tok_val = tok_for_auth.get();
                        return view! {
                            <div class="max-w-sm mx-auto py-12 space-y-4">
                                <h2 class="text-lg font-bold text-center">"PIN Required"</h2>
                                <p class="text-sm text-gray-400 text-center">
                                    "This schedule link is PIN-protected."
                                </p>
                                {move || {
                                    let e = pin_error.get();
                                    (!e.is_empty()).then(|| view! {
                                        <p class="text-sm text-red-400 text-center">{e}</p>
                                    })
                                }}
                                <input
                                    type="password"
                                    inputmode="numeric"
                                    placeholder="Enter PIN"
                                    class="w-full bg-gray-900 border border-gray-700 rounded-xl \
                                           px-4 py-3 text-white placeholder-gray-500 \
                                           focus:outline-none focus:border-blue-500 min-h-[48px]"
                                    prop:value=move || pin_input.get()
                                    on:input=move |ev| pin_input.set(event_target_value(&ev))
                                />
                                <button
                                    class="w-full py-3 bg-blue-600 hover:bg-blue-500 \
                                           text-white font-semibold rounded-xl transition-colors \
                                           min-h-[52px]"
                                    on:click={
                                        let tok_val = tok_val.clone();
                                        move |_| {
                                            let pin = pin_input.get_untracked();
                                            if pin.len() < 4 {
                                                pin_error.set("PIN must be at least 4 digits.".to_string());
                                                return;
                                            }
                                            let tok2 = tok_val.clone();
                                            is_loading.set(true);
                                            requires_pin.set(false);
                                            leptos::task::spawn_local(async move {
                                                match auth_token(&tok2, &pin).await {
                                                    Ok(info) => {
                                                        if info.role != "player" {
                                                            error_msg.set("This link is for players only.".to_string());
                                                            is_loading.set(false);
                                                            return;
                                                        }
                                                        resolved_token.set(tok2.clone());
                                                        if let Some(pid) = read_player_selection(&tok2) {
                                                            selected_player.set(Some(pid));
                                                        }
                                                        session_id.set(info.session_id.clone());
                                                        match pull_latest_into_log(&info.session_id, log, error_msg, true).await {
                                                            Ok(_) => {
                                                                is_loading.set(false);
                                                            }
                                                            Err(e) => {
                                                                error_msg.set(format!("Failed to load: {e}"));
                                                                is_loading.set(false);
                                                            }
                                                        }
                                                    }
                                                    Err(_) => {
                                                        pin_error.set("Incorrect PIN.".to_string());
                                                        requires_pin.set(true);
                                                        is_loading.set(false);
                                                    }
                                                }
                                            });
                                        }
                                    }
                                >
                                    "View Schedule"
                                </button>
                            </div>
                        }.into_any();
                    }
                    let err = error_msg.get();
                    if !err.is_empty() {
                        return view! {
                            <div class="text-center py-12">
                                <p class="text-red-400">{err}</p>
                            </div>
                        }.into_any();
                    }
                    let log_opt = log.get();
                    let el = match log_opt {
                        Some(el) => el,
                        None => return view! {
                            <p class="text-center py-12 text-gray-400">"No schedule yet."</p>
                        }.into_any(),
                    };

                    let state = materialize(&el);

                    // ── Player selector ───────────────────────────────────────────────
                    // Collect active players sorted by name for the picker.
                    let mut player_list: Vec<_> = state.players.values()
                        .filter(|p| p.status == PlayerStatus::Active)
                        .collect();
                    player_list.sort_by(|a, b| a.name.cmp(&b.name));

                    // Validate persisted selection is still in this session.
                    let selection = selected_player.get_untracked();
                    let valid_selection = selection.filter(|pid| state.players.contains_key(pid));
                    if valid_selection != selection {
                        selected_player.set(valid_selection);
                    }

                    if valid_selection.is_none() {
                        // Show the name picker.
                        let tok_val = resolved_token.get_untracked();
                        return view! {
                            <div class="max-w-sm mx-auto py-8 space-y-4">
                                <h2 class="text-lg font-bold text-center">"Who are you?"</h2>
                                <p class="text-sm text-gray-400 text-center">
                                    "Select your name to see only your matches."
                                </p>
                                <div class="space-y-2">
                                    {player_list.into_iter().map(|p| {
                                        let pid = p.id;
                                        let name = p.name.clone();
                                        let tok2 = tok_val.clone();
                                        view! {
                                            <button
                                                class="w-full py-3 bg-gray-900 border border-gray-700 \
                                                       hover:border-blue-500 hover:bg-gray-800 \
                                                       text-white font-medium rounded-xl \
                                                       transition-colors min-h-[52px]"
                                                on:click=move |_| {
                                                    write_player_selection(&tok2, pid);
                                                    selected_player.set(Some(pid));
                                                }
                                            >
                                                {name}
                                            </button>
                                        }
                                    }).collect_view()}
                                </div>
                            </div>
                        }.into_any();
                    }

                    let selected_id = valid_selection.unwrap();
                    let selected_name = state.players.get(&selected_id)
                        .map(|p| p.name.clone())
                        .unwrap_or_default();

                    let round = state.current_round.0;

                    // Show only matches for the selected player in current and next round.
                    let mut matches: Vec<_> = state.matches.values()
                        .filter(|m| {
                            (m.round.0 == round || m.round.0 == round + 1)
                                && m.status != MatchStatus::Voided
                                && (m.team_a.contains(&selected_id) || m.team_b.contains(&selected_id))
                        })
                        .collect();
                    matches.sort_by(|a, b| a.round.0.cmp(&b.round.0).then(a.field.cmp(&b.field)));

                    let tok_val = resolved_token.get_untracked();

                    view! {
                        <div class="space-y-3">
                            // "Not you?" link
                            <div class="flex items-center justify-between mb-1">
                                <span class="text-sm text-gray-400">
                                    "Showing schedule for "
                                    <span class="text-white font-semibold">{selected_name}</span>
                                </span>
                                <button
                                    class="text-xs text-blue-400 hover:text-blue-300 underline \
                                           underline-offset-2 transition-colors"
                                    on:click=move |_| {
                                        clear_player_selection(&tok_val);
                                        selected_player.set(None);
                                    }
                                >
                                    "Not you?"
                                </button>
                            </div>

                            {if matches.is_empty() {
                                view! {
                                    <div class="text-center py-12 text-gray-400">
                                        <p class="text-4xl mb-3">"📋"</p>
                                        <p>"No upcoming matches."</p>
                                        <p class="text-sm mt-1">"Check back after the coach generates the next round."</p>
                                    </div>
                                }.into_any()
                            } else {
                                matches.into_iter().map(|m| {
                                    let team_a_names: Vec<_> = m.team_a.iter()
                                        .filter_map(|id| state.players.get(id).map(|p| p.name.clone()))
                                        .collect();
                                    let team_b_names: Vec<_> = m.team_b.iter()
                                        .filter_map(|id| state.players.get(id).map(|p| p.name.clone()))
                                        .collect();
                                    let on_team_a = m.team_a.contains(&selected_id);
                                    let field = m.field;
                                    let rnd = m.round.0;
                                    let status = m.status.clone();

                                    view! {
                                        <div class="bg-gray-900 border border-gray-700/50 rounded-2xl p-5">
                                            <div class="flex items-center justify-between mb-4">
                                                <div>
                                                    <span class="text-xs font-semibold uppercase \
                                                                 tracking-widest text-gray-500">
                                                        "Round "{rnd}
                                                    </span>
                                                    <span class="ml-3 text-xs text-gray-500">
                                                        "Field "{field}
                                                    </span>
                                                </div>
                                                {match status {
                                                    MatchStatus::Completed =>
                                                        view! { <span class="text-xs text-green-400 font-medium">"Done"</span> }.into_any(),
                                                    MatchStatus::InProgress =>
                                                        view! { <span class="text-xs text-yellow-400 font-medium animate-pulse">"In Progress"</span> }.into_any(),
                                                    _ =>
                                                        view! { <span class="text-xs text-blue-400 font-medium">"Upcoming"</span> }.into_any(),
                                                }}
                                            </div>
                                            <div class="flex items-center gap-4">
                                                <div class=move || format!(
                                                    "flex-1 text-center {}",
                                                    if on_team_a { "ring-1 ring-blue-500/40 rounded-lg py-1" } else { "" }
                                                )>
                                                    {team_a_names.iter().map(|n| view! {
                                                        <p class="font-semibold text-white">{n.clone()}</p>
                                                    }).collect_view()}
                                                </div>
                                                <div class="shrink-0 text-center">
                                                    <span class="text-gray-500 font-black text-sm">"VS"</span>
                                                </div>
                                                <div class=move || format!(
                                                    "flex-1 text-center {}",
                                                    if !on_team_a { "ring-1 ring-blue-500/40 rounded-lg py-1" } else { "" }
                                                )>
                                                    {team_b_names.iter().map(|n| view! {
                                                        <p class="font-semibold text-white">{n.clone()}</p>
                                                    }).collect_view()}
                                                </div>
                                            </div>
                                        </div>
                                    }
                                }).collect_view().into_any()
                            }}
                        </div>
                    }.into_any()
                }}
            </main>
        </div>
    }
}
