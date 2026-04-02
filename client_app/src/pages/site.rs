use crate::meta::use_page_meta;
use crate::state::{apply_dark_mode, AppContext};
use leptos::prelude::*;
use leptos_router::components::A;

#[component]
pub fn LandingPage() -> impl IntoView {
    use_page_meta(
        "PC Player Picker",
        "Offline-first player ranking and active-learning match scheduling for team sessions.",
    );

    let feature_cards = [
        (
            "Offline-first coach flow",
            "Run setup, scheduling, scoring, and rankings on one device. No network required.",
        ),
        (
            "Batch scheduling",
            "Generate rounds in chunks when you need to assign two or three games ahead of time to keep a steady flow.",
        ),
        (
            "Uncertainty-aware rankings",
            "Show rank intervals, not just a single ladder, so close results stay visibly uncertain.",
        ),
        (
            "Assistant and player links",
            "Publish a session when you want score-entry helpers or simple player itinerary pages.",
        ),
    ];

    let workflow = [
        "Create a session with team size, players, scheduling frequency, and field count.",
        "Generate matchups for the next batch, then collect goals scored per player.",
        "Update rankings, uncertainty intervals, and confidence estimates as results come in.",
        "Open deeper analysis for overall skill, attack/defense/teamwork, and synergy.",
    ];

    view! {
        <SiteShell>
            <section class="relative overflow-hidden border-b border-white/10">
                <div class="site-hero-backdrop absolute inset-0 bg-[radial-gradient(circle_at_top_left,_rgba(45,212,191,0.18),_transparent_34%),radial-gradient(circle_at_top_right,_rgba(251,191,36,0.12),_transparent_28%),linear-gradient(180deg,_rgba(15,23,42,0.98),_rgba(2,6,23,1))]"></div>
                <div class="relative mx-auto max-w-6xl px-5 py-16 sm:px-8 sm:py-24">
                    <div class="grid gap-10 lg:grid-cols-[1.3fr_0.9fr] lg:items-center">
                        <div class="max-w-3xl">
                            <div class="inline-flex items-center gap-2 rounded-full border border-teal-400/20 bg-teal-400/10 px-3 py-1 text-xs font-semibold uppercase tracking-[0.22em] text-teal-200">
                                <span class="h-2 w-2 rounded-full bg-teal-300"></span>
                                "Rust Powered: PWA + Static Site"
                            </div>
                            <h1 class="mt-6 max-w-2xl text-5xl font-black tracking-[-0.04em] text-white sm:text-6xl">
                                "Schedule better matches. Rank players, with uncertainty shown."
                            </h1>
                            <p class="mt-5 max-w-2xl text-lg leading-8 text-slate-300">
                                "PC Player Picker is built for coaches running repeat small-sided games to evaluate players. It can also be used for quick, flexible tournaments."
                            </p>
                            <div class="mt-8 flex flex-col gap-3 sm:flex-row">
                                <A
                                    href="/coach"
                                    attr:class="inline-flex min-h-[52px] items-center justify-center rounded-2xl bg-teal-400 px-6 text-sm font-bold uppercase tracking-[0.14em] text-slate-950 transition hover:bg-teal-300"
                                >
                                    "Launch App"
                                </A>
                                <A
                                    href="/tutorial"
                                    attr:class="inline-flex min-h-[52px] items-center justify-center rounded-2xl border border-white/15 bg-white/5 px-6 text-sm font-semibold uppercase tracking-[0.14em] text-white transition hover:border-white/30 hover:bg-white/10"
                                >
                                    "Read Tutorial"
                                </A>
                            </div>
                            <div class="mt-8 grid gap-3 sm:grid-cols-3">
                                <HeroStat value="100% Offline" label="Coach mode works without network"/>
                                <HeroStat value="Goal totals" label="Per-player input, not just wins"/>
                                <HeroStat value="Installable" label="Add to home screen on any device"/>
                            </div>
                        </div>

                        <div class="rounded-[28px] border border-white/10 bg-slate-900/80 p-5 shadow-[0_30px_80px_rgba(0,0,0,0.45)] backdrop-blur">
                            <div class="rounded-[24px] border border-white/10 bg-slate-950/80 p-5">
                                <div class="flex items-center justify-between">
                                    <div>
                                        <p class="text-xs font-semibold uppercase tracking-[0.2em] text-slate-500">"Coach Session"</p>
                                        <p class="mt-2 text-2xl font-bold text-white">"2v2 Soccer"</p>
                                    </div>
                                    <div class="rounded-full border border-emerald-400/25 bg-emerald-400/10 px-3 py-1 text-xs font-semibold text-emerald-200">
                                        "Round 4"
                                    </div>
                                </div>
                                <div class="mt-5 grid gap-3">
                                    <ScoreStrip label="Field 1" left="Kira + Maya" right="Noah + Eli" accent="teal"/>
                                    <ScoreStrip label="Field 2" left="Rae + Omar" right="Jules + Theo" accent="amber"/>
                                </div>
                                <div class="mt-5 rounded-2xl border border-white/10 bg-slate-900/70 p-4">
                                    <div class="flex items-center justify-between text-xs uppercase tracking-[0.2em] text-slate-500">
                                        <span>"Ranking spread"</span>
                                        <span>"90%"</span>
                                    </div>
                                    <div class="mt-4 space-y-3">
                                        <RankStrip name="Kira" lane="1-3" width="72%" offset="4%"/>
                                        <RankStrip name="Maya" lane="2-4" width="65%" offset="14%"/>
                                        <RankStrip name="Noah" lane="3-6" width="58%" offset="24%"/>
                                    </div>
                                </div>
                            </div>
                        </div>
                    </div>
                </div>
            </section>

            <section class="mx-auto max-w-6xl px-5 py-16 sm:px-8">
                <div class="flex items-end justify-between gap-6">
                    <div class="max-w-2xl">
                        <p class="text-xs font-semibold uppercase tracking-[0.22em] text-teal-200">"Features"</p>
                        <h2 class="mt-3 text-3xl font-bold tracking-[-0.03em] text-white">"Built for the sideline"</h2>
                    </div>
                    <A href="/faq" attr:class="hidden text-sm font-semibold text-slate-300 transition hover:text-white sm:inline-flex">
                        "View FAQ"
                    </A>
                </div>
                <div class="mt-8 grid gap-4 md:grid-cols-2 xl:grid-cols-4">
                    {feature_cards.into_iter().map(|(title, body)| view! {
                        <FeatureCard title=title body=body/>
                    }).collect_view()}
                </div>
            </section>

            <section class="border-y border-white/10 bg-slate-900/45">
                <div class="mx-auto max-w-6xl px-5 py-16 sm:px-8">
                    <div class="grid gap-8 lg:grid-cols-[0.9fr_1.1fr]">
                        <div>
                            <p class="text-xs font-semibold uppercase tracking-[0.22em] text-amber-200">"How it runs"</p>
                            <h2 class="mt-3 text-3xl font-bold tracking-[-0.03em] text-white">"Designed for the coach workflow first"</h2>
                            <p class="mt-4 max-w-xl text-base leading-7 text-slate-300">
                                "The app defaults to an offline session on the coach device. Online syncing and public links are layered on top instead of being required."
                            </p>
                        </div>
                        <div class="space-y-3">
                            {workflow.into_iter().enumerate().map(|(index, item)| view! {
                                <div class="flex gap-4 rounded-2xl border border-white/10 bg-slate-950/60 p-4">
                                    <div class="flex h-10 w-10 shrink-0 items-center justify-center rounded-2xl bg-white/5 text-sm font-black text-amber-200">
                                        {index + 1}
                                    </div>
                                    <p class="pt-1 text-base leading-7 text-slate-200">{item}</p>
                                </div>
                            }).collect_view()}
                        </div>
                    </div>
                </div>
            </section>

            <section class="mx-auto max-w-6xl px-5 py-16 sm:px-8">
                <div class="grid gap-4 lg:grid-cols-3">
                    <RoleCard
                        title="Coach"
                        body="Creates sessions, generates schedules, enters corrections, and owns the source of truth."
                    />
                    <RoleCard
                        title="Assistant"
                        body="Gets a lighter score-entry view and live rankings through a shared link when online sync is enabled."
                    />
                    <RoleCard
                        title="Player"
                        body="Sees a stripped-down itinerary page with upcoming fields and opponents."
                    />
                </div>
            </section>
        </SiteShell>
    }
}

