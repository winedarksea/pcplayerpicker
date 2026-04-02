use crate::meta::use_page_meta;
use crate::state::{save_session, AppContext};
use app_core::io::csv::import_players;
use app_core::models::{ScoreEntryMode, SessionConfig, Sport};
use app_core::scheduler::fields_needed;
use app_core::session::SessionManager;
use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

const TEAM_SIZES: &[u8] = &[1, 2, 3, 4, 5, 7, 11];

fn score_entry_mode_help_text(mode: ScoreEntryMode) -> &'static str {
    match mode {
        ScoreEntryMode::PointsPerPlayer => {
            "Enter points for each player individually. Best when teammates can score different amounts."
        }
        ScoreEntryMode::PointsPerTeam => {
            "Enter one total for each team. Best when the match is decided by the team score."
        }
        ScoreEntryMode::WinDrawLose => {
            "Record only the outcome: Team A win, draw, or Team B win. Best when exact points do not matter."
        }
    }
}

fn score_entry_mode_tooltip_id(mode: ScoreEntryMode) -> &'static str {
    match mode {
        ScoreEntryMode::PointsPerPlayer => "score-entry-mode-help-points-per-player",
        ScoreEntryMode::PointsPerTeam => "score-entry-mode-help-points-per-team",
        ScoreEntryMode::WinDrawLose => "score-entry-mode-help-win-draw-lose",
    }
}

// ── Component ────────────────────────────────────────────────────────────────

