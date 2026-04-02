use crate::meta::use_page_meta;
use crate::state::{delete_session, load_session_summaries, storage_get, storage_set, AppContext};
use crate::sync::recover_session;
use app_core::events::EventLog;
use app_core::session::SessionManager;
use js_sys::Reflect;
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_navigate;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

fn canonicalize_recovery_session_id(raw_session_id: &str) -> Option<String> {
    let trimmed = raw_session_id.trim().trim_matches('{').trim_matches('}');
    if trimmed.is_empty() {
        return None;
    }

    let compact_hex: String = trimmed.chars().filter(|ch| *ch != '-').collect();
    if compact_hex.len() != 32 || !compact_hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }

    Some(
        format!(
            "{}-{}-{}-{}-{}",
            &compact_hex[0..8],
            &compact_hex[8..12],
            &compact_hex[12..16],
            &compact_hex[16..20],
            &compact_hex[20..32],
        )
        .to_ascii_lowercase(),
    )
}

/// Ask the browser to treat storage as persistent (not subject to eviction).
/// Called when the coach app loads — not on the landing page.
fn request_persistent_storage() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let storage_mgr = window.navigator().storage();
    if let Ok(promise) = storage_mgr.persist() {
        leptos::task::spawn_local(async move {
            let _ = JsFuture::from(promise).await;
        });
    }
}

const IOS_NUDGE_KEY: &str = "pcpp_ios_nudge_dismissed";