#[component]
pub fn TutorialPage() -> impl IntoView {
    use_page_meta(
        "Tutorial · PC Player Picker",
        "Quick-start guide for creating a session, entering scores, and reading rankings.",
    );

    let steps = [
        (
            "Create the session",
            "Choose team size, player count, scheduling frequency, and optional match duration. Player names can stay simple: numbers, initials, or jersey IDs all work.",
        ),
        (
            "Add enough players for the format",
            "The setup flow enforces the minimum for two full sides. Bench counts and approximate field needs update as you adjust the pool.",
        ),
        (
            "Generate the next batch",
            "Use the Matches tab to build the current round. If you schedule every 2 or 3 rounds, the app can hand you a short batch instead of stopping after each game.",
        ),
        (
            "Enter per-player goals",
            "Results default to did-not-play instead of zero. That keeps inactive or substituted players from being misread as scoreless participants.",
        ),
        (
            "Explore analysis and export",
            "The Analysis tab has three sub-tabs: Overall rankings with uncertainty intervals, A/D/T for attack, defense, and teamwork breakdowns, and Synergy for pairing effects. Download a CSV of rankings from the Overall sub-tab when you need a shareable snapshot.",
        ),
        (
            "Publish online only when needed",
            "Go online from the coach dashboard to create assistant and player links. Set a recovery PIN to restore the session on another device if you lose local data. The local coach flow remains usable if the network disappears.",
        ),
    ];

    view! {
        <SiteShell>
            <section class="mx-auto max-w-4xl px-5 py-16 sm:px-8 sm:py-20">
                <div class="max-w-3xl">
                    <p class="text-xs font-semibold uppercase tracking-[0.22em] text-teal-200">"Tutorial"</p>
                    <h1 class="mt-3 text-4xl font-black tracking-[-0.04em] text-white sm:text-5xl">
                        "Run a session from setup to analysis."
                    </h1>
                    <p class="mt-5 text-lg leading-8 text-slate-300">
                        "This flow is aimed at the real sideline use case: one coach device first, optional assistants second."
                    </p>
                </div>

                <div class="mt-10 space-y-4">
                    {steps.into_iter().enumerate().map(|(index, (title, body))| view! {
                        <div class="rounded-[24px] border border-white/10 bg-slate-900/60 p-6">
                            <div class="flex items-start gap-4">
                                <div class="flex h-11 w-11 shrink-0 items-center justify-center rounded-2xl bg-teal-400/10 text-sm font-black text-teal-200">
                                    {index + 1}
                                </div>
                                <div>
                                    <h2 class="text-xl font-bold text-white">{title}</h2>
                                    <p class="mt-3 leading-7 text-slate-300">{body}</p>
                                </div>
                            </div>
                        </div>
                    }).collect_view()}
                </div>

                <div class="mt-10 grid gap-4 md:grid-cols-2">
                    <InfoPanel
                        title="Corrections stay easy"
                        body="The coach dashboard already exposes player swaps, match voiding, and partial-duration score entry. Those flows matter more than raw model complexity because bad data arrives in real sessions."
                    />
                    <InfoPanel
                        title="Install on mobile"
                        body="Open the site once, then use Add to Home Screen on iPhone/iPad or Install in Chrome-based browsers. The manifest and service worker provide the shell needed for that flow."
                    />
                </div>
                <div class="mt-12 flex justify-center">
                    <A
                        href="/coach"
                        attr:class="inline-flex min-h-[52px] items-center justify-center rounded-2xl bg-teal-400 px-8 text-sm font-bold uppercase tracking-[0.14em] text-slate-950 transition hover:bg-teal-300"
                    >
                        "Launch App"
                    </A>
                </div>
            </section>
        </SiteShell>
    }
}

