use super::schedule_export::{
    build_round_schedule_share_snapshot, copy_text_to_clipboard, format_round_schedule_share_text,
    share_round_schedule_image, RoundScheduleImageShareOutcome,
};
use crate::coach_sync::pull_assistant_score_events;
use crate::meta::use_page_meta;
use crate::state::{load_session, save_session, AppContext};
use crate::sync::{
    go_online, load_sync_state, push_new_events, push_session_archive, set_recovery_pin,
    set_token_pin, SessionArchive, SyncState,
};
/// Coach dashboard — tab layout driven by a `:tab` URL param.
///
/// Route pattern:
///   /coach/session/:id           → default (Matches tab)
///   /coach/session/:id/:tab      → explicit tab (matches | results | analysis | online | players)
use app_core::events::{materialize, Event};
use app_core::io::csv::{self, import_rankings};
use app_core::models::{MatchId, MatchResult, MatchStatus, PlayerId, PlayerMatchScore, Role};
use app_core::ranking::goal_model::GoalModelEngine;
use app_core::ranking::RankingEngine;
use app_core::scheduler::{select_scheduler, ScheduleGenerationRequest};
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use std::collections::HashMap;
use wasm_bindgen::JsCast;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn tab_class(active: bool) -> &'static str {
    if active {
        "shrink-0 min-w-[88px] px-3 py-3 text-white font-semibold text-xs text-center \
         border-b-2 border-blue-500 transition-colors md:flex-1"
    } else {
        "shrink-0 min-w-[88px] px-3 py-3 text-gray-400 hover:text-gray-200 font-medium \
         text-xs text-center border-b-2 border-transparent transition-colors md:flex-1"
    }
}

fn select_input_text_on_focus(ev: leptos::ev::FocusEvent) {
    let Some(target) = ev.target() else {
        return;
    };
    let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() else {
        return;
    };
    input.select();
}

// ── Dashboard page (parent + tab router) ─────────────────────────────────────