/// Returns true if we are on iOS/iPadOS Safari and NOT running as a standalone
/// PWA (i.e. the user has NOT added the app to their Home Screen).
fn should_show_ios_nudge() -> bool {
    if storage_get(IOS_NUDGE_KEY).is_some() {
        return false; // user dismissed it before
    }
    let Some(window) = web_sys::window() else {
        return false;
    };
    let nav = window.navigator();
    // Detect iOS via userAgent
    let ua = Reflect::get(&nav, &JsValue::from_str("userAgent"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    let is_ios = ua.contains("iPhone") || ua.contains("iPad") || ua.contains("iPod");
    if !is_ios {
        return false;
    }
    // Check if already running as standalone (Home Screen install)
    let standalone = Reflect::get(&nav, &JsValue::from_str("standalone")).unwrap_or(JsValue::FALSE);
    !standalone.is_truthy()
}

#[component]
pub fn CoachHome() -> impl IntoView {
    use_page_meta(
        "Coach Sessions · PCPlayerPicker",
        "Create or resume offline-first coach sessions for scheduling and player ranking.",
    );

    let ctx = use_context::<AppContext>().expect("AppContext missing");
    let navigate = use_navigate();

    // Request persistent storage now that the user is in the app.
    request_persistent_storage();

    // Clear any loaded session when returning home
    Effect::new(move |_| {
        ctx.session.set(None);
    });

    // iOS "Add to Home Screen" nudge — improves storage durability on Safari
    let show_ios_nudge = RwSignal::new(should_show_ios_nudge());

    // Bump this signal to force a re-read of session summaries after delete
    let delete_trigger = RwSignal::new(0u32);

    // Server recovery signals
    let show_recover = RwSignal::new(false);
    let recover_id = RwSignal::new(String::new());
    let recover_pin = RwSignal::new(String::new());
    let recover_status = RwSignal::new(String::new());

    // CSV recovery signals
    let show_recover_csv = RwSignal::new(false);
    let recover_csv_text = RwSignal::new(String::new());
    let recover_csv_status = RwSignal::new(String::new());
    let recover_csv_selected_filename = RwSignal::new(String::new());

    let summaries = move || {
        delete_trigger.get(); // subscribe
        ctx.storage_restore_epoch.get(); // subscribe to startup restore completion
        load_session_summaries()
    };

    // Capture ctx and navigate by value — both are Clone/Copy-safe
    let on_resume = {
        let ctx = ctx.clone();
        let navigate = navigate.clone();
        move |id: String| {
            ctx.session.set(None);
            navigate(&format!("/coach/session/{id}/matches"), Default::default());
        }
    };

    let on_delete = move |id: String| {
        if let Some(win) = web_sys::window() {
            let confirmed = win
                .confirm_with_message("Delete this session and all local data for it?")
                .unwrap_or(false);
            if !confirmed {
                return;
            }
        }
        delete_session(&id);
        delete_trigger.update(|n| *n += 1);
    };

    let on_recover = {
        let ctx = ctx.clone();
        let navigate = navigate.clone();
        move |_| {
            let raw_session_id = recover_id.get_untracked();
            let pin = recover_pin.get_untracked().trim().to_string();
            let Some(session_id) = canonicalize_recovery_session_id(&raw_session_id) else {
                recover_status.set(
                    "Enter the full session ID shown online, including all UUID digits."
                        .to_string(),
                );
                return;
            };
            recover_id.set(session_id.clone());
            if pin.len() < 4 {
                recover_status.set("Enter a session ID and at least 4-digit PIN.".to_string());
                return;
            }
            recover_status.set("Recovering…".to_string());
            let ctx = ctx.clone();
            let navigate = navigate.clone();
            leptos::task::spawn_local(async move {
                match recover_session(&session_id, &pin).await {
                    Ok(resp) => {
                        let log = EventLog::from_saved(resp.events);
                        let manager = SessionManager::from_log(log);
                        // Persist locally then navigate
                        crate::state::save_session(&manager);
                        ctx.session.set(Some(manager));
                        navigate(
                            &format!("/coach/session/{}/matches", session_id),
                            Default::default(),
                        );
                    }
                    Err(e) => {
                        recover_status.set(format!("Failed: {e}"));
                    }
                }
            });
        }
    };

    let on_recover_csv = {
        let ctx = ctx.clone();
        let navigate = navigate.clone();
        move |_| {
            let text = recover_csv_text.get_untracked();
            if text.trim().is_empty() {
                recover_csv_status.set("Paste a results CSV to recover.".to_string());
                return;
            }
            match app_core::io::csv::import_results(&text) {
                Err(e) => {
                    recover_csv_status.set(format!("CSV error: {e}"));
                }
                Ok(imported) => {
                    let manager = app_core::session::SessionManager::from_results_csv(&imported);
                    let session_id = manager
                        .state
                        .config
                        .as_ref()
                        .map(|c| c.id.to_string())
                        .unwrap_or_default();
                    crate::state::save_session(&manager);
                    ctx.session.set(Some(manager));
                    navigate(
                        &format!("/coach/session/{session_id}/matches"),
                        Default::default(),
                    );
                }
            }
        }
    };

    let on_select_recovery_csv_file = {
        move |ev: web_sys::Event| {
            let Some(input) = ev
                .target()
                .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
            else {
                recover_csv_status.set("Could not read the selected file input.".to_string());
                return;
            };

            let Some(files) = input.files() else {
                recover_csv_selected_filename.set(String::new());
                return;
            };

            let Some(file) = files.get(0) else {
                recover_csv_selected_filename.set(String::new());
                return;
            };

            let filename = file.name();
            recover_csv_selected_filename.set(filename.clone());
            recover_csv_status.set(format!("Loading {filename}…"));

            let recover_csv_text = recover_csv_text;
            let recover_csv_status = recover_csv_status;
            leptos::task::spawn_local(async move {
                match JsFuture::from(file.text()).await {
                    Ok(text_value) => {
                        let text = text_value.as_string().unwrap_or_default();
                        recover_csv_text.set(text);
                        recover_csv_status
                            .set("CSV file loaded. Review or recover when ready.".to_string());
                    }
                    Err(_) => {
                        recover_csv_status.set("Failed to read the selected CSV file.".to_string());
                    }
                }
            });
        }
    };

    view! {
        <div class="app-theme min-h-screen bg-gray-950 text-white flex flex-col">
            // ── iOS Home Screen nudge ─────────────────────────────────────────
            {move || show_ios_nudge.get().then(|| view! {
                <div class="mx-4 mt-4 rounded-xl border border-amber-500/40 bg-amber-950/60 \
                            px-4 py-3 flex items-start gap-3">
                    <span class="text-xl shrink-0 mt-0.5" aria-hidden="true">"📌"</span>
                    <div class="flex-1 min-w-0">
                        <p class="text-sm font-semibold text-amber-300">
                            "Add to Home Screen for reliable storage"
                        </p>
                        <p class="text-xs text-amber-400/80 mt-0.5">
                            "Safari may clear app data after 7 days of inactivity unless \
                             installed. Tap the Share button "
                            <span class="font-medium">"⬆"</span>
                            " then \"Add to Home Screen\"."
                        </p>
                    </div>
                    <button
                        class="shrink-0 text-amber-500 hover:text-amber-300 text-xl \
                               leading-none min-w-[32px] min-h-[32px] flex items-center \
                               justify-center"
                        on:click=move |_| {
                            storage_set(IOS_NUDGE_KEY, "1");
                            show_ios_nudge.set(false);
                        }
                        title="Dismiss"
                    >
                        "×"
                    </button>
                </div>
            })}

            // ── Header ───────────────────────────────────────────────────────
            <header class="px-4 pt-10 pb-6 text-center">
                <h1 class="text-3xl font-bold tracking-tight text-white">"PCPlayerPicker"</h1>
                <p class="mt-1 text-gray-400 text-sm">
                    "Bayesian match scheduling · Player ranking"
                </p>
            </header>

            // ── New session button ────────────────────────────────────────────
            <div class="px-4 mb-6">
                <A
                    href="/coach/setup"
                    attr:class="flex items-center justify-center gap-2 w-full py-4 \
                                bg-blue-600 hover:bg-blue-500 active:bg-blue-700 \
                                text-white font-semibold text-lg rounded-2xl \
                                transition-colors min-h-[56px]"
                >
                    <span class="text-2xl leading-none">"+"</span>
                    "New Session"
                </A>
            </div>

            // ── Session list ──────────────────────────────────────────────────
            <div class="px-4 flex-1">
                {move || {
                    let restoring = ctx.storage_restore_in_progress.get();
                    let sessions = summaries();
                    if sessions.is_empty() {
                        view! {
                            <div class="text-center py-16 text-gray-500">
                                <p class="text-4xl mb-3">"📋"</p>
                                <p class="font-medium">
                                    {if restoring { "Checking local sessions…" } else { "No sessions yet" }}
                                </p>
                                <p class="text-sm mt-1">
                                    {if restoring {
                                        "Recovering backups from browser storage."
                                    } else {
                                        "Tap + New Session to get started"
                                    }}
                                </p>
                            </div>
                        }.into_any()
                    } else {
                        let on_resume = on_resume.clone();
                        let on_delete = on_delete;
                        view! {
                            <div>
                                <h2 class="text-xs font-semibold uppercase tracking-widest \
                                           text-gray-500 mb-3">
                                    "Recent Sessions"
                                </h2>
                                <ul class="space-y-3">
                                    {sessions.into_iter().map(|s| {
                                        let id_resume = s.id.clone();
                                        let id_del    = s.id.clone();
                                        let on_resume = on_resume.clone();
                                        view! {
                                            <li class="content-auto-card bg-gray-900 border \
                                                       border-gray-700/50 rounded-xl overflow-hidden">
                                                <button
                                                    class="w-full text-left px-4 py-4 \
                                                           hover:bg-gray-800 transition-colors \
                                                           min-h-[72px]"
                                                    on:click=move |_| on_resume(id_resume.clone())
                                                >
                                                    <div class="flex items-start justify-between">
                                                        <div>
                                                            <span class="font-semibold text-white">
                                                                {s.sport.clone()}
                                                                " "
                                                                {s.team_size}"v"{s.team_size}
                                                            </span>
                                                            <div class="text-sm text-gray-400 mt-0.5">
                                                                {s.player_count}" players · "
                                                                {s.rounds_played}" rounds · "
                                                                {s.score_entry_mode.clone()}
                                                            </div>
                                                        </div>
                                                        <span class="text-xs text-gray-500 shrink-0 ml-2 mt-0.5">
                                                            {s.created_at.clone()}
                                                        </span>
                                                    </div>
                                                </button>
                                                <div class="border-t border-gray-700/50 px-4 py-2 \
                                                            flex justify-end">
                                                    <button
                                                        class="text-xs text-red-400 hover:text-red-300 \
                                                               min-h-[36px] px-2"
                                                        on:click=move |_| on_delete(id_del.clone())
                                                    >
                                                        "Delete"
                                                    </button>
                                                </div>
                                            </li>
                                        }
                                    }).collect_view()}
                                </ul>
                            </div>
                        }.into_any()
                    }
                }}
            </div>

            // ── Recover session ───────────────────────────────────────────────
            <div class="px-4 pb-4">
                <button
                    class="w-full py-3 text-sm text-gray-500 hover:text-gray-300 \
                           border border-dashed border-gray-700 rounded-xl transition-colors \
                           min-h-[48px]"
                    on:click=move |_| show_recover.update(|v| *v = !*v)
                >
                    {move || if show_recover.get() { "▲ Hide Server Recovery" } else { "▼ Recover Session from Server" }}
                </button>

                {move || show_recover.get().then(|| view! {
                    <div class="mt-3 bg-gray-900 border border-gray-700/50 rounded-xl p-4 space-y-3">
                        <p class="text-xs text-gray-400">
                            "Paste the full session ID from the Online tab. Hyphens are accepted and will be normalized automatically."
                        </p>
                        {move || {
                            let s = recover_status.get();
                            (!s.is_empty()).then(|| view! {
                                <p class="text-sm text-red-400">{s}</p>
                            })
                        }}
                        <input
                            type="text"
                            placeholder="Session ID (UUID)"
                            class="w-full bg-gray-800 border border-gray-600 rounded-lg \
                                   px-3 py-2 text-white text-sm placeholder-gray-500 \
                                   focus:outline-none focus:border-blue-500 min-h-[44px]"
                            prop:value=move || recover_id.get()
                            on:input=move |ev| recover_id.set(event_target_value(&ev))
                            autocapitalize="off"
                            autocomplete="off"
                            spellcheck="false"
                        />
                        <input
                            type="password"
                            inputmode="numeric"
                            placeholder="Recovery PIN"
                            class="w-full bg-gray-800 border border-gray-600 rounded-lg \
                                   px-3 py-2 text-white text-sm placeholder-gray-500 \
                                   focus:outline-none focus:border-blue-500 min-h-[44px]"
                            prop:value=move || recover_pin.get()
                            on:input=move |ev| recover_pin.set(event_target_value(&ev))
                        />
                        <button
                            class="w-full py-3 bg-blue-600 hover:bg-blue-500 text-white \
                                   font-semibold rounded-xl transition-colors min-h-[48px]"
                            on:click=on_recover.clone()
                        >
                            "Recover Session"
                        </button>
                    </div>
                })}
            </div>

            // ── Recover from CSV ──────────────────────────────────────────────
            <div class="px-4 pb-4">
                <button
                    class="w-full py-3 text-sm text-gray-500 hover:text-gray-300 \
                           border border-dashed border-gray-700 rounded-xl transition-colors \
                           min-h-[48px]"
                    on:click=move |_| show_recover_csv.update(|v| *v = !*v)
                >
                    {move || if show_recover_csv.get() { "▲ Hide CSV Recovery" } else { "▼ Recover Session from CSV" }}
                </button>

                {move || show_recover_csv.get().then(|| view! {
                    <div class="mt-3 bg-gray-900 border border-gray-700/50 rounded-xl p-4 space-y-3">
                        <p class="text-xs text-gray-400">
                            "Paste a results CSV exported from a previous session, or choose \
                             a `.csv` file. \
                             A new session will be created with all players and match \
                             history loaded in — then use Update Rankings to resume."
                        </p>
                        {move || {
                            let s = recover_csv_status.get();
                            (!s.is_empty()).then(|| view! {
                                <p class="text-sm text-red-400">{s}</p>
                            })
                        }}
                        <textarea
                            placeholder="Paste results CSV here…"
                            class="w-full h-40 bg-gray-800 border border-gray-600 rounded-lg \
                                   px-3 py-2 text-white text-xs placeholder-gray-500 font-mono \
                                   focus:outline-none focus:border-blue-500 resize-y min-h-[44px]"
                            prop:value=move || recover_csv_text.get()
                            on:input=move |ev| recover_csv_text.set(event_target_value(&ev))
                        />
                        <div class="space-y-2">
                            <label
                                class="flex items-center justify-center w-full min-h-[44px] px-3 py-2 \
                                       bg-gray-800 hover:bg-gray-700 text-sm text-white rounded-lg \
                                       border border-gray-600 transition-colors cursor-pointer"
                            >
                                <input
                                    type="file"
                                    accept=".csv,text/csv"
                                    class="hidden"
                                    on:change=on_select_recovery_csv_file
                                />
                                "Choose CSV File"
                            </label>
                            {move || {
                                let filename = recover_csv_selected_filename.get();
                                (!filename.is_empty()).then(|| view! {
                                    <p class="text-xs text-gray-500">"Selected file: "{filename}</p>
                                })
                            }}
                        </div>
                        <button
                            class="w-full py-3 bg-blue-600 hover:bg-blue-500 text-white \
                                   font-semibold rounded-xl transition-colors min-h-[48px]"
                            on:click=on_recover_csv.clone()
                        >
                            "Recover Session"
                        </button>
                    </div>
                })}
            </div>

            // ── Footer ────────────────────────────────────────────────────────
            <footer class="px-4 py-6 text-center text-xs text-gray-600">
                "pcplayerpicker.com"
            </footer>
        </div>
    }
}
