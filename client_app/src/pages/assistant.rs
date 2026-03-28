//! Assistant page — accessible via /a/:token
//!
//! Bootstraps by resolving the token, fetching all events, materializing state.
//! Score submission posts events directly to the worker API.

use crate::meta::use_page_meta;
use crate::sync::{api_base, auth_token, pull_events, resolve_token};
use app_core::events::{materialize, Event, EventLog};
use app_core::models::{MatchResult, MatchStatus, PlayerMatchScore, Role};
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use std::collections::HashMap;

const POLL_INTERVAL_MS: u32 = 10_000;

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
pub fn AssistantPage() -> impl IntoView {
    use_page_meta(
        "Assistant View · PCPlayerPicker",
        "Enter match scores and watch live rankings from an online coach session.",
    );

    let params = use_params_map();
    let token = move || params.with(|p| p.get("token").unwrap_or_default());

    // Resolved session_id (set after token lookup)
    let session_id: RwSignal<String> = RwSignal::new(String::new());
    let log: RwSignal<Option<EventLog>> = RwSignal::new(None);
    let error_msg: RwSignal<String> = RwSignal::new(String::new());
    let is_loading = RwSignal::new(true);

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
                error_msg.set(format!("Invalid token: {e}"));
                is_loading.set(false);
                return;
            }
        };
        if info.requires_pin {
            // Stop here — show PIN gate
            tok_for_auth.set(tok);
            requires_pin.set(true);
            is_loading.set(false);
            return;
        }
        if info.role != "assistant" {
            error_msg.set("This link is for assistants only.".to_string());
            is_loading.set(false);
            return;
        }
        session_id.set(info.session_id.clone());
        match pull_latest_into_log(&info.session_id, log, error_msg, true).await {
            Ok(_) => is_loading.set(false),
            Err(e) => {
                error_msg.set(format!("Failed to load session: {e}"));
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
            <header class="px-4 pt-6 pb-4 border-b border-gray-800">
                <div class="flex items-center justify-between gap-3">
                    <div>
                        <h1 class="text-xl font-bold">"Assistant View"</h1>
                        <p class="text-xs text-gray-400 mt-0.5">"Score entry · Live rankings"</p>
                    </div>
                    <button
                        class="shrink-0 rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-xs font-semibold text-gray-300 transition-colors hover:border-gray-500 hover:text-white"
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

            <main class="px-4 py-5">
                {move || {
                    if is_loading.get() {
                        return view! {
                            <p class="text-center py-16 text-gray-400">"Loading session…"</p>
                        }.into_any();
                    }
                    // PIN gate
                    if requires_pin.get() {
                        let tok_val = tok_for_auth.get();
                        return view! {
                            <div class="max-w-sm mx-auto py-16 space-y-4">
                                <h2 class="text-lg font-bold text-center">"PIN Required"</h2>
                                <p class="text-sm text-gray-400 text-center">
                                    "This link is PIN-protected. Enter the PIN to continue."
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
                                    on:input=move |ev| pin_input.set(leptos::prelude::event_target_value(&ev))
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
                                                        if info.role != "assistant" {
                                                            error_msg.set("This link is for assistants only.".to_string());
                                                            is_loading.set(false);
                                                            return;
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
                                    "Unlock"
                                </button>
                            </div>
                        }.into_any();
                    }
                    let err = error_msg.get();
                    if !err.is_empty() {
                        return view! {
                            <div class="text-center py-16">
                                <p class="text-red-400 font-medium">"Error"</p>
                                <p class="text-gray-400 text-sm mt-1">{err}</p>
                            </div>
                        }.into_any();
                    }
                    let log_opt = log.get();
                    let el = match log_opt {
                        Some(el) => el,
                        None => return view! {
                            <p class="text-center py-16 text-gray-400">"No data"</p>
                        }.into_any(),
                    };

                    let state = materialize(&el);
                    let round = state.current_round.0;
                    let player_names: HashMap<_, _> = state.players.iter()
                        .map(|(id, p)| (*id, p.name.clone()))
                        .collect();
                    let mut matches: Vec<_> = state.matches.values()
                        .filter(|m| m.round.0 == round && m.status != MatchStatus::Voided)
                        .collect();
                    matches.sort_by_key(|m| m.field);
                    let rankings = state.rankings.clone();
                    let player_map = state.players.clone();
                    let sid = session_id.get();
                    let tok_val = token();

                    view! {
                        <div class="space-y-6">
                            // Score entry
                            <div>
                                <h2 class="font-semibold text-lg mb-3">
                                    "Round "{round}" · Score Entry"
                                </h2>
                                {if matches.is_empty() {
                                    view! {
                                        <p class="text-gray-400 text-sm">
                                            "No matches scheduled for this round yet."
                                        </p>
                                    }.into_any()
                                } else {
                                    view! {
                                        <div class="space-y-4">
                                            {matches.into_iter().map(|m| {
                                                let mid = m.id;
                                                let field = m.field;
                                                let rnd = m.round.0;
                                                let team_a = m.team_a.clone();
                                                let team_b = m.team_b.clone();
                                                let existing = state.results.get(&mid).cloned();
                                                let pnames = player_names.clone();
                                                let sid2 = sid.clone();
                                                let tok2 = tok_val.clone();
                                                view! {
                                                    <AssistantScoreCard
                                                        match_id=mid field=field round=rnd
                                                        team_a=team_a team_b=team_b
                                                        player_names=pnames
                                                        existing_result=existing
                                                        session_id=sid2
                                                        token=tok2
                                                    />
                                                }
                                            }).collect_view()}
                                        </div>
                                    }.into_any()
                                }}
                            </div>

                            // Live rankings (if available)
                            {(!rankings.is_empty()).then(|| {
                                let mut sorted = rankings;
                                sorted.sort_by_key(|r| r.rank);
                                view! {
                                    <div>
                                        <h2 class="font-semibold text-lg mb-3">"Live Rankings"</h2>
                                        <div class="space-y-2">
                                            {sorted.into_iter().map(|r| {
                                                let name = player_map.get(&r.player_id)
                                                    .map(|p| p.name.clone())
                                                    .unwrap_or_else(|| format!("#{}", r.player_id.0));
                                                let rank = r.rank;
                                                let is_active = r.is_active;
                                                view! {
                                                    <div class="flex items-center gap-3 bg-gray-900 \
                                                                border border-gray-700/50 rounded-xl px-4 py-3">
                                                        <span class=move || format!(
                                                            "w-8 font-bold text-lg tabular-nums {}",
                                                            if rank <= 3 { "text-yellow-400" } else { "text-gray-500" }
                                                        )>{rank}</span>
                                                        <span class=move || format!(
                                                            "flex-1 font-medium {}",
                                                            if is_active { "text-white" } else { "text-gray-500 line-through" }
                                                        )>{name}</span>
                                                        <span class="text-xs text-gray-500">
                                                            {r.rank_range_90.0}"–"{r.rank_range_90.1}
                                                        </span>
                                                    </div>
                                                }
                                            }).collect_view()}
                                        </div>
                                    </div>
                                }
                            })}
                        </div>
                    }.into_any()
                }}
            </main>
        </div>
    }
}

// ── Score card that posts via API ─────────────────────────────────────────────

#[component]
fn AssistantScoreCard(
    match_id: app_core::models::MatchId,
    field: u8,
    round: u32,
    team_a: Vec<app_core::models::PlayerId>,
    team_b: Vec<app_core::models::PlayerId>,
    player_names: HashMap<app_core::models::PlayerId, String>,
    existing_result: Option<MatchResult>,
    session_id: String,
    token: String,
) -> impl IntoView {
    use app_core::models::PlayerId;

    let all_ids: Vec<PlayerId> = team_a.iter().chain(team_b.iter()).cloned().collect();
    let init: HashMap<PlayerId, Option<u16>> = all_ids
        .iter()
        .map(|id| {
            let goals = existing_result
                .as_ref()
                .and_then(|r| r.scores.get(id))
                .and_then(|s| s.goals);
            (*id, goals)
        })
        .collect();

    let draft = RwSignal::new(init);
    let is_saved = RwSignal::new(existing_result.is_some());
    let is_submitting = RwSignal::new(false);
    let save_error = RwSignal::new(String::new());

    view! {
        <div class="bg-gray-900 border border-gray-700/50 rounded-xl overflow-hidden">
            <div class="px-4 pt-4 pb-3 border-b border-gray-700/30 flex items-center justify-between">
                <span class="text-sm font-semibold">"Field "{field}" · Rd "{round}</span>
                {move || is_saved.get().then(|| view! {
                    <span class="text-xs text-green-400 font-medium">"Submitted ✓"</span>
                })}
            </div>
            <div class="px-4 py-3 space-y-3">
                {all_ids.iter().map(|&pid| {
                    let name = player_names.get(&pid).cloned()
                        .unwrap_or_else(|| format!("#{}", pid.0));
                    let on_team_a = team_a.contains(&pid);
                    view! {
                        <div class="flex items-center gap-2 flex-wrap">
                            <span class=move || format!("w-2 h-2 rounded-full shrink-0 {}",
                                if on_team_a { "bg-blue-400" } else { "bg-orange-400" })/>
                            <span class="flex-1 min-w-[80px] text-sm truncate">{name}</span>
                            <div class="flex gap-1 flex-wrap">
                                <button
                                    class=move || {
                                        let active = draft.with(|d| d.get(&pid).copied().flatten().is_none());
                                        format!("px-2 py-1 rounded min-h-[36px] min-w-[40px] text-xs {}",
                                            if active { "bg-gray-600 text-white font-semibold" }
                                            else { "bg-gray-800 text-gray-400" })
                                    }
                                    on:click=move |_| { draft.update(|d| { d.insert(pid, None); }); }
                                >"DNP"</button>
                                {(0u16..=9).map(|n| view! {
                                    <button
                                        class=move || {
                                            let active = draft.with(|d| *d.get(&pid).unwrap_or(&None) == Some(n));
                                            format!("px-2 py-1 rounded min-h-[36px] min-w-[32px] font-semibold text-sm {}",
                                                if active { "bg-blue-600 text-white" }
                                                else { "bg-gray-800 text-gray-400" })
                                        }
                                        on:click=move |_| { draft.update(|d| { d.insert(pid, Some(n)); }); }
                                    >{n}</button>
                                }).collect_view()}
                            </div>
                        </div>
                    }
                }).collect_view()}
            </div>
            {move || {
                let err = save_error.get();
                (!err.is_empty()).then(|| view! {
                    <p class="px-4 pb-2 text-xs text-red-400">{err.clone()}</p>
                })
            }}
            <div class="px-4 pb-4">
                <button
                    class="w-full py-3 bg-blue-600 hover:bg-blue-500 text-white \
                           font-semibold rounded-lg transition-colors min-h-[48px]"
                    disabled=move || is_submitting.get()
                    on:click={
                        let all_ids2 = all_ids.clone();
                        let session_id2 = session_id.clone();
                        let token2 = token.clone();
                        move |_| {
                            let scores: HashMap<_, _> = draft.with(|d| {
                                all_ids2.iter().map(|id| {
                                    (*id, PlayerMatchScore { goals: *d.get(id).unwrap_or(&None) })
                                }).collect()
                            });
                            let result = MatchResult {
                                match_id,
                                scores,
                                duration_multiplier: 1.0,
                                entered_by: Role::Assistant,
                            };
                            // Build minimal EventEnvelope and push to API
                            let envelope = app_core::events::EventEnvelope {
                                id: app_core::models::EventId(0), // server assigns real ID
                                session_version: 0,               // server assigns version
                                entered_by: Role::Assistant,
                                payload: Event::ScoreEntered {
                                    match_id: result.match_id,
                                    result: result.clone(),
                                },
                            };
                            let sid = session_id2.clone();
                            let tok = token2.clone();
                            is_submitting.set(true);
                            is_saved.set(false);
                            save_error.set(String::new());
                            leptos::task::spawn_local(async move {
                                let body = serde_json::json!({
                                    "events": [envelope],
                                    "token": tok,
                                });
                                let url = format!("{}/api/sessions/{}/events", api_base(), sid);
                                match crate::sync::fetch_post_json(&url, &body.to_string()).await {
                                    Ok(_) => {
                                        is_saved.set(true);
                                        save_error.set(String::new());
                                    }
                                    Err(e) => {
                                        is_saved.set(false);
                                        save_error.set(format!("Submit failed: {e}"));
                                    }
                                }
                                is_submitting.set(false);
                            });
                        }
                    }
                >
                    {move || if is_submitting.get() { "Submitting…" } else { "Submit Scores" }}
                </button>
            </div>
        </div>
    }
}