#[component]
pub fn DashboardPage() -> impl IntoView {
    use_page_meta(
        "Session Dashboard · PCPlayerPicker",
        "Manage schedules, score entry, rankings, and online sharing for an active session.",
    );

    let params = use_params_map();
    let ctx = use_context::<AppContext>().expect("AppContext missing");

    let session_id = move || params.with(|p| p.get("id").unwrap_or_default());

    let active_tab = move || {
        params.with(|p| {
            let t = p.get("tab").unwrap_or_default();
            if t.is_empty() {
                "matches".to_string()
            } else {
                t
            }
        })
    };

    // Load from durable browser storage if the context session doesn't match this URL.
    Effect::new(move |_| {
        ctx.storage_restore_epoch.get();
        let id = params.with(|p| p.get("id").unwrap_or_default());
        if id.is_empty() {
            ctx.session.set(None);
            return;
        }
        let needs_load = ctx.session.with_untracked(|s| {
            s.as_ref()
                .and_then(|m| m.state.config.as_ref())
                .map(|c| c.id.to_string() != id)
                .unwrap_or(true)
        });
        if needs_load {
            ctx.session.set(None);
            leptos::task::spawn_local(async move {
                if let Some(manager) = load_session(&id).await {
                    ctx.session.set(Some(manager));
                }
            });
        }
    });

    view! {
        <div class="app-theme min-h-screen bg-gray-950 text-white flex flex-col">

            // ── Top bar ───────────────────────────────────────────────────
            <header class="flex items-center gap-3 px-4 pt-5 pb-3">
                <A
                    href="/coach"
                    attr:class="text-gray-400 hover:text-white text-2xl leading-none \
                                min-w-[44px] min-h-[44px] flex items-center"
                >
                    "←"
                </A>
                <div class="flex-1 min-w-0">
                    {move || {
                        ctx.session.with(|s| {
                            s.as_ref().and_then(|m| m.state.config.as_ref()).map(|c| {
                                view! {
                                    <span class="font-bold text-lg truncate block">
                                        {c.sport.to_string()}" "{c.team_size}"v"{c.team_size}
                                    </span>
                                }
                            })
                        })
                    }}
                </div>
                {move || {
                    ctx.session.with(|s| {
                        s.as_ref().map(|m| {
                            let r = m.state.current_round.0;
                            view! {
                                <span class="text-xs bg-gray-800 px-2 py-1 rounded-full \
                                            text-gray-300 font-medium shrink-0">
                                    "Rd "{r}
                                </span>
                            }
                        })
                    })
                }}
            </header>

            // ── Tab bar ───────────────────────────────────────────────────
            <nav class="flex overflow-x-auto bg-gray-900 border-b border-gray-700/50 shrink-0 whitespace-nowrap">
                {move || {
                    let sid = session_id();
                    let tab = active_tab();
                    [("matches","Matches"),("results","Results"),
                     ("analysis","Analysis"),("players","Players"),("online","Online")]
                        .iter()
                        .map(|(slug, label)| {
                            let href = format!("/coach/session/{sid}/{slug}");
                            let is_active = tab == *slug;
                            view! {
                                <A href=href attr:class=tab_class(is_active)>
                                    {*label}
                                </A>
                            }
                        })
                        .collect_view()
                }}
            </nav>

            // ── Active tab content ────────────────────────────────────────
            <main class="flex-1 overflow-y-auto">
                {move || match active_tab().as_str() {
                    "results"  => view! { <ResultsTab/> }.into_any(),
                    "analysis" => view! { <AnalysisTab/> }.into_any(),
                    "players"  => view! { <PlayersTab/> }.into_any(),
                    "online"   => view! { <OnlineTab/> }.into_any(),
                    _          => view! { <MatchesTab/> }.into_any(),
                }}
            </main>
        </div>
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// MATCHES TAB
// ══════════════════════════════════════════════════════════════════════════════

#[component]
pub fn MatchesTab() -> impl IntoView {
    let ctx = use_context::<AppContext>().expect("AppContext missing");

    // Signal to control which match has the swap UI open: Some(match_id)
    let swap_open: RwSignal<Option<MatchId>> = RwSignal::new(None);
    // Per-match swap selections: (match_id, old_player_id, new_player_id)
    let swap_old: RwSignal<Option<PlayerId>> = RwSignal::new(None);
    let swap_new: RwSignal<Option<PlayerId>> = RwSignal::new(None);
    let round_share_status_message: RwSignal<String> = RwSignal::new(String::new());
    let round_share_image_is_busy = RwSignal::new(false);

    let current_round_share_snapshot = {
        let ctx = ctx.clone();
        move || {
            ctx.session.with(|session| {
                let manager = session.as_ref()?;
                let config = manager.state.config.as_ref()?;
                let current_round_number = manager.state.current_round.0;
                let round_matches: Vec<_> = manager
                    .state
                    .matches
                    .values()
                    .filter(|scheduled_match| {
                        scheduled_match.round.0 == current_round_number
                            && scheduled_match.status != MatchStatus::Voided
                    })
                    .collect();
                if round_matches.is_empty() {
                    return None;
                }

                Some(build_round_schedule_share_snapshot(
                    format!("{} {}v{}", config.sport, config.team_size, config.team_size),
                    current_round_number,
                    &manager.state.players,
                    &round_matches,
                ))
            })
        }
    };

    let on_generate = {
        let ctx = ctx.clone();
        move |_| {
            ctx.session.update(|opt| {
                let manager = match opt {
                    Some(m) => m,
                    None => return,
                };
                let active_players: Vec<_> = manager.state.active_players().cloned().collect();
                if active_players.is_empty() {
                    return;
                }
                let config = match manager.state.config.clone() {
                    Some(c) => c,
                    None => return,
                };
                let rankings = manager.state.rankings.clone();
                let existing_matches: Vec<_> = manager.state.matches.values().collect();
                let starting_round = manager.state.current_round;
                let num_rounds = config.scheduling_frequency as u32;
                let scheduler = select_scheduler(&rankings);
                let matches = scheduler.generate_schedule(ScheduleGenerationRequest {
                    players: &active_players,
                    rankings: &rankings,
                    existing_matches: &existing_matches,
                    config: &config,
                    rng: &mut manager.rng,
                    starting_round,
                    num_rounds,
                });
                manager.log.append(
                    Event::ScheduleGenerated {
                        round: starting_round,
                        matches,
                    },
                    Role::Coach,
                );
                manager.state = materialize(&manager.log);
            });
            ctx.session.with(|s| {
                if let Some(m) = s {
                    save_session(m);
                }
            });
        }
    };

    let on_reseed = {
        let ctx = ctx.clone();
        move |_| {
            ctx.session.update(|opt| {
                if let Some(manager) = opt {
                    manager.reseed();
                }
            });
            ctx.session.with(|s| {
                if let Some(m) = s {
                    save_session(m);
                }
            });
        }
    };

    let on_void = {
        let ctx = ctx.clone();
        move |mid: MatchId| {
            if let Some(win) = web_sys::window() {
                let confirmed = win
                    .confirm_with_message("Void this match? It will be excluded from rankings.")
                    .unwrap_or(false);
                if !confirmed {
                    return;
                }
            }
            ctx.session.update(|opt| {
                if let Some(manager) = opt {
                    manager.void_match(mid);
                }
            });
            ctx.session.with(|s| {
                if let Some(m) = s {
                    save_session(m);
                }
            });
        }
    };

    let on_swap = {
        let ctx = ctx.clone();
        move |mid: MatchId| {
            let old_pid = match swap_old.get_untracked() {
                Some(p) => p,
                None => return,
            };
            let new_pid = match swap_new.get_untracked() {
                Some(p) => p,
                None => return,
            };
            if old_pid == new_pid {
                return;
            }
            ctx.session.update(|opt| {
                if let Some(manager) = opt {
                    manager.swap_player(mid, old_pid, new_pid);
                }
            });
            ctx.session.with(|s| {
                if let Some(m) = s {
                    save_session(m);
                }
            });
            swap_open.set(None);
            swap_old.set(None);
            swap_new.set(None);
        }
    };

    let on_copy_round_text = {
        move |_| {
            let Some(round_share_snapshot) = current_round_share_snapshot() else {
                round_share_status_message.set("Generate a round schedule first.".to_string());
                return;
            };
            round_share_status_message.set("Copying round text…".to_string());
            let share_text = format_round_schedule_share_text(&round_share_snapshot);
            leptos::task::spawn_local(async move {
                match copy_text_to_clipboard(&share_text).await {
                    Ok(()) => round_share_status_message.set("Round text copied.".to_string()),
                    Err(error_message) => round_share_status_message.set(error_message),
                }
            });
        }
    };

    let on_share_round_image = {
        move |_| {
            let Some(round_share_snapshot) = current_round_share_snapshot() else {
                round_share_status_message.set("Generate a round schedule first.".to_string());
                return;
            };
            round_share_image_is_busy.set(true);
            round_share_status_message.set("Preparing round image…".to_string());
            leptos::task::spawn_local(async move {
                let share_result = share_round_schedule_image(&round_share_snapshot).await;
                round_share_image_is_busy.set(false);
                match share_result {
                    Ok(RoundScheduleImageShareOutcome::SharedWithSystemSheet) => {
                        round_share_status_message.set("Round image shared.".to_string());
                    }
                    Ok(RoundScheduleImageShareOutcome::DownloadedPngFallback) => {
                        round_share_status_message.set(
                            "Round image downloaded. Native share is unavailable here.".to_string(),
                        );
                    }
                    Err(error_message) => round_share_status_message.set(error_message),
                }
            });
        }
    };

    view! {
        <div class="px-4 py-5 space-y-5">
            {move || {
                let session_opt = ctx.session.get();
                let manager = match session_opt.as_ref() {
                    Some(m) => m,
                    None => return view! { <LoadingOrMissing/> }.into_any(),
                };

                let round = manager.state.current_round.0;

                let mut round_matches: Vec<_> = manager.state.matches.values()
                    .filter(|m| m.round.0 == round && m.status != MatchStatus::Voided)
                    .collect();
                round_matches.sort_by_key(|m| m.field);

                if round_matches.is_empty() {
                    view! {
                        <div class="text-center py-12 space-y-3">
                            <p class="text-gray-400">
                                "No schedule for Round "{round}" yet."
                            </p>
                            <button
                                class="px-8 py-4 bg-blue-600 hover:bg-blue-500 \
                                       text-white font-semibold rounded-xl \
                                       transition-colors min-h-[52px]"
                                on:click=on_generate
                            >
                                "Generate Round "{round}" Schedule"
                            </button>
                            <div>
                                <button
                                    class="text-xs text-gray-500 hover:text-gray-300 \
                                           underline underline-offset-2 transition-colors"
                                    on:click=on_reseed
                                >
                                    "Re-seed RNG"
                                </button>
                            </div>
                        </div>
                    }.into_any()
                } else {
                    let player_map = manager.state.players.clone();
                    let all_players: Vec<_> = manager.state.players.values()
                        .filter(|p| p.status == app_core::models::PlayerStatus::Active)
                        .map(|p| (p.id, p.name.clone()))
                        .collect();

                    view! {
                        <div>
                            <div class="flex items-center justify-between mb-4">
                                <h2 class="font-bold text-lg">"Round "{round}</h2>
                                <div class="flex items-center gap-3">
                                    <button
                                        class="text-xs text-gray-500 hover:text-gray-300 \
                                               underline underline-offset-2 transition-colors"
                                        on:click=on_reseed
                                    >
                                        "Re-seed RNG"
                                    </button>
                                    <span class="text-sm text-gray-400">
                                        {round_matches.len()}" match"
                                        {if round_matches.len() == 1 { "" } else { "es" }}
                                    </span>
                                </div>
                            </div>
                            <div class="mb-4 rounded-xl border border-gray-700/50 bg-gray-900/70 p-4">
                                <div class="flex flex-col gap-3 md:flex-row md:items-center md:justify-between">
                                    <div>
                                        <p class="text-sm font-semibold text-white">
                                            "Share This Round"
                                        </p>
                                        <p class="mt-1 text-xs text-gray-400">
                                            "Copy a chat-friendly schedule or generate a shareable image without going online."
                                        </p>
                                    </div>
                                    <div class="flex flex-col gap-2 sm:flex-row">
                                        <button
                                            class="min-h-[44px] rounded-lg border border-gray-600 bg-gray-800 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-gray-700"
                                            on:click=on_copy_round_text
                                        >
                                            "Copy Round Text"
                                        </button>
                                        <button
                                            class="min-h-[44px] rounded-lg bg-teal-500 px-4 py-2 text-sm font-semibold text-slate-950 transition-colors hover:bg-teal-400 disabled:cursor-not-allowed disabled:opacity-60"
                                            on:click=on_share_round_image
                                            disabled=move || round_share_image_is_busy.get()
                                        >
                                            {move || if round_share_image_is_busy.get() {
                                                "Preparing Image…"
                                            } else {
                                                "Share Image"
                                            }}
                                        </button>
                                    </div>
                                </div>
                                {move || {
                                    let status_message = round_share_status_message.get();
                                    (!status_message.is_empty()).then(|| view! {
                                        <p class="mt-3 text-sm text-gray-300">{status_message}</p>
                                    })
                                }}
                            </div>
                            <div class="space-y-3">
                                {round_matches.into_iter().map(|m| {
                                    let mid = m.id;
                                    let team_a_names: Vec<_> = m.team_a.iter()
                                        .filter_map(|id| player_map.get(id).map(|p| p.name.clone()))
                                        .collect();
                                    let team_b_names: Vec<_> = m.team_b.iter()
                                        .filter_map(|id| player_map.get(id).map(|p| p.name.clone()))
                                        .collect();
                                    let all_match_ids: Vec<PlayerId> =
                                        m.team_a.iter().chain(m.team_b.iter()).cloned().collect();
                                    let field = m.field;
                                    let status = m.status.clone();
                                    let is_completed = status == MatchStatus::Completed;

                                    let all_players_for_swap = all_players.clone();
                                    view! {
                                        <div class="bg-gray-900 border border-gray-700/50 rounded-xl p-4">
                                            <div class="flex items-center justify-between mb-3">
                                                <span class="text-xs font-semibold uppercase \
                                                             tracking-widest text-gray-500">
                                                    "Field "{field}
                                                </span>
                                                {match status.clone() {
                                                    MatchStatus::Completed =>
                                                        view! { <span class="text-xs text-green-400 font-medium">"Done"</span> }.into_any(),
                                                    MatchStatus::InProgress =>
                                                        view! { <span class="text-xs text-yellow-400 font-medium">"In Progress"</span> }.into_any(),
                                                    _ =>
                                                        view! { <span class="text-xs text-gray-500">"Scheduled"</span> }.into_any(),
                                                }}
                                            </div>
                                            <div class="flex items-center gap-3 mb-3">
                                                <div class="flex-1 text-center">
                                                    {team_a_names.iter().map(|n| view! {
                                                        <p class="text-white font-medium text-sm">{n.clone()}</p>
                                                    }).collect_view()}
                                                </div>
                                                <span class="text-gray-500 font-bold shrink-0">"vs"</span>
                                                <div class="flex-1 text-center">
                                                    {team_b_names.iter().map(|n| view! {
                                                        <p class="text-white font-medium text-sm">{n.clone()}</p>
                                                    }).collect_view()}
                                                </div>
                                            </div>

                                            // Action buttons (only if not completed)
                                            {(!is_completed).then(|| {
                                                let on_void2 = on_void;
                                                let on_swap2 = on_swap;
                                                let all_players2 = all_players_for_swap.clone();
                                                let match_ids2 = all_match_ids.clone();
                                                view! {
                                                    <div>
                                                        <div class="flex gap-2 border-t border-gray-700/30 pt-3">
                                                            // Swap player toggle
                                                            <button
                                                                class=move || {
                                                                    let open = swap_open.get() == Some(mid);
                                                                    if open {
                                                                        "flex-1 py-2 text-xs font-medium rounded-lg \
                                                                         bg-gray-700 text-white transition-colors"
                                                                    } else {
                                                                        "flex-1 py-2 text-xs font-medium rounded-lg \
                                                                         bg-gray-800 text-gray-300 hover:bg-gray-700 \
                                                                         transition-colors"
                                                                    }
                                                                }
                                                                on:click=move |_| {
                                                                    let already = swap_open.get_untracked() == Some(mid);
                                                                    if already {
                                                                        swap_open.set(None);
                                                                        swap_old.set(None);
                                                                        swap_new.set(None);
                                                                    } else {
                                                                        swap_open.set(Some(mid));
                                                                        swap_old.set(None);
                                                                        swap_new.set(None);
                                                                    }
                                                                }
                                                            >
                                                                "Swap Player"
                                                            </button>
                                                            // Void match
                                                            <button
                                                                class="px-3 py-2 text-xs font-medium rounded-lg \
                                                                       bg-gray-800 text-red-400 hover:bg-red-950/40 \
                                                                       transition-colors"
                                                                on:click=move |_| on_void2(mid)
                                                            >
                                                                "Void"
                                                            </button>
                                                        </div>

                                                        // Inline swap form
                                                        {move || (swap_open.get() == Some(mid)).then(|| {
                                                            let match_ids3 = match_ids2.clone();
                                                            let all_players3 = all_players2.clone();
                                                            let on_swap3 = on_swap2;
                                                            view! {
                                                                <div class="mt-3 p-3 bg-gray-800 rounded-lg space-y-2">
                                                                    <p class="text-xs text-gray-400 mb-2">
                                                                        "Replace a player in this match:"
                                                                    </p>
                                                                    // Remove (old player)
                                                                    <div>
                                                                        <label class="text-xs text-gray-500">"Remove"</label>
                                                                        <select
                                                                            class="mt-1 w-full bg-gray-900 border border-gray-600 \
                                                                                   rounded-lg px-3 py-2 text-white text-sm \
                                                                                   min-h-[40px]"
                                                                            on:change=move |ev| {
                                                                                let v = event_target_value(&ev);
                                                                                if v.is_empty() {
                                                                                    swap_old.set(None);
                                                                                } else if let Ok(n) = v.parse::<u32>() {
                                                                                    swap_old.set(Some(PlayerId(n)));
                                                                                }
                                                                            }
                                                                        >
                                                                            <option value="">"— pick player —"</option>
                                                                            {match_ids3.iter().map(|&pid| {
                                                                                let name = all_players3.iter()
                                                                                    .find(|(id,_)| *id == pid)
                                                                                    .map(|(_,n)| n.clone())
                                                                                    .unwrap_or_else(|| format!("#{}", pid.0));
                                                                                let val = pid.0.to_string();
                                                                                view! {
                                                                                    <option value=val>{name}</option>
                                                                                }
                                                                            }).collect_view()}
                                                                        </select>
                                                                    </div>
                                                                    // Add (new player)
                                                                    <div>
                                                                        <label class="text-xs text-gray-500">"Substitute"</label>
                                                                        <select
                                                                            class="mt-1 w-full bg-gray-900 border border-gray-600 \
                                                                                   rounded-lg px-3 py-2 text-white text-sm \
                                                                                   min-h-[40px]"
                                                                            on:change=move |ev| {
                                                                                let v = event_target_value(&ev);
                                                                                if v.is_empty() {
                                                                                    swap_new.set(None);
                                                                                } else if let Ok(n) = v.parse::<u32>() {
                                                                                    swap_new.set(Some(PlayerId(n)));
                                                                                }
                                                                            }
                                                                        >
                                                                            <option value="">"— pick player —"</option>
                                                                            {all_players3.iter()
                                                                                .filter(|(pid,_)| !match_ids2.contains(pid))
                                                                                .map(|(pid, name)| {
                                                                                    let val = pid.0.to_string();
                                                                                    let name = name.clone();
                                                                                    view! {
                                                                                        <option value=val>{name}</option>
                                                                                    }
                                                                                }).collect_view()}
                                                                        </select>
                                                                    </div>
                                                                    <button
                                                                        class="w-full py-2 mt-1 bg-blue-600 hover:bg-blue-500 \
                                                                               text-white text-sm font-semibold rounded-lg \
                                                                               transition-colors disabled:opacity-40"
                                                                        on:click=move |_| on_swap3(mid)
                                                                    >
                                                                        "Confirm Swap"
                                                                    </button>
                                                                </div>
                                                            }
                                                        })}
                                                    </div>
                                                }
                                            })}
                                        </div>
                                    }
                                }).collect_view()}
                            </div>
                        </div>
                    }.into_any()
                }
            }}
        </div>
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// RESULTS TAB
// ══════════════════════════════════════════════════════════════════════════════

const DURATION_OPTIONS: &[(f64, &str)] = &[
    (0.5, "½ match"),
    (0.75, "¾ match"),
    (1.0, "Full"),
    (1.25, "1¼×"),
    (1.5, "1½×"),
];

#[component]
fn MatchScoreCard(
    match_id: MatchId,
    field: u8,
    round: u32,
    team_a: Vec<PlayerId>,
    team_b: Vec<PlayerId>,
    player_names: HashMap<PlayerId, String>,
    existing_result: Option<MatchResult>,
    on_submit: impl Fn(MatchResult) + 'static + Clone,
) -> impl IntoView {
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

    let init_multiplier = existing_result
        .as_ref()
        .map(|r| r.duration_multiplier)
        .unwrap_or(1.0);

    let draft = RwSignal::new(init);
    let duration_mult = RwSignal::new(init_multiplier);
    let is_saved = RwSignal::new(existing_result.is_some());

    view! {
        <div class="bg-gray-900 border border-gray-700/50 rounded-xl overflow-hidden">
            // Header
            <div class="px-4 pt-4 pb-3 border-b border-gray-700/30 flex items-center justify-between">
                <span class="text-sm font-semibold text-white">
                    "Field "{field}" · Rd "{round}
                </span>
                {move || is_saved.get().then(|| view! {
                    <span class="text-xs text-green-400 font-medium">"Saved ✓"</span>
                })}
            </div>

            // Player score rows
            <div class="px-4 py-3 space-y-3">
                {all_ids.iter().map(|&pid| {
                    let name = player_names.get(&pid).cloned()
                        .unwrap_or_else(|| format!("Player {}", pid.0));
                    let on_team_a = team_a.contains(&pid);
                    view! {
                        <div class="flex items-center gap-2 flex-wrap">
                            <span class=move || format!(
                                "w-2 h-2 rounded-full shrink-0 {}",
                                if on_team_a { "bg-blue-400" } else { "bg-orange-400" }
                            )/>
                            <span class="flex-1 min-w-[80px] text-sm text-white truncate">{name}</span>
                            <div class="flex gap-1 flex-wrap">
                                <button
                                    class=move || {
                                        let active = draft.with(|d| d.get(&pid).copied().flatten().is_none());
                                        format!(
                                            "px-2 py-1 rounded min-h-[36px] min-w-[40px] text-xs {}",
                                            if active { "bg-gray-600 text-white font-semibold" }
                                            else { "bg-gray-800 text-gray-400 hover:bg-gray-700" }
                                        )
                                    }
                                    on:click=move |_| { draft.update(|d| { d.insert(pid, None); }); }
                                >
                                    "DNP"
                                </button>
                                {(0u16..=9).map(|n| {
                                    view! {
                                        <button
                                            class=move || {
                                                let active = draft.with(|d| *d.get(&pid).unwrap_or(&None) == Some(n));
                                                format!(
                                                    "px-2 py-1 rounded min-h-[36px] min-w-[32px] \
                                                     font-semibold text-sm {}",
                                                    if active { "bg-blue-600 text-white" }
                                                    else { "bg-gray-800 text-gray-400 hover:bg-gray-700" }
                                                )
                                            }
                                            on:click=move |_| {
                                                draft.update(|d| { d.insert(pid, Some(n)); });
                                            }
                                        >
                                            {n}
                                        </button>
                                    }
                                }).collect_view()}
                            </div>
                        </div>
                    }
                }).collect_view()}
            </div>

            // Duration multiplier
            <div class="px-4 pb-3 border-t border-gray-700/20 pt-3">
                <p class="text-xs text-gray-500 mb-2">"Match duration"</p>
                <div class="flex gap-1 flex-wrap">
                    {DURATION_OPTIONS.iter().map(|&(val, label)| {
                        view! {
                            <button
                                class=move || {
                                    let active = (duration_mult.get() - val).abs() < 0.01;
                                    format!(
                                        "px-3 py-1 rounded text-xs font-medium min-h-[32px] \
                                         transition-colors {}",
                                        if active { "bg-blue-600 text-white" }
                                        else { "bg-gray-800 text-gray-400 hover:bg-gray-700" }
                                    )
                                }
                                on:click=move |_| duration_mult.set(val)
                            >
                                {label}
                            </button>
                        }
                    }).collect_view()}
                </div>
            </div>

            // Save button
            <div class="px-4 pb-4">
                <button
                    class="w-full py-3 bg-blue-600 hover:bg-blue-500 active:bg-blue-700 \
                           text-white font-semibold rounded-lg transition-colors min-h-[48px]"
                    on:click={
                        let on_submit = on_submit;
                        let all_ids = all_ids.clone();
                        move |_| {
                            let scores: HashMap<_, _> = draft.with(|d| {
                                all_ids.iter().map(|id| {
                                    (*id, PlayerMatchScore { goals: *d.get(id).unwrap_or(&None) })
                                }).collect()
                            });
                            let mult = duration_mult.get_untracked();
                            on_submit(MatchResult {
                                match_id,
                                scores,
                                duration_multiplier: mult,
                                entered_by: Role::Coach,
                            });
                            is_saved.set(true);
                        }
                    }
                >
                    "Save Scores"
                </button>
            </div>
        </div>
    }
}

#[component]
pub fn ResultsTab() -> impl IntoView {
    let ctx = use_context::<AppContext>().expect("AppContext missing");

    let on_score_submit = {
        let ctx = ctx.clone();
        move |result: MatchResult| {
            ctx.session.update(|opt| {
                if let Some(manager) = opt {
                    manager.enter_score(result, Role::Coach);
                }
            });
            ctx.session.with(|s| {
                if let Some(m) = s {
                    save_session(m);
                }
            });
        }
    };

    // Handler for the "Download All Results CSV" button.
    let on_download_results = {
        let ctx = ctx.clone();
        move |_| {
            ctx.session.with(|s| {
                let manager = match s {
                    Some(m) => m,
                    None => return,
                };
                let config = match manager.state.config.as_ref() {
                    Some(c) => c,
                    None => return,
                };
                let players: Vec<_> = manager.state.players.values().cloned().collect();
                let results: Vec<_> = manager
                    .state
                    .results
                    .values()
                    .filter(|r| {
                        manager
                            .state
                            .matches
                            .get(&r.match_id)
                            .map(|m| m.status == MatchStatus::Completed)
                            .unwrap_or(false)
                    })
                    .collect();
                if results.is_empty() {
                    return;
                }
                let csv_str =
                    csv::export_results(&results, &players, &manager.state.matches, config);
                trigger_csv_download(&csv_str, "results.csv");
            });
        }
    };

    view! {
        <div class="px-4 py-5 space-y-4">
            {move || {
                let session_opt = ctx.session.get();
                let manager = match session_opt.as_ref() {
                    Some(m) => m,
                    None => return view! { <LoadingOrMissing/> }.into_any(),
                };

                let round = manager.state.current_round.0;
                let player_names: HashMap<_, _> = manager.state.players.iter()
                    .map(|(id, p)| (*id, p.name.clone()))
                    .collect();

                let mut matches: Vec<_> = manager.state.matches.values()
                    .filter(|m| m.round.0 == round && m.status != MatchStatus::Voided)
                    .collect();
                matches.sort_by_key(|m| m.field);

                if matches.is_empty() {
                    view! {
                        <div class="text-center py-12 text-gray-400">
                            <p>"No matches scheduled for Round "{round}"."</p>
                            <p class="text-sm mt-1">
                                "Go to the Matches tab to generate a schedule first."
                            </p>
                        </div>
                    }.into_any()
                } else {
                    let on_submit = on_score_submit;
                    view! {
                        <div class="space-y-4">
                            <h2 class="font-bold text-lg">"Round "{round}" — Score Entry"</h2>
                            {matches.into_iter().map(|m| {
                                let mid     = m.id;
                                let field   = m.field;
                                let rnd     = m.round.0;
                                let team_a  = m.team_a.clone();
                                let team_b  = m.team_b.clone();
                                let existing = manager.state.results.get(&mid).cloned();
                                let pnames  = player_names.clone();
                                let submit  = on_submit;
                                view! {
                                    <MatchScoreCard
                                        match_id=mid
                                        field=field
                                        round=rnd
                                        team_a=team_a
                                        team_b=team_b
                                        player_names=pnames
                                        existing_result=existing
                                        on_submit=submit
                                    />
                                }
                            }).collect_view()}
                        </div>
                    }.into_any()
                }
            }}

            // Download all completed results as CSV (visible once any results exist).
            {move || {
                let has_results = ctx.session.with(|s| {
                    s.as_ref().map(|m| {
                        m.state.results.values().any(|r| {
                            m.state.matches.get(&r.match_id)
                                .map(|sm| sm.status == MatchStatus::Completed)
                                .unwrap_or(false)
                        })
                    }).unwrap_or(false)
                });
                has_results.then(|| view! {
                    <div class="pt-2 border-t border-gray-700/40">
                        <button
                            class="w-full py-2 text-sm text-gray-500 hover:text-gray-300 \
                                   flex items-center justify-center gap-1.5 transition-colors"
                            on:click=on_download_results
                        >
                            "↓ Download All Results CSV"
                        </button>
                    </div>
                })
            }}
        </div>
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// ANALYSIS TAB  (Overall | A/D/T | Synergy)
// ══════════════════════════════════════════════════════════════════════════════

use app_core::ranking::synergy::SynergyEngine;
use app_core::ranking::trivariate::TrivariateEngine;

/// Trigger a browser file download of `content` as a UTF-8 text file named `filename`.
fn trigger_csv_download(content: &str, filename: &str) {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return,
    };
    let document = match window.document() {
        Some(d) => d,
        None => return,
    };

    // Build a Blob from the CSV string
    let parts = js_sys::Array::new();
    parts.push(&wasm_bindgen::JsValue::from_str(content));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("text/csv;charset=utf-8");
    let blob = match web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts) {
        Ok(b) => b,
        Err(_) => return,
    };
    let url = match web_sys::Url::create_object_url_with_blob(&blob) {
        Ok(u) => u,
        Err(_) => return,
    };

    // Create a hidden <a> element and programmatically click it
    if let Ok(el) = document.create_element("a") {
        let anchor: web_sys::HtmlAnchorElement = wasm_bindgen::JsCast::unchecked_into(el);
        anchor.set_href(&url);
        anchor.set_download(filename);
        anchor.set_attribute("style", "display:none").ok();
        document.body().map(|b| b.append_child(&anchor).ok());
        anchor.click();
        document.body().map(|b| b.remove_child(&anchor).ok());
        web_sys::Url::revoke_object_url(&url).ok();
    }
}

fn analysis_sub_tab_class(active: bool) -> &'static str {
    if active {
        "px-3 py-1.5 text-sm font-semibold text-white bg-gray-800 \
         rounded-lg border border-gray-600"
    } else {
        "px-3 py-1.5 text-sm font-medium text-gray-400 hover:text-gray-200 \
         rounded-lg border border-transparent"
    }
}

#[component]
pub fn AnalysisTab() -> impl IntoView {
    let ctx = use_context::<AppContext>().expect("AppContext missing");
    // "overall" | "adt" | "synergy"
    let sub_tab: RwSignal<&'static str> = RwSignal::new("overall");

    let on_compute = {
        let ctx = ctx.clone();
        move |_| {
            ctx.session.update(|opt| {
                let manager = match opt {
                    Some(m) => m,
                    None => return,
                };
                let config = match manager.state.config.clone() {
                    Some(c) => c,
                    None => return,
                };
                let players: Vec<_> = manager.state.players.values().cloned().collect();
                if players.is_empty() {
                    return;
                }
                let results: Vec<_> = manager
                    .state
                    .results
                    .values()
                    .filter(|r| {
                        manager
                            .state
                            .matches
                            .get(&r.match_id)
                            .map(|m| m.status == MatchStatus::Completed)
                            .unwrap_or(false)
                    })
                    .collect();
                if results.is_empty() {
                    return;
                }
                let engine = GoalModelEngine::default();
                let rankings = engine.compute_ratings(
                    &players,
                    &results,
                    Some(&manager.state.matches),
                    &config,
                );
                let round = manager.state.current_round;
                manager
                    .log
                    .append(Event::RankingsComputed { round, rankings }, Role::Coach);
                manager.state = materialize(&manager.log);
            });
            ctx.session.with(|s| {
                if let Some(m) = s {
                    save_session(m);
                }
            });
        }
    };

    // CSV import state for "load from CSV" analysis mode
    let show_csv_import = RwSignal::new(false);
    let csv_import_text = RwSignal::new(String::new());
    let csv_import_error = RwSignal::new(String::new());
    let csv_rankings: RwSignal<Option<Vec<(String, app_core::models::PlayerRanking)>>> =
        RwSignal::new(None);

    view! {
        <div class="px-4 py-5 space-y-4">
                    // Sub-tab bar
            <div class="flex items-center gap-2">
                <button class=move || analysis_sub_tab_class(sub_tab.get() == "overall")
                    on:click=move |_| sub_tab.set("overall")>"Overall"</button>
                <button class=move || analysis_sub_tab_class(sub_tab.get() == "adt")
                    on:click=move |_| sub_tab.set("adt")>"A/D/T"</button>
                <button class=move || analysis_sub_tab_class(sub_tab.get() == "synergy")
                    on:click=move |_| sub_tab.set("synergy")>"Synergy"</button>
            </div>

            {move || {
                let session_opt = ctx.session.get();
                let manager = match session_opt.as_ref() {
                    Some(m) => m,
                    None => return view! { <LoadingOrMissing/> }.into_any(),
                };

                let rankings = manager.state.rankings.clone();
                let player_map = manager.state.players.clone();
                let results_count = manager.state.results.len();
                let config = manager.state.config.clone();
                let tab = sub_tab.get();

                match tab {
                    "adt" => {
                        // Attack / Defense / Teamwork
                        let players: Vec<_> = manager.state.players.values().cloned().collect();
                        let completed_results: Vec<_> = manager.state.results.values()
                            .filter(|r| manager.state.matches.get(&r.match_id)
                                .map(|m| m.status == MatchStatus::Completed)
                                .unwrap_or(false))
                            .collect();
                        let completed_matches: Vec<_> = manager.state.matches.values()
                            .filter(|m| m.status == MatchStatus::Completed)
                            .collect();
                        let team_size = config.as_ref().map(|c| c.team_size).unwrap_or(2);
                        let engine = TrivariateEngine::default();
                        let trivariate = engine.compute(&players, &completed_results, &completed_matches, team_size);
                        let has_teamwork = team_size >= 2;

                        view! {
                            <div class="space-y-3">
                                <p class="text-xs text-gray-500">
                                    "Attack · Defense · Teamwork decomposition. "
                                    "Requires ≥3 matches per player."
                                </p>
                                {match trivariate {
                                    None => view! {
                                        <div class="text-center py-8 text-gray-400">
                                            <p class="font-medium">"Not enough data yet"</p>
                                            <p class="text-sm mt-1">
                                                "Each player needs at least 3 completed matches."
                                            </p>
                                        </div>
                                    }.into_any(),
                                    Some(mut ratings) => {
                                        // Sort by attack rating descending
                                        ratings.sort_by(|a, b| b.attack.rating.partial_cmp(&a.attack.rating)
                                            .unwrap_or(std::cmp::Ordering::Equal));
                                        view! {
                                            <div class="content-auto-table overflow-x-auto -mx-4 px-4">
                                                <table class="w-full text-sm min-w-[360px]">
                                                    <thead>
                                                        <tr class="text-gray-500 text-xs uppercase \
                                                                   tracking-wide border-b border-gray-700/50">
                                                            <th class="text-left py-2 pr-3">"Player"</th>
                                                            <th class="text-right py-2 pr-3">"Attack"</th>
                                                            <th class="text-right py-2 pr-3">"Defense"</th>
                                                            {has_teamwork.then(|| view! {
                                                                <th class="text-right py-2">"Teamwork"</th>
                                                            })}
                                                        </tr>
                                                    </thead>
                                                    <tbody>
                                                        {ratings.into_iter().map(|r| {
                                                            let name = player_map.get(&r.player_id)
                                                                .map(|p| p.name.clone())
                                                                .unwrap_or_else(|| r.player_id.to_string());
                                                            let atk = r.attack.rating;
                                                            let def = r.defense.rating;
                                                            let tmw = r.teamwork.as_ref().map(|t| t.rating);
                                                            let is_active = r.is_active;
                                                            view! {
                                                                <tr class="border-b border-gray-800/50 hover:bg-gray-900/50">
                                                                    <td class="py-3 pr-3">
                                                                        <span class=move || format!("font-medium {}",
                                                                            if is_active { "text-white" } else { "text-gray-500 line-through" })>
                                                                            {name}
                                                                        </span>
                                                                    </td>
                                                                    <td class=move || format!("py-3 pr-3 text-right tabular-nums font-medium {}",
                                                                        sub_rating_color(atk))>
                                                                        {format!("{:+.2}", atk)}
                                                                    </td>
                                                                    <td class=move || format!("py-3 pr-3 text-right tabular-nums font-medium {}",
                                                                        sub_rating_color(def))>
                                                                        {format!("{:+.2}", def)}
                                                                    </td>
                                                                    {has_teamwork.then(|| view! {
                                                                        <td class=move || format!("py-3 text-right tabular-nums font-medium {}",
                                                                            tmw.map(sub_rating_color).unwrap_or("text-gray-500"))>
                                                                            {tmw.map(|v| format!("{:+.2}", v)).unwrap_or_else(|| "—".into())}
                                                                        </td>
                                                                    })}
                                                                </tr>
                                                            }
                                                        }).collect_view()}
                                                    </tbody>
                                                </table>
                                            </div>
                                        }.into_any()
                                    }
                                }}
                            </div>
                        }.into_any()
                    }

                    "synergy" => {
                        let players: Vec<_> = manager.state.players.values().cloned().collect();
                        let completed_matches: Vec<_> = manager.state.matches.values()
                            .filter(|m| m.status == MatchStatus::Completed)
                            .collect();
                        let completed_results: Vec<_> = manager.state.results.values()
                            .filter(|r| manager.state.matches.get(&r.match_id)
                                .map(|m| m.status == MatchStatus::Completed)
                                .unwrap_or(false))
                            .collect();
                        let engine = SynergyEngine::default();
                        let synergy = engine.compute(&players, &completed_matches, &completed_results);

                        view! {
                            <div class="space-y-3">
                                <p class="text-xs text-gray-500">
                                    "Pair synergy via RAPM. Requires ≥30% of player pairs observed."
                                </p>
                                {match synergy {
                                    None => view! {
                                        <div class="text-center py-8 text-gray-400">
                                            <p class="font-medium">"Not enough data yet"</p>
                                            <p class="text-sm mt-1">
                                                "Need more matches covering a wider variety of player pairings."
                                            </p>
                                        </div>
                                    }.into_any(),
                                    Some(mat) => {
                                        let n = mat.players.len();
                                        // Player names in matrix order
                                        let names: Vec<String> = mat.players.iter()
                                            .map(|id| player_map.get(id)
                                                .map(|p| p.name.chars().take(10).collect())
                                                .unwrap_or_else(|| format!("#{}", id.0)))
                                            .collect();

                                        view! {
                                            <div class="space-y-4">
                                                // Individual APM row
                                                <div>
                                                    <p class="text-xs text-gray-500 mb-2">
                                                        "Individual APM (Adjusted Plus-Minus)"
                                                    </p>
                                                    <div class="space-y-1">
                                                        {mat.players.iter().enumerate()
                                                            .map(|(i, _)| {
                                                                let name = names[i].clone();
                                                                let apm = mat.individual_apm[i];
                                                                view! {
                                                                    <div class="flex items-center gap-3">
                                                                        <span class="text-sm text-gray-300 w-24 truncate">{name}</span>
                                                                        <div class="flex-1 h-2 bg-gray-800 rounded-full overflow-hidden">
                                                                            <div
                                                                                style=format!("width:{}%; margin-left:{}%",
                                                                                    if apm > 0.0 { apm.min(3.0) / 3.0 * 50.0 } else { 0.0 },
                                                                                    if apm > 0.0 { 50.0 } else { (50.0 + apm.max(-3.0) / 3.0 * 50.0).max(0.0) })
                                                                                class=move || format!("h-full rounded-full {}",
                                                                                    if apm >= 0.0 { "bg-blue-500" } else { "bg-red-500" })
                                                                            />
                                                                        </div>
                                                                        <span class=move || format!("text-sm tabular-nums font-medium w-12 text-right {}",
                                                                            if apm > 0.1 { "text-blue-400" }
                                                                            else if apm < -0.1 { "text-red-400" }
                                                                            else { "text-gray-400" })>
                                                                            {format!("{:+.2}", apm)}
                                                                        </span>
                                                                    </div>
                                                                }
                                                            }).collect_view()}
                                                    </div>
                                                </div>

                                                // Synergy heatmap (SVG)
                                                {(n >= 3).then(|| {
                                                    let names2 = names.clone();
                                                    let matrix = mat.matrix.clone();
                                                    view! {
                                                        <div>
                                                            <p class="text-xs text-gray-500 mb-2">
                                                                "Pair synergy (blue = better together, red = worse)"
                                                            </p>
                                                            <SynergyHeatmap
                                                                names=names2
                                                                matrix=matrix
                                                            />
                                                        </div>
                                                    }
                                                })}
                                            </div>
                                        }.into_any()
                                    }
                                }}
                            </div>
                        }.into_any()
                    }

                    _ => {
                        // "overall" tab (default)
                        let confidence_est = if !rankings.is_empty() {
                            let engine = GoalModelEngine::default();
                            let config = config.unwrap().clone();
                            Some(engine.estimated_rounds_to_confidence(&rankings, 3, &config))
                        } else { None };

                        // Prepare CSV export data while we still have rankings + player_map
                        let export_players: Vec<_> = player_map.values().cloned().collect();
                        let export_rankings = rankings.clone();

                        view! {
                            <div class="space-y-4">
                                <div class="flex items-center justify-between">
                                    <h2 class="font-bold text-lg">"Rankings"</h2>
                                    {(results_count > 0).then(|| view! {
                                        <button
                                            class="px-4 py-2 bg-blue-600 hover:bg-blue-500 \
                                                   text-white font-medium text-sm rounded-lg \
                                                   min-h-[44px] transition-colors"
                                            on:click=on_compute
                                        >"Update Rankings"</button>
                                    })}
                                </div>

                                {confidence_est.map(|r| view! {
                                    <p class="text-sm text-gray-400 bg-gray-900 rounded-lg px-3 py-2 \
                                              border border-gray-700/50">
                                        "Estimated "
                                        <span class="text-white font-semibold">{r}" more rounds"</span>
                                        " for confident rankings (±1 rank, 90%)"
                                    </p>
                                })}

                                {if rankings.is_empty() {
                                    view! {
                                        <div class="text-center py-12 text-gray-400">
                                            <p>"No rankings yet."</p>
                                            <p class="text-sm mt-1">
                                                "Save scores in Results, then tap "
                                                <span class="text-white">"Update Rankings"</span>"."
                                            </p>
                                        </div>
                                    }.into_any()
                                } else {
                                    let mut sorted = rankings.clone();
                                    sorted.sort_by_key(|r| r.rank);
                                    view! {
                                        <div class="space-y-4">
                                            <RankLane rankings=sorted.clone() player_map=player_map.clone()/>
                                            <OverallTable rankings=sorted player_map=player_map/>
                                            // CSV export
                                            <button
                                                class="flex items-center gap-2 px-3 py-2 text-xs \
                                                       text-gray-400 hover:text-white \
                                                       bg-gray-900 hover:bg-gray-800 \
                                                       border border-gray-700/50 rounded-lg \
                                                       transition-colors min-h-[36px]"
                                                on:click=move |_| {
                                                    let content = csv::export_rankings(
                                                        &export_rankings, &export_players,
                                                    );
                                                    trigger_csv_download(&content, "rankings.csv");
                                                }
                                            >
                                                "↓ Download Rankings CSV"
                                            </button>
                                        </div>
                                    }.into_any()
                                }}
                                // CSV import accordion — always available regardless of session state
                                <div>
                                    <button
                                        class="text-xs text-gray-500 hover:text-gray-300 \
                                               underline underline-offset-2 transition-colors"
                                        on:click=move |_| {
                                            show_csv_import.update(|v| *v = !*v);
                                            csv_import_error.set(String::new());
                                        }
                                    >
                                        {move || if show_csv_import.get() {
                                            "▲ Hide CSV import"
                                        } else {
                                            "▼ Load rankings from CSV"
                                        }}
                                    </button>
                                    {move || show_csv_import.get().then(|| view! {
                                        <div class="mt-2 space-y-2">
                                            <p class="text-xs text-gray-500">
                                                "Paste a rankings CSV exported from this app \
                                                 to view it without an active session."
                                            </p>
                                            {move || {
                                                let err = csv_import_error.get();
                                                (!err.is_empty()).then(|| view! {
                                                    <p class="text-xs text-red-400">{err}</p>
                                                })
                                            }}
                                            <textarea
                                                rows="6"
                                                placeholder="rank,name,rating,..."
                                                class="w-full bg-gray-900 border border-gray-700 \
                                                       rounded-lg px-3 py-2 text-white text-xs \
                                                       font-mono placeholder-gray-600 \
                                                       focus:outline-none focus:border-blue-500 \
                                                       resize-none"
                                                prop:value=move || csv_import_text.get()
                                                on:input=move |ev| csv_import_text.set(event_target_value(&ev))
                                            />
                                            <div class="flex gap-2">
                                                <button
                                                    class="flex-1 py-2 bg-gray-800 hover:bg-gray-700 \
                                                           text-white text-sm font-medium rounded-lg \
                                                           transition-colors min-h-[40px] \
                                                           border border-gray-600"
                                                    on:click=move |_| {
                                                        let raw = csv_import_text.get_untracked();
                                                        match import_rankings(&raw) {
                                                            Ok(rows) => {
                                                                csv_rankings.set(Some(rows));
                                                                csv_import_error.set(String::new());
                                                                show_csv_import.set(false);
                                                            }
                                                            Err(e) => {
                                                                csv_import_error.set(e.to_string());
                                                            }
                                                        }
                                                    }
                                                >
                                                    "Load"
                                                </button>
                                                {move || csv_rankings.get().is_some().then(|| view! {
                                                    <button
                                                        class="px-3 py-2 bg-gray-800 hover:bg-gray-700 \
                                                               text-gray-400 text-xs rounded-lg \
                                                               border border-gray-600 min-h-[40px]"
                                                        on:click=move |_| {
                                                            csv_rankings.set(None);
                                                            csv_import_text.set(String::new());
                                                        }
                                                    >
                                                        "Clear"
                                                    </button>
                                                })}
                                            </div>
                                        </div>
                                    })}
                                    // Display CSV-loaded rankings if present
                                    {move || {
                                        let loaded = csv_rankings.get();
                                        loaded.map(|rows| {
                                            let n = rows.len();
                                            let csv_player_map: HashMap<app_core::models::PlayerId, app_core::models::Player> =
                                                rows.iter().map(|(name, r)| {
                                                    let p = app_core::models::Player {
                                                        id: r.player_id,
                                                        name: name.clone(),
                                                        status: if r.is_active {
                                                            app_core::models::PlayerStatus::Active
                                                        } else {
                                                            app_core::models::PlayerStatus::Inactive
                                                        },
                                                        joined_at_round: app_core::models::RoundNumber(1),
                                                        deactivated_at_round: None,
                                                    };
                                                    (r.player_id, p)
                                                }).collect();
                                            let csv_sorted: Vec<_> = rows.into_iter()
                                                .map(|(_, r)| r)
                                                .collect();
                                            view! {
                                                <div class="mt-3 space-y-3">
                                                    <p class="text-xs text-blue-400 font-medium">
                                                        "Showing CSV-loaded rankings ("
                                                        {n}" players)"
                                                    </p>
                                                    <RankLane
                                                        rankings=csv_sorted.clone()
                                                        player_map=csv_player_map.clone()
                                                    />
                                                    <OverallTable
                                                        rankings=csv_sorted
                                                        player_map=csv_player_map
                                                    />
                                                </div>
                                            }
                                        })
                                    }}
                                </div>
                            </div>
                        }.into_any()
                    }
                }
            }}
        </div>
    }
}

fn sub_rating_color(v: f64) -> &'static str {
    if v > 0.2 {
        "text-green-400"
    } else if v < -0.2 {
        "text-red-400"
    } else {
        "text-gray-400"
    }
}

// ── Overall stats table ──────────────────────────────────────────────────────

#[component]
fn OverallTable(
    rankings: Vec<app_core::models::PlayerRanking>,
    player_map: HashMap<PlayerId, app_core::models::Player>,
) -> impl IntoView {
    view! {
        <div class="content-auto-table overflow-x-auto -mx-4 px-4">
            <table class="w-full text-sm min-w-[380px]">
                <thead>
                    <tr class="text-gray-500 text-xs uppercase tracking-wide border-b border-gray-700/50">
                        <th class="text-left py-2 pr-2 w-8">"#"</th>
                        <th class="text-left py-2 pr-2">"Player"</th>
                        <th class="text-right py-2 pr-2">"Rating"</th>
                        <th class="text-right py-2 pr-2 hidden sm:table-cell">"±"</th>
                        <th class="text-right py-2 pr-2">"Range"</th>
                        <th class="text-right py-2 pr-2 hidden sm:table-cell">"M"</th>
                        <th class="text-right py-2">"G"</th>
                    </tr>
                </thead>
                <tbody>
                    {rankings.into_iter().map(|r| {
                        let name = player_map.get(&r.player_id)
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| r.player_id.to_string());
                        let deact_round = player_map.get(&r.player_id)
                            .and_then(|p| p.deactivated_at_round)
                            .map(|rn| rn.0);
                        let rank        = r.rank;
                        let rating      = format!("{:.2}", r.rating);
                        let uncertainty = format!("{:.2}", r.uncertainty);
                        let rank_lo     = r.rank_range_90.0;
                        let rank_hi     = r.rank_range_90.1;
                        let played      = r.matches_played;
                        let goals       = r.total_goals;
                        let is_active   = r.is_active;
                        view! {
                            <tr class="border-b border-gray-800/50 hover:bg-gray-900/50">
                                <td class=move || format!(
                                    "py-3 pr-2 font-bold tabular-nums {}",
                                    if rank <= 3 { "text-yellow-400" } else { "text-gray-400" })>
                                    {rank}
                                </td>
                                <td class="py-3 pr-2">
                                    <span class=move || format!("font-medium {}",
                                        if !is_active { "text-gray-500 line-through" } else { "text-white" })>
                                        {name}
                                    </span>
                                    {(!is_active).then(move || view! {
                                        <span class="ml-1 text-xs text-gray-500">
                                            {match deact_round {
                                                Some(n) => format!("(as of Rd {n})"),
                                                None    => "(inactive)".to_string(),
                                            }}
                                        </span>
                                    })}
                                </td>
                                <td class="py-3 pr-2 text-right text-white tabular-nums">{rating}</td>
                                <td class="py-3 pr-2 text-right text-gray-500 tabular-nums hidden sm:table-cell">{uncertainty}</td>
                                <td class="py-3 pr-2 text-right text-gray-400 text-xs tabular-nums">
                                    {rank_lo}"–"{rank_hi}
                                </td>
                                <td class="py-3 pr-2 text-right text-gray-400 hidden sm:table-cell tabular-nums">{played}</td>
                                <td class="py-3 text-right text-gray-400 tabular-nums">{goals}</td>
                            </tr>
                        }
                    }).collect_view()}
                </tbody>
            </table>
        </div>
    }
}

// ── Synergy heatmap SVG ──────────────────────────────────────────────────────

#[component]
fn SynergyHeatmap(names: Vec<String>, matrix: Vec<Vec<f64>>) -> impl IntoView {
    let n = names.len();
    if n == 0 {
        return view! { <div/> }.into_any();
    }

    let cell = 28i32;
    let label_w = 70i32;
    let label_h = 64i32; // rotated top labels need extra clearance from the first data row
    let svg_w = label_w + n as i32 * cell;
    let svg_h = label_h + n as i32 * cell;

    // Determine color scale: map [-max_abs, max_abs] to blue/white/red
    let max_abs: f64 = matrix
        .iter()
        .enumerate()
        .flat_map(|(i, row)| {
            row.iter()
                .enumerate()
                .filter(move |&(j, _)| i != j)
                .map(|(_, &v)| v.abs())
        })
        .fold(0.0f64, f64::max)
        .max(0.01);

    let color = |v: f64| -> String {
        // v in [-max_abs, max_abs] → blue=positive, red=negative, white=zero
        let t = (v / max_abs).clamp(-1.0, 1.0);
        if t >= 0.0 {
            let b = (t * 180.0) as u8;
            format!("rgb({}, {}, 255)", 255 - b, 255 - b)
        } else {
            let r = ((-t) * 180.0) as u8;
            format!("rgb(255, {}, {})", 255 - r, 255 - r)
        }
    };

    view! {
        <div class="overflow-x-auto -mx-4 px-4">
            <svg
                viewBox=format!("0 0 {svg_w} {svg_h}")
                style=format!("width:100%; max-width:{}px; height:auto; display:block;",
                    svg_w.max(200))
            >
                // Row labels (left) + cells
                {(0..n).map(|i| {
                    let y_top = label_h + i as i32 * cell;
                    let cy = y_top + cell / 2;
                    let row_name: String = names[i].chars().take(10).collect();
                    view! {
                        <g>
                            <text x=label_w-3 y=cy+4 font-size="9"
                                fill="#d1d5db" text-anchor="end">
                                {row_name}
                            </text>
                            {(0..n).map(|j| {
                                let x_left = label_w + j as i32 * cell;
                                let val = matrix[i][j];
                                let bg = color(if i == j { 0.0 } else { val });
                                let text_color = if val.abs() > max_abs * 0.6 { "#ffffff" }
                                    else { "#1f2937" };
                                let label = if i == j { "—".to_string() }
                                    else { format!("{:+.1}", val) };
                                view! {
                                    <g>
                                        <rect x=x_left y=y_top width=cell height=cell
                                            fill=bg stroke="#030712" stroke-width="1"/>
                                        <text
                                            x=x_left + cell/2
                                            y=cy + 3
                                            font-size="7"
                                            fill=text_color
                                            text-anchor="middle"
                                        >{label}</text>
                                    </g>
                                }
                            }).collect_view()}
                        </g>
                    }
                }).collect_view()}

                // Render column labels after the cells so long names are not hidden by the matrix.
                {(0..n).map(|j| {
                    let x = label_w + j as i32 * cell + cell / 2;
                    let name: String = names[j].chars().take(8).collect();
                    view! {
                        <text
                            x=x
                            y=label_h - 8
                            font-size="8"
                            fill="#9ca3af"
                            text-anchor="end"
                            transform=format!("rotate(-45,{x},{})", label_h - 8)
                        >{name}</text>
                    }
                }).collect_view()}
            </svg>
        </div>
    }
    .into_any()
}

// ── Rank-lane SVG visualization ───────────────────────────────────────────────
//
// Each player is a horizontal lane. The 90% rank interval is a rounded bar;
// the median rank is a circle. Lanes are sorted by median rank (best at top).

#[component]
fn RankLane(
    rankings: Vec<app_core::models::PlayerRanking>,
    player_map: HashMap<PlayerId, app_core::models::Player>,
) -> impl IntoView {
    let n = rankings.len();

    if n < 2 {
        return view! { <div/> }.into_any();
    }

    let row_h = 28i32;
    let label_w = 90i32;
    let bar_area = 200i32;
    let svg_w = label_w + bar_area + 8;
    let svg_h = (n as i32) * row_h + 8;
    let pad = 4i32;

    let rank_to_x = move |rank: u32| -> i32 {
        let r = rank.max(1).min(n as u32) as i32;
        label_w + pad + ((r - 1) * (bar_area - 2 * pad)) / (n as i32 - 1).max(1)
    };

    let rows: Vec<_> = rankings
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let cy = pad + (i as i32) * row_h + row_h / 2;
            let x_lo = rank_to_x(r.rank_range_90.0);
            let x_hi = rank_to_x(r.rank_range_90.1);
            let x_med = rank_to_x(r.rank);
            let bar_width = (x_hi - x_lo).max(4);
            let display_name: String = player_map
                .get(&r.player_id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| format!("#{}", r.player_id.0))
                .chars()
                .take(12)
                .collect();
            let is_active = r.is_active;
            let bar_color = if !is_active {
                "#4b5563"
            } else if r.rank <= 3 {
                "#ca8a04"
            } else {
                "#2563eb"
            };
            let dot_color = if !is_active {
                "#6b7280"
            } else if r.rank <= 3 {
                "#fbbf24"
            } else {
                "#60a5fa"
            };
            let text_fill = if is_active { "#e5e7eb" } else { "#6b7280" };
            (
                cy,
                x_lo,
                x_hi,
                x_med,
                bar_width,
                display_name,
                bar_color,
                dot_color,
                text_fill,
            )
        })
        .collect();

    view! {
        <div class="overflow-x-auto -mx-4 px-4">
            <p class="text-xs text-gray-500 mb-2">"Rank uncertainty (90% interval)"</p>
            <svg
                viewBox=format!("0 0 {svg_w} {svg_h}")
                style="width:100%; max-width:500px; height:auto; display:block;"
            >
                {rows.into_iter().map(|(cy, x_lo, _x_hi, x_med, bar_width, name, bar_color, dot_color, text_fill)| {
                    view! {
                        <g>
                            <text x=label_w-4 y=cy+4 font-size="10"
                                fill=text_fill text-anchor="end">{name}</text>
                            <rect x=label_w+pad y=cy-3 width=bar_area-2*pad
                                height=6 rx=3 fill="#1f2937"/>
                            <rect x=x_lo y=cy-4 width=bar_width
                                height=8 rx=4 fill=bar_color fill-opacity="0.7"/>
                            <circle cx=x_med cy=cy r=4 fill=dot_color/>
                        </g>
                    }
                }).collect_view()}
            </svg>
        </div>
    }.into_any()
}