#[component]
pub fn FaqPage() -> impl IntoView {
    use_page_meta(
        "FAQ · PC Player Picker",
        "FAQ covering privacy, offline behavior, rankings, substitutions, and support for different sports.",
    );

    let faqs = [
        (
            "Does the coach app work offline?",
            "Yes. Session setup, local persistence, scheduling, results entry, and ranking analysis are designed to work on the coach device without a network connection.",
        ),
        (
            "How do I install it for offline use?",
            "Open the app in your mobile browser, then use the browser install action (Install app / Add to Home Screen). After installing, launch from the home screen for the most reliable offline use.",
        ),
        (
            "What does 'going online' mean here?",
            "Going online means registering the session with the host server by clicking the 'Go Online' button. This enables features like recovery PINs and sharing session data by URL with Assistants and Players. Without clicking the go online button, the session data stays local to the coach device and won't be accessible from other devices or browsers. Going online is optional, other means of sharing a session are enabled, such as exporting csvs and copy-paste match schedules ready to share in group chat apps",
        ),
        (
            "Can I remove an injured or unavailable player?",
            "Yes. Players can remain in the reports while being marked inactive so they stay out of future scheduling without erasing completed matches.",
        ),
        (
            "Can I export rankings?",
            "Yes. The Analysis tab has a Download Rankings CSV button. You can also import a previously saved CSV to overlay historical rankings in the Overall sub-tab.",
        ),
        (
            "Can this be used for teams rather than individual players?",
            "Yes. Instead of entering player names, you can enter team names and track just team performance, setting up in 1v1 format.",
        ),
        (
            "How accurate are the rankings and synergy scores?",
            "While they provide a useful indication of performance, these measures lack insight into many confounding real world factors. Adding more matches improves accuracy but introduces other tradeoffs like increased fatigue. These numbers may anchor judgements but should never be the exclusive judgement of players.",
        ),
        (
            "What is Bayesian active scheduling, and why isn't it used more widely?",
            "Traditional brackets were much easier to organize in the pen and paper era. Now, Bayesian rankings can deliver better quality rankings in fewer rounds, making this better for skills determination, with new schedules distributed in seconds across devices. Brackets remain useful for for major ticketed events (world cups) where brackets provide predictable, fan-understandable storylines. Here, in setup, users can choose to create rounds in batches, if they want a more predictable flow with fewer schedule updates.",
        ),
        (
            "What about privacy?",
            "Use initials, nicknames, or jersey numbers if you want less identifying data stored locally. The app is designed to keep the heavy lifting on the coach device, and online sharing is optional.",
        ),
        (
            "What is the recovery PIN for?",
            "When you go online, you should set a short PIN on the session. If you switch devices or lose local data, entering the session ID and PIN pulls the full event log back from the server.",
        ),
        (
            "Can I self-host this?",
            "Yes. The worker is thin and the core logic lives in Rust, so the sync layer can be replaced by another backend without touching the coach app.",
        ),
        (
            "My online session is seeing quota limits, how do I prevent this?",
            "The current hosting plan has a maximum global pool of 100000 daily online session (Cloudflare Worker) calls. That may sound like a lot, but with each schedule refresh from a player counting as a call, this can add up quickly. Quota limits should not stop usage of the main coach device. We recommend having a backup plan of sharing schedules over group chat apps. If you see this issue routinely, file an issue on GitHub (see below).",
        ),
        (
            "How do I report Bugs and Issues?",
            "You can report bugs and issues by opening an issue on the GitHub repository: https://github.com/winedarksea/pcplayerpicker/issues",
        ),
    ];

    let history_paragraphs = [
        "PC Player Picker started as the idea of Mark Catlin back in 1990, who as a soccer coach wrote a number of books (The Art of Soccer, Good N' Fun Soccer, and 'Understanding Soccer' under the pseudonym Kram Niltac). He commissioned a simple MS-DOS program from a local developer to create schedules and rank players, Player Picker for PC. Simpler than the current version, the basic idea was the same, to use 2 on 2 soccer as a more focused way to test players and get a quantitative basis for skills evaluation.",
        "Since no one uses floppy disks anymore, I wrote this updated version as a birthday present for Mark Catlin (my father). The goal remains the same now, to be an effective yet simple tool for evaluating players, now powered by Bayesian statistics and the Rust programming language. This active learning approach (adjusting schedules based on results) also makes it useful for running tournaments, the idea being a pickleball league can do an informal tournament, just playing as many games as they want in an evening without a full bracket, and the skills pairings should mean most matches are closer, more exciting matches.",
    ];

    view! {
        <SiteShell>
            <section class="mx-auto max-w-4xl px-5 py-16 sm:px-8 sm:py-20">
                <div class="max-w-3xl">
                    <p class="text-xs font-semibold uppercase tracking-[0.22em] text-amber-200">"FAQ"</p>
                    <h1 class="mt-3 text-4xl font-black tracking-[-0.04em] text-white sm:text-5xl">
                        "Questions coaches usually ask before using it live."
                    </h1>
                </div>

                <div class="mt-10 space-y-4">
                    {faqs.into_iter().map(|(question, answer)| view! {
                        <div class="rounded-[24px] border border-white/10 bg-slate-900/60 p-6">
                            <h2 class="text-xl font-bold text-white">{question}</h2>
                            <p class="mt-3 leading-7 text-slate-300">{answer}</p>
                        </div>
                    }).collect_view()}

                    <div class="rounded-[24px] border border-white/10 bg-slate-900/60 p-6">
                        <h2 class="text-xl font-bold text-white">"Where is the source code?"</h2>
                        <p class="mt-3 leading-7 text-slate-300">
                            "PC Player Picker is free and open-source. View the repository on "
                            <a
                                href="https://github.com/winedarksea/pcplayerpicker"
                                target="_blank"
                                rel="noopener noreferrer"
                                class="font-semibold text-teal-200 underline decoration-teal-200/60 underline-offset-4 transition hover:text-teal-100"
                            >
                                "GitHub"
                            </a>
                            "."
                        </p>
                    </div>

                    <div class="rounded-[24px] border border-amber-300/20 bg-gradient-to-br from-amber-300/10 to-slate-900/70 p-6">
                        <h2 class="text-xl font-bold text-white">"Project History and Motivation"</h2>
                        <div class="mt-3 space-y-4">
                            {history_paragraphs.into_iter().map(|paragraph| view! {
                                <p class="leading-7 text-slate-200">{paragraph}</p>
                            }).collect_view()}
                        </div>
                    </div>
                </div>
            </section>
        </SiteShell>
    }
}

