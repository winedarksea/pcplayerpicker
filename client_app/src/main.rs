mod meta;
mod pages;
mod state;
mod sync;

use crate::pages::assistant::AssistantPage;
use crate::pages::coach::{CoachHome, DashboardPage, SetupPage};
use crate::pages::player::PlayerPage;
use crate::pages::site::{FaqPage, LandingPage, TutorialPage};
use crate::state::{apply_dark_mode, restore_sessions_from_idb, restore_sessions_from_opfs, AppContext};
use leptos::prelude::*;
use leptos_router::{
    components::{Route, Router, Routes},
    path,
};

fn main() {
    console_error_panic_hook::set_once();
    register_service_worker();
    request_persistent_storage();
    // Recover sessions lost to Safari's storage eviction.
    // Run IDB first (faster), then OPFS (slower but more durable).
    leptos::task::spawn_local(async {
        restore_sessions_from_idb().await;
        restore_sessions_from_opfs().await;
    });
    leptos::mount::mount_to_body(App);
}

fn register_service_worker() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let service_worker = window.navigator().service_worker();
    let _ = service_worker.register("/sw.js");
}

/// Ask the browser to treat localStorage as persistent (not subject to eviction).
/// On iOS/Safari this reduces the risk of data loss when storage is under pressure.
/// The call is fire-and-forget; failure is silent and non-fatal.
fn request_persistent_storage() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let storage_mgr = window.navigator().storage();
    if let Ok(promise) = storage_mgr.persist() {
        // Spawn the promise so the JS engine resolves it; we don't need the bool result.
        leptos::task::spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        });
    }
}

#[component]
fn App() -> impl IntoView {
    let ctx = AppContext::new();
    // Sync dark-mode class on <html> from persisted preference on first render.
    apply_dark_mode(ctx.dark_mode.get_untracked());
    provide_context(ctx);

    view! {
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
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <div class="flex items-center justify-center min-h-screen bg-gray-950">
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