// ══════════════════════════════════════════════════════════════════════════════
// PLAYERS TAB
// ══════════════════════════════════════════════════════════════════════════════

#[component]
pub fn PlayersTab() -> impl IntoView {
    let ctx = use_context::<AppContext>().expect("AppContext missing");

    let on_toggle = {
        let ctx = ctx.clone();
        move |pid: PlayerId, is_active: bool| {
            ctx.session.update(|opt| {
                if let Some(manager) = opt {
                    if is_active {
                        manager.deactivate_player(pid);
                    } else {
                        manager.reactivate_player(pid);
                    }
                }
            });
            ctx.session.with(|s| {
                if let Some(m) = s {
                    save_session(m);
                }
            });
        }
    };

    view! {
        <div class="px-4 py-5">
            {move || {
                let session_opt = ctx.session.get();
                let manager = match session_opt.as_ref() {
                    Some(m) => m,
                    None => return view! { <LoadingOrMissing/> }.into_any(),
                };

                let mut players: Vec<_> = manager.state.players.values().cloned().collect();
                players.sort_by_key(|p| p.id.0);

                let rankings_map: HashMap<PlayerId, &app_core::models::PlayerRanking> =
                    manager.state.rankings.iter().map(|r| (r.player_id, r)).collect();

                view! {
                    <div class="space-y-3">
                        <div class="flex items-center justify-between mb-1">
                            <h2 class="font-bold text-lg">"Players"</h2>
                            <span class="text-sm text-gray-400">
                                {players.iter().filter(|p| p.status == app_core::models::PlayerStatus::Active).count()}
                                " active"
                            </span>
                        </div>
                        <p class="text-xs text-gray-500">
                            "Deactivating a player removes them from future scheduling "
                            "but keeps their match history."
                        </p>
                        {players.into_iter().map(|p| {
                            let pid = p.id;
                            let is_active = p.status == app_core::models::PlayerStatus::Active;
                            let name = p.name.clone();
                            let rank_info = rankings_map.get(&pid).map(|r| {
                                (r.rank, r.matches_played, r.total_goals)
                            });
                            view! {
                                <div class="content-auto-card bg-gray-900 border border-gray-700/50 \
                                            rounded-xl px-4 py-3 flex items-center gap-3">
                                    // Status dot
                                    <span class=move || format!(
                                        "w-2.5 h-2.5 rounded-full shrink-0 {}",
                                        if is_active { "bg-green-400" } else { "bg-gray-600" }
                                    )/>

                                    // Name + stats
                                    <div class="flex-1 min-w-0">
                                        <p class=move || format!(
                                            "font-medium text-sm {}",
                                            if is_active { "text-white" } else { "text-gray-500" }
                                        )>
                                            {name.clone()}
                                        </p>
                                        <p class="text-xs text-gray-500">
                                            {rank_info.map(|(rank, played, goals)| {
                                                format!("Rank #{rank} · {played} matches · {goals} goals")
                                            }).unwrap_or_else(|| "No matches yet".to_string())}
                                        </p>
                                    </div>

                                    // Active/Inactive toggle
                                    <button
                                        class=move || format!(
                                            "px-3 py-1.5 rounded-lg text-xs font-semibold \
                                             transition-colors min-h-[36px] {}",
                                            if is_active {
                                                "bg-gray-800 text-gray-300 hover:bg-red-950/40 \
                                                 hover:text-red-400"
                                            } else {
                                                "bg-gray-800 text-gray-500 hover:bg-green-950/40 \
                                                 hover:text-green-400"
                                            }
                                        )
                                        on:click=move |_| on_toggle(pid, is_active)
                                    >
                                        {if is_active { "Deactivate" } else { "Reactivate" }}
                                    </button>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                }.into_any()
            }}
        </div>
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// ONLINE TAB
// ══════════════════════════════════════════════════════════════════════════════

#[component]
pub fn OnlineTab() -> impl IntoView {
    let ctx = use_context::<AppContext>().expect("AppContext missing");

    // Sync state loaded from localStorage; updated when "Go Online" succeeds
    let sync: RwSignal<Option<SyncState>> = RwSignal::new(None);
    let is_going_online = RwSignal::new(false);
    let is_pulling_assistant_results = RwSignal::new(false);
    let online_error = RwSignal::new(String::new());
    let assistant_pull_status = RwSignal::new(String::new());

    // Coach recovery PIN signals
    let pin_input = RwSignal::new(String::new());
    let pin_status = RwSignal::new(String::new()); // "" | "Setting…" | "PIN set." | error
    let pin_saved = RwSignal::new(false);

    // Assistant link PIN signals
    let asst_pin_input = RwSignal::new(String::new());
    let asst_pin_status = RwSignal::new(String::new());
    let asst_pin_saved = RwSignal::new(false);

    // Player link PIN signals
    let plyr_pin_input = RwSignal::new(String::new());
    let plyr_pin_status = RwSignal::new(String::new());
    let plyr_pin_saved = RwSignal::new(false);

    // Load existing sync state if present
    Effect::new(move |_| {
        let id = ctx.session.with(|s| {
            s.as_ref()
                .and_then(|m| m.state.config.as_ref())
                .map(|c| c.id.to_string())
        });
        if let Some(id) = id {
            sync.set(load_sync_state(&id));
        }
    });

    let on_go_online = {
        let ctx = ctx.clone();
        move |_| {
            let events = ctx
                .session
                .with(|s| s.as_ref().map(|m| m.log.all().to_vec()));
            let session_id = ctx.session.with(|s| {
                s.as_ref()
                    .and_then(|m| m.state.config.as_ref())
                    .map(|c| c.id.to_string())
            });

            if let (Some(events), Some(session_id)) = (events, session_id) {
                is_going_online.set(true);
                online_error.set(String::new());
                leptos::task::spawn_local(async move {
                    match go_online(&session_id, &events).await {
                        Ok(state) => {
                            sync.set(Some(state));
                            is_going_online.set(false);
                        }
                        Err(e) => {
                            online_error.set(format!("Failed to go online: {e}"));
                            is_going_online.set(false);
                        }
                    }
                });
            }
        }
    };

    let on_sync = StoredValue::new({
        let ctx = ctx.clone();
        move |_| {
            let events = ctx
                .session
                .with(|s| s.as_ref().map(|m| m.log.all().to_vec()));
            // Build archive snapshot from current session state (synchronously,
            // before entering the async block). Only populated when rankings exist.
            let archive = ctx.session.with(|s| {
                s.as_ref().and_then(|m| {
                    let config = m.state.config.as_ref()?;
                    let player_names: HashMap<String, String> = m
                        .state
                        .players
                        .values()
                        .map(|p| (p.id.0.to_string(), p.name.clone()))
                        .collect();
                    Some(SessionArchive {
                        sport: config.sport.to_string(),
                        team_size: config.team_size,
                        player_names,
                        final_rankings: if m.state.rankings.is_empty() {
                            None
                        } else {
                            Some(m.state.rankings.clone())
                        },
                    })
                })
            });
            if let Some(events) = events {
                if let Some(mut state) = sync.get_untracked() {
                    online_error.set(String::new());
                    leptos::task::spawn_local(async move {
                        if let Err(e) = push_new_events(&mut state, &events).await {
                            online_error.set(format!("Sync failed: {e}"));
                        } else {
                            // Best-effort: push archive snapshot so final results survive
                            // the raw event retention window. Errors are silently ignored.
                            if let Some(arch) = archive {
                                let _ = push_session_archive(&state, &arch).await;
                            }
                            sync.set(Some(state));
                        }
                    });
                }
            }
        }
    });

    let on_pull_assistant_results = StoredValue::new({
        let ctx = ctx.clone();
        move |_| {
            let Some(sync_state) = sync.get_untracked() else {
                return;
            };
            let local_events = ctx
                .session
                .with(|session| session.as_ref().map(|manager| manager.log.all().to_vec()));
            let Some(local_events) = local_events else {
                return;
            };

            is_pulling_assistant_results.set(true);
            online_error.set(String::new());
            assistant_pull_status.set(String::new());

            leptos::task::spawn_local(async move {
                match pull_assistant_score_events(sync_state, &local_events).await {
                    Ok(pulled) => {
                        let imported_count = pulled.assistant_score_events.len();
                        let updated_sync_state = pulled.updated_sync_state;
                        ctx.session.update(|session| {
                            let Some(manager) = session else {
                                return;
                            };
                            for payload in pulled.assistant_score_events {
                                manager.log.append(payload, Role::Assistant);
                            }
                            manager.state = materialize(&manager.log);
                            save_session(manager);
                        });
                        sync.set(Some(updated_sync_state));
                        assistant_pull_status.set(if imported_count == 0 {
                            if pulled.new_server_events_seen == 0 {
                                "No new server updates.".to_string()
                            } else {
                                "No new assistant-entered scores found.".to_string()
                            }
                        } else if imported_count == 1 {
                            "Pulled 1 assistant-entered score.".to_string()
                        } else {
                            format!("Pulled {imported_count} assistant-entered scores.")
                        });
                    }
                    Err(error) => {
                        online_error.set(format!("Pull failed: {error}"));
                    }
                }
                is_pulling_assistant_results.set(false);
            });
        }
    });

    view! {
        <div class="px-4 py-5">
            {move || {
                let sync_state = sync.get();

                match sync_state {
                    None => {
                        // Not yet online
                        view! {
                            <div class="text-center py-8">
                                <p class="text-3xl mb-3">"📡"</p>
                                <h2 class="font-bold text-lg mb-2">"Go Online"</h2>
                                <p class="text-gray-400 text-sm mb-6 max-w-xs mx-auto">
                                    "Upload this session so assistants and players can connect "
                                    "with share links. All computation stays on your device."
                                </p>
                                {move || {
                                    let err = online_error.get();
                                    (!err.is_empty()).then(|| view! {
                                        <p class="text-red-400 text-sm mb-4">{err}</p>
                                    })
                                }}
                                <button
                                    class="px-8 py-4 bg-blue-600 hover:bg-blue-500 \
                                           text-white font-semibold rounded-xl \
                                           transition-colors min-h-[52px] \
                                           disabled:opacity-50 disabled:cursor-not-allowed"
                                    disabled=move || is_going_online.get()
                                    on:click=on_go_online
                                >
                                    {move || if is_going_online.get() { "Uploading…" } else { "Go Online" }}
                                </button>
                            </div>
                        }.into_any()
                    }

                    Some(state) => {
                        // Online — show share links
                        let assistant_url = state.assistant_url();
                        let player_url = state.player_url();

                        view! {
                            <div class="space-y-4">
                                <div class="flex items-center justify-between">
                                    <h2 class="font-bold text-lg">"Session Online"</h2>
                                    <span class="text-xs bg-green-900/50 text-green-400 \
                                                 border border-green-700/50 px-2 py-1 rounded-full \
                                                 font-medium">
                                        "Live"
                                    </span>
                                </div>

                                {move || {
                                    let err = online_error.get();
                                    (!err.is_empty()).then(|| view! {
                                        <p class="text-red-400 text-sm">{err}</p>
                                    })
                                }}

                                // Sync button
                                <div class="space-y-2">
                                    <button
                                        class="w-full py-3 bg-gray-800 hover:bg-gray-700 \
                                               text-white font-medium rounded-xl transition-colors \
                                               min-h-[48px] border border-gray-700/50"
                                        on:click=move |ev| on_sync.with_value(|handler| handler(ev))
                                    >
                                        "Push Latest Events"
                                    </button>
                                    <button
                                        class="w-full py-3 bg-blue-950 hover:bg-blue-900 \
                                               text-blue-100 font-medium rounded-xl transition-colors \
                                               min-h-[48px] border border-blue-800/60 \
                                               disabled:opacity-50 disabled:cursor-not-allowed"
                                        disabled=move || is_pulling_assistant_results.get()
                                        on:click=move |ev| {
                                            on_pull_assistant_results.with_value(|handler| handler(ev))
                                        }
                                    >
                                        {move || {
                                            if is_pulling_assistant_results.get() {
                                                "Pulling Assistant Results…"
                                            } else {
                                                "Pull Assistant Results"
                                            }
                                        }}
                                    </button>
                                    {move || {
                                        let status = assistant_pull_status.get();
                                        (!status.is_empty()).then(|| view! {
                                            <p class="text-sm text-blue-300">{status}</p>
                                        })
                                    }}
                                    <p class="text-xs text-gray-500">
                                        "Use this after assistants submit scores from their own devices."
                                    </p>
                                </div>

                                // Assistant link
                                <ShareLinkCard
                                    label="Assistant Link"
                                    description="Assistants can enter scores"
                                    url=assistant_url.clone()
                                />

                                // Player link
                                <ShareLinkCard
                                    label="Player Link"
                                    description="Players can view their schedule"
                                    url=player_url.clone()
                                />

                                // Assistant link PIN protection
                                {
                                    let asst_token = state.assistant_token.clone();
                                    view! {
                                        <TokenPinCard
                                            label="Assistant Link PIN"
                                            description="Require a PIN to access the assistant link"
                                            token=asst_token
                                            pin_input=asst_pin_input
                                            pin_status=asst_pin_status
                                            pin_saved=asst_pin_saved
                                        />
                                    }
                                }

                                // Player link PIN protection
                                {
                                    let plyr_token = state.player_token.clone();
                                    view! {
                                        <TokenPinCard
                                            label="Player Link PIN"
                                            description="Require a PIN to view the player schedule"
                                            token=plyr_token
                                            pin_input=plyr_pin_input
                                            pin_status=plyr_pin_status
                                            pin_saved=plyr_pin_saved
                                        />
                                    }
                                }

                                // Recovery PIN
                                {
                                    let sync_for_pin = state.clone();
                                    view! {
                                        <div class="bg-gray-900 border border-gray-700/50 rounded-xl p-4">
                                            <p class="text-xs font-semibold uppercase tracking-widest \
                                                       text-gray-500 mb-3">
                                                "Recovery PIN"
                                            </p>
                                            <p class="text-sm text-gray-400 mb-3">
                                                "Set a 4–8 digit PIN so you can reload this coach session "
                                                "from another device if your main device runs out of power."
                                            </p>
                                            <div class="mb-3 rounded-lg border border-gray-700/60 bg-gray-800/60 px-3 py-2">
                                                <p class="text-[11px] uppercase tracking-wide text-gray-500 mb-1">
                                                    "Session ID"
                                                </p>
                                                <input
                                                    type="text"
                                                    readonly
                                                    class="w-full bg-transparent text-sm font-mono text-gray-200 \
                                                           focus:outline-none"
                                                    prop:value=sync_for_pin.session_id.clone()
                                                    on:focus=select_input_text_on_focus
                                                    autocapitalize="off"
                                                    autocomplete="off"
                                                    spellcheck="false"
                                                />
                                                <p class="mt-2 text-[11px] text-gray-500">
                                                    "Copy this exact UUID for recovery. Keep the hyphens."
                                                </p>
                                            </div>
                                            {move || {
                                                let status = pin_status.get();
                                                (!status.is_empty()).then(|| view! {
                                                    <p class={if pin_saved.get() {
                                                        "text-green-400 text-sm mb-2"
                                                    } else {
                                                        "text-red-400 text-sm mb-2"
                                                    }}>{status}</p>
                                                })
                                            }}
                                            <div class="flex gap-2">
                                                <input
                                                    type="password"
                                                    inputmode="numeric"
                                                    maxlength="8"
                                                    placeholder="4–8 digits"
                                                    class="flex-1 bg-gray-800 border border-gray-600 \
                                                           rounded-lg px-3 py-2 text-white text-sm \
                                                           placeholder-gray-500 focus:outline-none \
                                                           focus:border-blue-500 min-h-[44px]"
                                                    prop:value=move || pin_input.get()
                                                    on:input=move |ev| {
                                                        let val = event_target_value(&ev);
                                                        pin_input.set(val.chars().filter(|c| c.is_ascii_digit()).take(8).collect());
                                                    }
                                                />
                                                <button
                                                    class="px-4 py-2 bg-blue-600 hover:bg-blue-500 \
                                                           text-white text-sm font-medium rounded-lg \
                                                           transition-colors min-h-[44px] \
                                                           disabled:opacity-50 disabled:cursor-not-allowed"
                                                    on:click={
                                                        let sync_state = sync_for_pin.clone();
                                                        move |_| {
                                                            let pin = pin_input.get_untracked();
                                                            if pin.len() < 4 {
                                                                pin_status.set("PIN must be at least 4 digits.".to_string());
                                                                pin_saved.set(false);
                                                                return;
                                                            }
                                                            let sync_state = sync_state.clone();
                                                            pin_status.set("Setting…".to_string());
                                                            leptos::task::spawn_local(async move {
                                                                match set_recovery_pin(&sync_state, &pin).await {
                                                                    Ok(_) => {
                                                                        pin_status.set("Recovery PIN saved.".to_string());
                                                                        pin_saved.set(true);
                                                                        pin_input.set(String::new());
                                                                    }
                                                                    Err(e) => {
                                                                        pin_status.set(format!("Error: {e}"));
                                                                        pin_saved.set(false);
                                                                    }
                                                                }
                                                            });
                                                        }
                                                    }
                                                >
                                                    "Set PIN"
                                                </button>
                                            </div>
                                        </div>
                                    }
                                }
                            </div>
                        }.into_any()
                    }
                }
            }}
        </div>
    }
}

/// Render a URL as a QR code SVG string using the `qrcode` crate.
/// Returns an SVG with a white-on-black module grid, sized `size`×`size` pixels.
fn qr_svg(url: &str, size: u32) -> String {
    use qrcode::render::svg;
    use qrcode::{EcLevel, QrCode};

    let code = match QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    code.render::<svg::Color<'_>>()
        .min_dimensions(size, size)
        .max_dimensions(size, size)
        .dark_color(svg::Color("#ffffff"))
        .light_color(svg::Color("#111827")) // gray-900
        .quiet_zone(true)
        .build()
}

#[component]
fn ShareLinkCard(label: &'static str, description: &'static str, url: String) -> impl IntoView {
    let copied = RwSignal::new(false);
    let url_clone = url.clone();
    let qr = qr_svg(&url, 200);

    let on_copy = move |_| {
        let url2 = url_clone.clone();
        if let Some(window) = web_sys::window() {
            let _ = window.navigator().clipboard().write_text(&url2);
            copied.set(true);
        }
    };

    view! {
        <div class="bg-gray-900 border border-gray-700/50 rounded-xl p-4">
            <div class="flex items-center justify-between mb-1">
                <span class="text-sm font-semibold text-white">{label}</span>
                {move || if copied.get() {
                    view! { <span class="text-xs text-green-400 font-medium">"Copied ✓"</span> }.into_any()
                } else {
                    view! {
                        <button
                            class="text-xs text-blue-400 hover:text-blue-300 font-medium \
                                   min-h-[32px] px-2"
                            on:click=on_copy.clone()
                        >
                            "Copy"
                        </button>
                    }.into_any()
                }}
            </div>
            <p class="text-xs text-gray-500 mb-2">{description}</p>
            // QR code (inline SVG)
            {if !qr.is_empty() {
                view! {
                    <div class="flex justify-center my-3 rounded-lg overflow-hidden">
                        <div inner_html=qr style="width:200px;height:200px;"/>
                    </div>
                }.into_any()
            } else {
                view! { <div/> }.into_any()
            }}
            <p class="text-xs text-gray-400 font-mono break-all bg-gray-800 \
                      rounded px-2 py-1.5 select-all">
                {url}
            </p>
        </div>
    }
}

// ── Token PIN management card ─────────────────────────────────────────────────

#[component]
fn TokenPinCard(
    label: &'static str,
    description: &'static str,
    token: String,
    pin_input: RwSignal<String>,
    pin_status: RwSignal<String>,
    pin_saved: RwSignal<bool>,
) -> impl IntoView {
    view! {
        <div class="bg-gray-900 border border-gray-700/50 rounded-xl p-4">
            <p class="text-xs font-semibold uppercase tracking-widest text-gray-500 mb-1">
                {label}
            </p>
            <p class="text-sm text-gray-400 mb-3">{description}</p>
            {move || {
                let status = pin_status.get();
                (!status.is_empty()).then(|| view! {
                    <p class={if pin_saved.get() {
                        "text-green-400 text-sm mb-2"
                    } else {
                        "text-red-400 text-sm mb-2"
                    }}>{status}</p>
                })
            }}
            <div class="flex gap-2">
                <input
                    type="password"
                    inputmode="numeric"
                    maxlength="8"
                    placeholder="4–8 digits (blank = no PIN)"
                    class="flex-1 bg-gray-800 border border-gray-600 rounded-lg px-3 py-2 \
                           text-white text-sm placeholder-gray-500 focus:outline-none \
                           focus:border-blue-500 min-h-[44px]"
                    prop:value=move || pin_input.get()
                    on:input=move |ev| {
                        let val = event_target_value(&ev);
                        pin_input.set(val.chars().filter(|c| c.is_ascii_digit()).take(8).collect());
                    }
                />
                <button
                    class="px-4 py-2 bg-blue-600 hover:bg-blue-500 text-white text-sm \
                           font-medium rounded-lg transition-colors min-h-[44px] \
                           disabled:opacity-50 disabled:cursor-not-allowed"
                    on:click={
                        let token = token.clone();
                        move |_| {
                            let pin = pin_input.get_untracked();
                            // Allow empty (clears PIN) or 4–8 digits
                            if !pin.is_empty() && pin.len() < 4 {
                                pin_status.set("PIN must be 4–8 digits, or leave blank to remove.".to_string());
                                pin_saved.set(false);
                                return;
                            }
                            let tok = token.clone();
                            pin_status.set(if pin.is_empty() { "Clearing…" } else { "Setting…" }.to_string());
                            leptos::task::spawn_local(async move {
                                match set_token_pin(&tok, &pin).await {
                                    Ok(_) => {
                                        pin_status.set(if pin.is_empty() {
                                            "PIN removed.".to_string()
                                        } else {
                                            "PIN set.".to_string()
                                        });
                                        pin_saved.set(true);
                                        pin_input.set(String::new());
                                    }
                                    Err(e) => {
                                        pin_status.set(format!("Error: {e}"));
                                        pin_saved.set(false);
                                    }
                                }
                            });
                        }
                    }
                >
                    "Set PIN"
                </button>
            </div>
        </div>
    }
}

// ── Shared loading placeholder ─────────────────────────────────────────────────

#[component]
fn LoadingOrMissing() -> impl IntoView {
    view! {
        <div class="text-center py-12 text-gray-500">
            <p>"Loading session…"</p>
            <p class="text-sm mt-1">
                <a href="/coach" class="text-blue-400 hover:text-blue-300">"← Back to sessions"</a>
            </p>
        </div>
    }
}