#[component]
pub fn SetupPage() -> impl IntoView {
    use_page_meta(
        "New Session · PCPlayerPicker",
        "Configure players, scheduling cadence, and field needs for a new session.",
    );

    let ctx = use_context::<AppContext>().expect("AppContext missing");
    let navigate = use_navigate();

    // ── Form state ────────────────────────────────────────────────────────
    let sport = RwSignal::new(Sport::Soccer);
    let team_size = RwSignal::new(2u8);
    let score_entry_mode = RwSignal::new(ScoreEntryMode::PointsPerPlayer);
    let player_count = RwSignal::new(8u32);
    let sched_frequency = RwSignal::new(1u8); // reschedule every N rounds
    let match_duration = RwSignal::new(String::new()); // empty = untimed
    let error_msg = RwSignal::new(String::new());

    // Player names: default to "Player N", updated when count changes
    let player_names: RwSignal<Vec<String>> =
        RwSignal::new((1..=8u32).map(|i| format!("Player {i}")).collect());

    // CSV import state
    let show_csv = RwSignal::new(false);
    let csv_text = RwSignal::new(String::new());
    let csv_error = RwSignal::new(String::new());

    // Keep player_names length in sync with player_count
    Effect::new(move |_| {
        let count = player_count.get() as usize;
        player_names.update(|names| {
            // Compute the index of the *next* new slot before the mutable borrow
            let mut next_idx = names.len() + 1;
            names.resize_with(count, || {
                let name = format!("Player {next_idx}");
                next_idx += 1;
                name
            });
        });
    });

    // Derived: minimum players needed, fields at current team size
    let min_players = move || (team_size.get() as u32) * 4; // need at least 2 full matches
    let fields_count = move || fields_needed(player_count.get() as usize, team_size.get() as usize);
    let players_benched = move || {
        let p = player_count.get() as usize;
        let ts = team_size.get() as usize;
        p % (2 * ts)
    };

    // ── Submit ────────────────────────────────────────────────────────────
    let on_create = move |_| {
        let count = player_count.get();
        let ts = team_size.get();

        if count < (ts as u32) * 4 {
            error_msg.set(format!(
                "Need at least {} players for {}v{} (two full teams).",
                ts as u32 * 4,
                ts,
                ts
            ));
            return;
        }

        let duration = {
            let s = match_duration.get_untracked();
            if s.is_empty() {
                None
            } else {
                s.parse::<u16>().ok()
            }
        };

        let mut config = SessionConfig::new(ts, sched_frequency.get(), sport.get_untracked());
        config.score_entry_mode = score_entry_mode.get_untracked();
        config.match_duration_minutes = duration;

        let mut manager = SessionManager::new(config);

        let names = player_names.get_untracked();
        for name in &names {
            manager.add_player(name.clone());
        }

        let session_id = manager.state.config.as_ref().unwrap().id.to_string();

        save_session(&manager);
        ctx.session.set(Some(manager));
        navigate(
            &format!("/coach/session/{session_id}/matches"),
            Default::default(),
        );
    };

    // ── View ──────────────────────────────────────────────────────────────
    view! {
        <div class="app-theme min-h-screen bg-gray-950 text-white">
            // ── Nav bar ───────────────────────────────────────────────────
            <div class="flex items-center px-4 pt-6 pb-2 gap-3">
                <a href="/coach"
                   class="text-gray-400 hover:text-white text-2xl leading-none min-w-[44px] \
                          min-h-[44px] flex items-center">
                    "←"
                </a>
                <h1 class="text-xl font-bold">"New Session"</h1>
            </div>

            <div class="px-4 pb-16 space-y-8 max-w-lg mx-auto">

                // ── Team size ─────────────────────────────────────────────
                <section>
                    <h2 class="section-label">"Sport"</h2>
                    <div class="flex flex-wrap gap-2">
                        {Sport::built_in_sports().iter().map(|sport_option| {
                            let sport_option = sport_option.clone();
                            let label = sport_option.profile().label;
                            let class_sport_option = sport_option.clone();
                            let click_sport_option = sport_option.clone();
                            view! {
                                <button
                                    class=move || {
                                        let base = "px-4 py-3 rounded-xl border font-semibold \
                                                    text-sm transition-colors min-h-[48px]";
                                        if sport.get() == class_sport_option {
                                            format!("{base} bg-blue-600 border-blue-500 text-white")
                                        } else {
                                            format!("{base} bg-gray-900 border-gray-700 \
                                                    text-gray-300 hover:border-gray-500")
                                        }
                                    }
                                    on:click=move |_| {
                                        let profile = click_sport_option.profile();
                                        sport.set(click_sport_option.clone());
                                        team_size.set(profile.default_team_size);
                                        score_entry_mode.set(profile.default_score_entry_mode);
                                        let min = (profile.default_team_size as u32) * 4;
                                        if player_count.get_untracked() < min {
                                            player_count.set(min);
                                        }
                                    }
                                >
                                    {label}
                                </button>
                            }
                        }).collect_view()}
                    </div>
                </section>

                <section>
                    <h2 class="section-label">"Team Size"</h2>
                    <div class="flex flex-wrap gap-2">
                        {TEAM_SIZES.iter().map(|&ts| {
                            view! {
                                <button
                                    class=move || {
                                        let base = "px-4 py-3 rounded-xl border font-semibold \
                                                    text-sm transition-colors min-h-[48px] \
                                                    min-w-[64px]";
                                        if team_size.get() == ts {
                                            format!("{base} bg-blue-600 border-blue-500 text-white")
                                        } else {
                                            format!("{base} bg-gray-900 border-gray-700 \
                                                    text-gray-300 hover:border-gray-500")
                                        }
                                    }
                                    on:click=move |_| {
                                        team_size.set(ts);
                                        // Bump player count if it falls below minimum
                                        let min = (ts as u32) * 4;
                                        if player_count.get_untracked() < min {
                                            player_count.set(min);
                                        }
                                    }
                                >
                                    {format!("{ts}v{ts}")}
                                </button>
                            }
                        }).collect_view()}
                    </div>
                </section>

                <section>
                    <h2 class="section-label">"Score Entry"</h2>
                    <div class="flex flex-wrap gap-2">
                        {[ScoreEntryMode::PointsPerPlayer, ScoreEntryMode::PointsPerTeam, ScoreEntryMode::WinDrawLose]
                            .into_iter()
                            .map(|mode| {
                                let help_text = score_entry_mode_help_text(mode);
                                let tooltip_id = score_entry_mode_tooltip_id(mode);
                                view! {
                                    <button
                                        class=move || {
                                            let base = "score-entry-mode-card relative px-4 py-3 pr-10 rounded-xl border font-semibold \
                                                        text-sm transition-colors min-h-[48px]";
                                            if score_entry_mode.get() == mode {
                                                format!("{base} bg-blue-600 border-blue-500 text-white")
                                            } else {
                                                format!("{base} bg-gray-900 border-gray-700 \
                                                        text-gray-300 hover:border-gray-500")
                                            }
                                        }
                                        title=help_text
                                        aria-describedby=tooltip_id
                                        on:click=move |_| score_entry_mode.set(mode)
                                    >
                                        <span>{mode.to_string()}</span>
                                        <span
                                            class="score-entry-mode-info-anchor absolute top-2.5 right-2.5"
                                            aria-hidden="true"
                                        >
                                            <span class="score-entry-mode-info-icon">"i"</span>
                                            <span
                                                id=tooltip_id
                                                class="score-entry-mode-info-tooltip w-56 text-left"
                                                role="tooltip"
                                            >
                                                {help_text}
                                            </span>
                                        </span>
                                    </button>
                                }
                            }).collect_view()}
                    </div>
                </section>

                // ── Player count ──────────────────────────────────────────
                <section>
                    <h2 class="section-label">"Players"</h2>
                    <div class="flex items-center gap-4 mb-4">
                        <button
                            class="stepper-btn"
                            on:click=move |_| {
                                let min = min_players();
                                let cur = player_count.get();
                                if cur > min { player_count.set(cur - 1); }
                            }
                        >
                            "−"
                        </button>
                        <span class="text-3xl font-bold w-12 text-center tabular-nums">
                            {move || player_count.get()}
                        </span>
                        <button
                            class="stepper-btn"
                            on:click=move |_| {
                                let cur = player_count.get();
                                if cur < 44 { player_count.set(cur + 1); }
                            }
                        >
                            "+"
                        </button>
                        <span class="text-sm text-gray-400">
                            {move || {
                                let f = fields_count();
                                let b = players_benched();
                                if b == 0 {
                                    format!("{f} field{} · no bench", if f == 1 { "" } else { "s" })
                                } else {
                                    format!("{f} field{} · {b} on bench",
                                        if f == 1 { "" } else { "s" })
                                }
                            }}
                        </span>
                    </div>

                    // Player name inputs
                    <div class="space-y-2">
                        {move || {
                            let count = player_count.get() as usize;
                            (0..count).map(|i| {
                                view! {
                                    <div class="flex items-center gap-2">
                                        <span class="text-gray-500 text-sm w-6 text-right shrink-0">
                                            {i + 1}
                                        </span>
                                        <input
                                            type="text"
                                            class="flex-1 bg-gray-900 border border-gray-700 \
                                                   rounded-lg px-3 py-2 text-white text-sm \
                                                   focus:outline-none focus:border-blue-500 \
                                                   min-h-[44px]"
                                            placeholder=move || format!("Player {}", i + 1)
                                            prop:value=move || {
                                                player_names.with(|n| n.get(i).cloned().unwrap_or_default())
                                            }
                                            on:input=move |ev| {
                                                let val = event_target_value(&ev);
                                                player_names.update(|n| {
                                                    if let Some(slot) = n.get_mut(i) {
                                                        *slot = val;
                                                    }
                                                });
                                            }
                                        />
                                    </div>
                                }
                            }).collect_view()
                        }}
                    </div>

                    // CSV import (inside Players section)
                    <div class="mt-3">
                        <button
                            class="text-xs text-gray-500 hover:text-gray-300 \
                                   underline underline-offset-2 transition-colors"
                            on:click=move |_| {
                                show_csv.update(|v| *v = !*v);
                                csv_error.set(String::new());
                            }
                        >
                            {move || if show_csv.get() { "▲ Hide CSV import" } else { "▼ Import names from CSV" }}
                        </button>

                        {move || show_csv.get().then(|| view! {
                            <div class="mt-2 space-y-2">
                                <p class="text-xs text-gray-500">
                                    "One name per line, or id,name format. Lines starting with # are ignored."
                                </p>
                                {move || {
                                    let err = csv_error.get();
                                    (!err.is_empty()).then(|| view! {
                                        <p class="text-xs text-red-400">{err}</p>
                                    })
                                }}
                                <textarea
                                    rows="6"
                                    placeholder="Alice\nBob\nCarol\n..."
                                    class="w-full bg-gray-900 border border-gray-700 rounded-lg \
                                           px-3 py-2 text-white text-sm font-mono \
                                           placeholder-gray-600 focus:outline-none \
                                           focus:border-blue-500 resize-none"
                                    prop:value=move || csv_text.get()
                                    on:input=move |ev| csv_text.set(event_target_value(&ev))
                                />
                                <button
                                    class="w-full py-2 bg-gray-800 hover:bg-gray-700 \
                                           text-white text-sm font-medium rounded-lg \
                                           transition-colors min-h-[44px] \
                                           border border-gray-600"
                                    on:click=move |_| {
                                        let raw = csv_text.get_untracked();
                                        match import_players(&raw) {
                                            Ok(names) if !names.is_empty() => {
                                                let min = min_players() as usize;
                                                let count = names.len().max(min);
                                                player_count.set(count as u32);
                                                player_names.update(|n| {
                                                    n.resize(count, String::new());
                                                    for (i, name) in names.iter().enumerate() {
                                                        n[i] = name.clone();
                                                    }
                                                });
                                                csv_error.set(String::new());
                                                show_csv.set(false);
                                            }
                                            Ok(_) => csv_error.set("No names found in CSV.".to_string()),
                                            Err(e) => csv_error.set(e.to_string()),
                                        }
                                    }
                                >
                                    "Load Names"
                                </button>
                            </div>
                        })}
                    </div>
                </section>

                // ── Options ───────────────────────────────────────────────
                <section>
                    <h2 class="section-label">"Options"</h2>
                    <div class="space-y-4">

                        // Scheduling frequency
                        <div class="flex items-center justify-between bg-gray-900 \
                                    border border-gray-700/50 rounded-xl px-4 py-3">
                            <div>
                                <p class="font-medium text-sm">"Reschedule Every"</p>
                                <p class="text-xs text-gray-400 mt-0.5">
                                    "Rounds before regenerating matchups"
                                </p>
                            </div>
                            <select
                                class="bg-gray-800 border border-gray-600 rounded-lg px-3 py-2 \
                                       text-white text-sm min-h-[44px]"
                                prop:value=move || sched_frequency.get().to_string()
                                on:change=move |ev| {
                                    if let Ok(n) = event_target_value(&ev).parse::<u8>() {
                                        sched_frequency.set(n);
                                    }
                                }
                            >
                                <option value="1">"1 round"</option>
                                <option value="2">"2 rounds"</option>
                                <option value="3">"3 rounds"</option>
                                <option value="5">"5 rounds"</option>
                            </select>
                        </div>

                        // Match duration
                        <div class="flex items-center justify-between bg-gray-900 \
                                    border border-gray-700/50 rounded-xl px-4 py-3">
                            <div>
                                <p class="font-medium text-sm">"Match Duration"</p>
                                <p class="text-xs text-gray-400 mt-0.5">"Minutes (leave blank if untimed)"</p>
                            </div>
                            <input
                                type="number"
                                min="1"
                                max="120"
                                placeholder="–"
                                class="bg-gray-800 border border-gray-600 rounded-lg px-3 py-2 \
                                       text-white text-sm text-right w-20 min-h-[44px] \
                                       focus:outline-none focus:border-blue-500"
                                prop:value=move || match_duration.get()
                                on:input=move |ev| match_duration.set(event_target_value(&ev))
                            />
                        </div>
                    </div>
                </section>

                // ── Error ─────────────────────────────────────────────────
                {move || {
                    let msg = error_msg.get();
                    if msg.is_empty() { None } else {
                        Some(view! {
                            <p class="text-red-400 text-sm bg-red-950/40 border border-red-800/50 \
                                      rounded-lg px-4 py-3">
                                {msg}
                            </p>
                        })
                    }
                }}

                // ── Create button ─────────────────────────────────────────
                <button
                    class="w-full py-4 bg-blue-600 hover:bg-blue-500 active:bg-blue-700 \
                           text-white font-bold text-lg rounded-2xl transition-colors \
                           min-h-[60px]"
                    on:click=on_create
                >
                    "Create Session"
                </button>
            </div>
        </div>
    }
}
