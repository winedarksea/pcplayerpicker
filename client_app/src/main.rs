mod meta;
mod pages;
mod state;
mod sync;

use crate::pages::assistant::AssistantPage;
use crate::pages::coach::{CoachHome, DashboardPage, SetupPage};
use crate::pages::player::PlayerPage;
use crate::pages::site::{FaqPage, LandingPage, TutorialPage};
use crate::state::{
    apply_dark_mode, apply_dark_mode_class, restore_sessions_from_idb, restore_sessions_from_opfs,
    AppContext,
};
use leptos::prelude::*;
use leptos_router::{
    components::{Route, Router, Routes},
    hooks::use_location,
    path,
};

fn main() {
    #[cfg(debug_assertions)]
    console_error_panic_hook::set_once();
    pre_mount_init();
    leptos::mount::mount_to_body(App);
}

/// Everything that runs before `mount_to_body` initialises the Leptos executor.
///
/// IMPORTANT: this function must never call `leptos::task::spawn_local`.
/// The Leptos executor (any_spawner) is not configured until `mount_to_body`
/// runs; calling it earlier panics with:
///   "Executor::spawn_local called, but no global 'spawn_local' function is configured"
/// Use `wasm_bindgen_futures::spawn_local` for any async work here instead.
fn pre_mount_init() {
    register_service_worker();
}

fn register_service_worker() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let service_worker = window.navigator().service_worker();
    let _ = service_worker.register("/sw.js");
}

#[component]
fn App() -> impl IntoView {
    let ctx = AppContext::new();
    // Sync dark-mode class on <html> on first render without overriding preference persistence.
    apply_dark_mode_class(ctx.dark_mode.get_untracked());
    provide_context(ctx.clone());

    Effect::new(move |_| {
        if ctx.storage_restore_epoch.get() != 0 || ctx.storage_restore_in_progress.get() {
            return;
        }
        ctx.storage_restore_in_progress.set(true);
        leptos::task::spawn_local(async move {
            restore_sessions_from_idb().await;
            restore_sessions_from_opfs().await;
            ctx.storage_restore_in_progress.set(false);
            ctx.storage_restore_epoch.update(|n| *n += 1);
        });
    });

    view! {
        <>
            <Router>
                <Routes fallback=|| view! { <NotFound/> }>
                    <Route path=path!("/") view=LandingPage/>
                    <Route path=path!("/tutorial") view=TutorialPage/>
                    <Route path=path!("/faq") view=FaqPage/>

                    // Coach home — session list
                    <Route path=path!("/coach") view=CoachHome/>

                    // Session setup form
                    <Route path=path!("/coach/setup") view=SetupPage/>

                    // Session dashboard — default (no tab specified → Matches tab)
                    <Route path=path!("/coach/session/:id") view=DashboardPage/>

                    // Session dashboard — explicit tab param
                    // :tab matches "matches" | "results" | "analysis" | "online"
                    <Route path=path!("/coach/session/:id/:tab") view=DashboardPage/>

                    // Assistant access via share token
                    <Route path=path!("/a/:token") view=AssistantPage/>

                    // Player access via share token
                    <Route path=path!("/p/:token") view=PlayerPage/>

                    // Catch-all
                    <Route path=path!("/*any") view=NotFound/>
                </Routes>
            </Router>
            <ThemeToggleFab/>
        </>
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <div class="app-theme flex items-center justify-center min-h-screen bg-gray-950 text-white">
            <div class="text-center">
                <h1 class="text-5xl font-bold text-white mb-3">"404"</h1>
                <p class="text-gray-400 mb-6">"Page not found"</p>
                <a href="/"
                   class="text-blue-400 hover:text-blue-300 underline font-medium">
                    "← Back to Home"
                </a>
            </div>
        </div>
    }
}

#[component]
fn ThemeToggleFab() -> impl IntoView {
    let ctx = use_context::<AppContext>().expect("AppContext missing");
    let location = use_location();
    let dark = ctx.dark_mode;
    let on_toggle = move |_| {
        let next = !dark.get();
        dark.set(next);
        apply_dark_mode(next);
    };

    view! {
        {move || {
            let path = location.pathname.get();
            let is_site_page = path == "/" || path == "/tutorial" || path == "/faq";
            (!is_site_page).then(|| {
                view! {
                    <button
                        on:click=on_toggle
                        title="Toggle dark/light mode"
                        class="fixed bottom-4 right-4 z-50 rounded-full border border-white/20 bg-black/75 px-4 py-2 text-xs font-semibold uppercase tracking-[0.12em] text-white shadow-lg backdrop-blur transition hover:border-white/40 hover:bg-black/85"
                    >
                        {move || if dark.get() { "Light" } else { "Dark" }}
                    </button>
                }
            })
        }}
    }
}

#[cfg(test)]
mod tests {
    use super::pre_mount_init;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    /// Verifies that pre-mount initialisation never calls `leptos::task::spawn_local`.
    ///
    /// The Leptos executor (any_spawner) is not configured until `mount_to_body`
    /// runs.  Calling `leptos::task::spawn_local` before that panics with:
    ///   "Executor::spawn_local called, but no global 'spawn_local' function is configured"
    /// which leaves the body empty → black screen.
    ///
    /// In the wasm_bindgen_test environment no Leptos executor is configured, so
    /// any regression that reintroduces `leptos::task::spawn_local` inside
    /// `pre_mount_init` will cause this test to fail with that exact panic.
    #[wasm_bindgen_test]
    fn pre_mount_init_does_not_require_leptos_executor() {
        pre_mount_init();
    }
}