#[component]
fn SiteShell(children: Children) -> impl IntoView {
    view! {
        <div class="site-theme min-h-screen bg-slate-950 text-white">
            <SiteNav/>
            <main>{children()}</main>
            <SiteFooter/>
        </div>
    }
}

#[component]
fn SiteNav() -> impl IntoView {
    let ctx = use_context::<AppContext>().expect("AppContext missing");
    let dark = ctx.dark_mode;

    let toggle = move |_| {
        let next = !dark.get();
        dark.set(next);
        apply_dark_mode(next);
    };

    view! {
        <header class="sticky top-0 z-40 border-b border-white/10 bg-slate-950/85 dark:bg-slate-950/85 backdrop-blur">
            <div class="mx-auto flex max-w-6xl items-center justify-between gap-4 px-5 py-4 sm:px-8">
                <A href="/" attr:class="flex items-center gap-3">
                    <div class="flex h-11 w-11 items-center justify-center rounded-2xl bg-gradient-to-br from-teal-300 to-amber-300 text-sm font-black text-slate-950">
                        "PC"
                    </div>
                    <div>
                        <p class="text-base font-bold tracking-[0.02em] text-white">"PC Player Picker"</p>
                        <p class="text-xs uppercase tracking-[0.22em] text-slate-500">"Active Learning Sessions"</p>
                    </div>
                </A>
                <div class="flex items-center gap-2 md:hidden">
                    <button
                        on:click=toggle
                        title="Toggle dark/light mode"
                        class="flex h-9 items-center justify-center rounded-full border border-white/15 bg-white/5 px-3 text-xs font-semibold text-slate-300 transition hover:border-white/30 hover:bg-white/10 hover:text-white"
                    >
                        {move || if dark.get() { "Light" } else { "Dark" }}
                    </button>
                    <A href="/coach" attr:class="inline-flex min-h-[40px] items-center rounded-full bg-white px-3 text-xs font-semibold uppercase tracking-[0.14em] text-slate-950 transition hover:bg-slate-100">
                        "Open App"
                    </A>
                </div>
                <nav class="hidden items-center gap-5 text-sm font-semibold text-slate-300 md:flex">
                    <A href="/tutorial" attr:class="transition hover:text-white">"Tutorial"</A>
                    <A href="/faq" attr:class="transition hover:text-white">"FAQ"</A>
                    // Dark/light mode toggle
                    <button
                        on:click=toggle
                        title="Toggle dark/light mode"
                        class="flex h-9 items-center justify-center rounded-full border border-white/15 bg-white/5 px-3 text-xs font-semibold text-slate-300 transition hover:border-white/30 hover:bg-white/10 hover:text-white"
                    >
                        {move || if dark.get() { "Light" } else { "Dark" }}
                    </button>
                    <A href="/coach" attr:class="inline-flex min-h-[44px] items-center rounded-full bg-white px-4 text-slate-950 transition hover:bg-slate-100">
                        "Open App"
                    </A>
                </nav>
            </div>
            <div class="mx-auto flex max-w-6xl items-center gap-5 px-5 pb-3 text-xs font-semibold uppercase tracking-[0.14em] text-slate-400 md:hidden sm:px-8">
                <A href="/tutorial" attr:class="transition hover:text-white">"Tutorial"</A>
                <A href="/faq" attr:class="transition hover:text-white">"FAQ"</A>
            </div>
        </header>
    }
}

