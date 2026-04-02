use js_sys::{Function, Reflect};
use leptos::prelude::*;
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

fn navigator_user_agent() -> String {
    let Some(window) = web_sys::window() else {
        return String::new();
    };
    Reflect::get(&window.navigator(), &JsValue::from_str("userAgent"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default()
}

fn is_ios_browser() -> bool {
    let user_agent = navigator_user_agent();
    user_agent.contains("iPhone") || user_agent.contains("iPad") || user_agent.contains("iPod")
}

fn is_android_browser() -> bool {
    navigator_user_agent().contains("Android")
}

fn is_running_standalone() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };

    if let Ok(Some(display_mode_media_query)) = window.match_media("(display-mode: standalone)") {
        if display_mode_media_query.matches() {
            return true;
        }
    }

    Reflect::get(&window.navigator(), &JsValue::from_str("standalone"))
        .ok()
        .map(|value| value.is_truthy())
        .unwrap_or(false)
}

fn page_is_controlled_by_service_worker() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let service_worker_container = window.navigator().service_worker();
    Reflect::get(
        service_worker_container.as_ref(),
        &JsValue::from_str("controller"),
    )
    .ok()
    .map(|value| !value.is_null() && !value.is_undefined())
    .unwrap_or(false)
}

fn call_method_with_no_args(target: &JsValue, method_name: &str) {
    let Ok(method) = Reflect::get(target, &JsValue::from_str(method_name)) else {
        return;
    };
    let Ok(method) = method.dyn_into::<Function>() else {
        return;
    };
    let _ = method.call0(target);
}

#[component]
pub fn PwaInstallAndOfflineStatusCard() -> impl IntoView {
    let deferred_install_prompt_event = RwSignal::new(None::<JsValue>);
    let page_is_offline_ready = RwSignal::new(page_is_controlled_by_service_worker());
    let service_worker_is_active = RwSignal::new(false);
    let install_prompt_is_supported = RwSignal::new(false);

    Effect::new(move |_| {
        let Some(window) = web_sys::window() else {
            return;
        };

        let install_prompt_is_supported = install_prompt_is_supported;
        let deferred_install_prompt_event = deferred_install_prompt_event;
        let before_install_prompt_listener = Closure::<dyn FnMut(JsValue)>::new(move |event| {
            call_method_with_no_args(&event, "preventDefault");
            deferred_install_prompt_event.set(Some(event));
            install_prompt_is_supported.set(true);
        });
        let _ = window.add_event_listener_with_callback(
            "beforeinstallprompt",
            before_install_prompt_listener.as_ref().unchecked_ref(),
        );
        before_install_prompt_listener.forget();

        let service_worker_container = window.navigator().service_worker();
        let page_is_offline_ready = page_is_offline_ready;
        let controller_change_listener = Closure::<dyn FnMut(JsValue)>::new(move |_| {
            page_is_offline_ready.set(page_is_controlled_by_service_worker());
        });
        let _ = service_worker_container.add_event_listener_with_callback(
            "controllerchange",
            controller_change_listener.as_ref().unchecked_ref(),
        );
        controller_change_listener.forget();

        if let Ok(ready_promise) = service_worker_container.ready() {
            let service_worker_is_active = service_worker_is_active;
            leptos::task::spawn_local(async move {
                let _ = JsFuture::from(ready_promise).await;
                service_worker_is_active.set(true);
                page_is_offline_ready.set(page_is_controlled_by_service_worker());
            });
        }
    });

    let on_install = move |_| {
        let Some(event) = deferred_install_prompt_event.get_untracked() else {
            return;
        };
        call_method_with_no_args(&event, "prompt");
        deferred_install_prompt_event.set(None);
    };

    view! {
        <section class="mx-4 mb-6 rounded-2xl border border-teal-500/30 bg-teal-950/30 p-4">
            <div class="flex items-start justify-between gap-4">
                <div class="min-w-0">
                    <p class="text-sm font-semibold text-teal-200">"Offline App Status"</p>
                    <p class="mt-1 text-sm text-teal-50">
                        {move || {
                            if page_is_offline_ready.get() {
                                "Offline ready on this device. The installed app should reopen without network."
                            } else if service_worker_is_active.get() {
                                "Offline cache is installed, but this tab is not controlling the app yet. Reload once while online before relying on airplane mode."
                            } else {
                                "Downloading the offline shell now. Keep this page open while online, then reload once before testing without network."
                            }
                        }}
                    </p>
                </div>
                <span class="shrink-0 rounded-full border border-white/10 px-3 py-1 text-xs font-semibold uppercase tracking-[0.12em] text-white/80">
                    {move || if page_is_offline_ready.get() { "Offline Ready" } else { "Setup Needed" }}
                </span>
            </div>

            <div class="mt-4 space-y-3 text-xs text-teal-100/80">
                {move || {
                    if is_running_standalone() {
                        view! {
                            <p>"Installed app mode detected."</p>
                        }.into_any()
                    } else if is_android_browser() && deferred_install_prompt_event.get().is_some() {
                        view! {
                            <>
                                <p>"Android install is available from this page."</p>
                                <button
                                    class="min-h-[44px] rounded-xl bg-teal-400 px-4 py-2 text-sm font-semibold text-slate-950 transition hover:bg-teal-300"
                                    on:click=on_install
                                >
                                    "Install App"
                                </button>
                            </>
                        }.into_any()
                    } else if is_android_browser() {
                        view! {
                            <p>
                                {if install_prompt_is_supported.get() {
                                    "If Chrome dismissed the install dialog, open the browser menu and use Install app or Add to Home screen."
                                } else {
                                    "On Android Chrome, open the browser menu and use Install app or Add to Home screen."
                                }}
                            </p>
                        }.into_any()
                    } else if is_ios_browser() {
                        view! {
                            <p>
                                "On iPhone or iPad, use Share → Add to Home Screen after this page reaches Offline Ready."
                            </p>
                        }.into_any()
                    } else {
                        view! {
                            <p>"Install support depends on the browser, but Offline Ready is the signal that the app shell is cached."</p>
                        }.into_any()
                    }
                }}
            </div>
        </section>
    }
}