#[component]
fn SiteFooter() -> impl IntoView {
    view! {
        <footer class="border-t border-white/10 bg-slate-950">
            <div class="mx-auto flex max-w-6xl flex-col gap-5 px-5 py-8 text-sm text-slate-400 sm:px-8 md:flex-row md:items-center md:justify-between">
                <div>
                    <p class="font-semibold text-slate-200">"PC Player Picker"</p>
                    <p class="mt-1">"Offline-first scheduling and ranking for matches, tryouts, and tournaments."</p>
                </div>
                <div class="flex flex-wrap items-center gap-4">
                    <A href="/" attr:class="transition hover:text-white">"Home"</A>
                    <A href="/tutorial" attr:class="transition hover:text-white">"Tutorial"</A>
                    <A href="/faq" attr:class="transition hover:text-white">"FAQ"</A>
                    <A href="/coach" attr:class="transition hover:text-white">"Coach App"</A>
                    <a href="https://github.com/winedarksea/pcplayerpicker" target="_blank" rel="noopener noreferrer" class="transition hover:text-white">"GitHub"</a>
                </div>
            </div>
        </footer>
    }
}

#[component]
fn HeroStat(value: &'static str, label: &'static str) -> impl IntoView {
    view! {
        <div class="rounded-2xl border border-white/10 bg-white/5 px-4 py-4">
            <p class="text-2xl font-black tracking-[-0.03em] text-white">{value}</p>
            <p class="mt-1 text-sm leading-6 text-slate-400">{label}</p>
        </div>
    }
}

#[component]
fn FeatureCard(title: &'static str, body: &'static str) -> impl IntoView {
    view! {
        <div class="rounded-[24px] border border-white/10 bg-slate-900/60 p-5">
            <h3 class="text-lg font-bold text-white">{title}</h3>
            <p class="mt-3 leading-7 text-slate-300">{body}</p>
        </div>
    }
}

#[component]
fn RoleCard(title: &'static str, body: &'static str) -> impl IntoView {
    view! {
        <div class="rounded-[24px] border border-white/10 bg-slate-900/60 p-6">
            <p class="text-lg font-bold text-white">{title}</p>
            <p class="mt-3 leading-7 text-slate-300">{body}</p>
        </div>
    }
}

#[component]
fn InfoPanel(title: &'static str, body: &'static str) -> impl IntoView {
    view! {
        <div class="rounded-[24px] border border-white/10 bg-slate-900/60 p-6">
            <h2 class="text-xl font-bold text-white">{title}</h2>
            <p class="mt-3 leading-7 text-slate-300">{body}</p>
        </div>
    }
}

#[component]
fn ScoreStrip(
    label: &'static str,
    left: &'static str,
    right: &'static str,
    accent: &'static str,
) -> impl IntoView {
    let accent_class = match accent {
        "amber" => "border-amber-300/20 bg-amber-300/10 text-amber-100",
        _ => "border-teal-300/20 bg-teal-300/10 text-teal-100",
    };

    view! {
        <div class="rounded-2xl border border-white/10 bg-slate-900/60 p-4">
            <div class="flex items-center justify-between gap-4">
                <div>
                    <p class="text-xs font-semibold uppercase tracking-[0.2em] text-slate-500">{label}</p>
                    <p class="mt-2 text-sm font-semibold text-white">{left}</p>
                    <p class="mt-1 text-sm font-semibold text-slate-400">{right}</p>
                </div>
                <div class=format!("rounded-full border px-3 py-1 text-xs font-semibold uppercase tracking-[0.16em] {accent_class}")>
                    "ready"
                </div>
            </div>
        </div>
    }
}

#[component]
fn RankStrip(
    name: &'static str,
    lane: &'static str,
    width: &'static str,
    offset: &'static str,
) -> impl IntoView {
    view! {
        <div class="grid grid-cols-[72px_1fr_38px] items-center gap-3">
            <span class="text-sm font-semibold text-slate-200">{name}</span>
            <div class="relative h-6 rounded-full bg-slate-800">
                <div class="absolute inset-y-1 rounded-full bg-gradient-to-r from-teal-300 to-cyan-300" style=format!("left:{offset}; width:{width};")></div>
            </div>
            <span class="text-right text-xs font-semibold text-slate-400">{lane}</span>
        </div>
    }
}
