Overall purpose: a progressive web app that setups a schedule for 2 on 2 soccer matches for ranking players (designed for soccer players, but should be extensible to chess matches, basketball, etc) and progressively refines a ranking through a bayesian active learning process as matches are completed, updating the schedule with the next rounds matchups and updating the rankings with the latest results.

App Behavior:
Session Setup:
Set size, how many players on teams (1x1, 2x2, up to 11x11), with 2 on 2 the default.
Set number of players, enter names (defaults to ID numbers from 1, but names can be added and numbers replaced as lost as they remain unique)
Set how often scheduling is done (every round, every 3rd round, etc) as it may be convenient to assign games in batches of 2 or 3, rather than having to wait to collect results and run a new schedule for every game.
Provide an estimated number of rounds needed for a certain degree of ranking confidence
Provide how many fields/spaces are needed
Collected data: the points per participant. So in 2x2 soccer, how many goals each of the 4 players scored. Goals scored per player is the primary data collection.
Future extension: optional collected data (hidden 'advanced input' option) that does not contribute to the main ranking, but provides extra data if enough assistants are present to collect it: Conquered Balls (CB), Received Balls (RB), Lost Balls (LB), Attacking Passes (perhaps a simple up click to allow assistants to just tap up each time they see one for each of the players in the game they are overseeing).

Deploy an Active Learning Matchmaking Architecture. Pausing after a designated batch of matches, possibly calculating expected reduction in posterior ranking uncertainty, measured by pairwise order entropy or expected Kendall-type ranking loss. The system aims for every match played to reduce the ranking uncertainty of the player pool.
For multiple rounds scheduled in advance, perhaps use a receding-horizon approach.
Utilize a Bayesian Ranking Engine. The primary ranking should answer “how much does this player increase their team’s chance of outscoring the opponent?” This may use negative binomials. A warm-started Laplace or variational posterior is probably the right balance.
In addition to the primary "team" based player skill ranking, implement a separate trivariate regression for Attack/Defense/Teamwork (one measure for goal scoring, one measure for suppressing attacking goals, one measure for assisting teammates in increased scoring, assisting one only provided in 2x2 and greater, not 1x1 matches). Apply heavy Bayesian priors (regularization) pulling everyone toward the mean, so one lucky game doesn't make the algorithm think a player has a god-tier defensive rating.
Calculate Synergy (which players play well together), possibly via Regularized Adjusted Plus-Minus to isolate non-linear interaction terms between players, outputting a synergy matrix that informs full-squad tactical deployments.
Shortened match multiplier - an optional (generally expected to be unused) multiplier for matches that were shorter or longer than the standard fixed match length (where applicable)
Rankings update as soon as data is available.
The goal is to aim for high quality rankings and scheduling while also aiming to use fast efficient algorithms.
Rankings and schedule generation should be designed to be modular, so that different algorithms can be swapped our or chosen. Future versions will likely give the coach the option to use different algorithms for the ranking and scheduler when they setup a session.

Include players that are unable to continue (drop out due to injury) in the rankings, but marked clearly separately (perhaps two rankings provided, one of all who completed, one with those who dropped out early, if such are present, with the greater uncertainty). To support this, all matches should default to "did not play" (not a zero score) and an option to remove players from further scheduled rounds (but does not remove them from completed rounds).
Need to be able to substitute/swap players in results (in case a match accidentally started with the wrong players or for some other reason the pairings needed to be changed). Perhaps only on the coach app.

Treat dropouts as a first-class workflow:
* active / inactive badge
* ranking “as of last match played” 
* uncertainty widens after inactivity 
* removed from future scheduling but retained in reports

References:
True Skill, True Skill 2, OpenSkill
https://github.com/vivekjoshy/openskill.py
https://openreview.net/forum?id=UZZaWUR0n4

Technical Implementation:
The goal is a very lightweight app that does most of the heavy lifting on the client side, aiming for minimal serving compute to support a large number of sessions cheapily.

Rust as progressive web app, abstraction layer for SQLite locally or Cloudflare D1 as storage. Rust app should be frontend, core (scheduling and math), and backend (mainly just passing the data, some quick checks).
Persistent OPFS storage for app on device, so it stores permanently on Apple devices as well.
Main deployment is cloudflare workers and cloudflare D1, but the goal is to be able to relatively easily swap this to a VM with local SQLite if desired in the future.
Seeded random number shared across a session (just use the session id as this). Explicit reseed option for coach (maybe recorded and added as a +1 to the session id) on first creation.

Rust in cloudflare workers: https://developers.cloudflare.com/workers/languages/rust/
https://github.com/cloudflare/workers-rs
This example in particular looks to be key: https://github.com/cloudflare/workers-rs/tree/main/templates/leptos
Likely Leptos plus Tailwind CSS, perhaps DaisyUI

There are 3 "roles" for users. No user accounts are created or managed, but still user 'types' exist.
Coach - can manage all admin details of a particular session.
Assistants - can enter scores and see rankings live (arcade/kiosk style)
Players - can only view schedule (minimalist itinerary style)
Players can get a simpler UI view that shows just their schedule and which fields they are on
Last Write wins (for equals). Append-only event logs.
Coach wins over assistants (if different scores entered)
Pin login optional for assistants and players (separately)
Pin login to retrieve coach session ('create pin'), allowing coach sessions to be loaded from another device if the main device runs out of power. Heartbeat list of devices that have logged into that session as a coach and their last refresh.

The coach role is the 'main' landing role for the app unless accessing with a link (links/QR codes are distributed for a particular session for players and assistants to access)
The app should entirely work offline in the coach role (schedule setup, rankings generated, results entered). Creating an online session for assistants and players to access on their own devices is optional. Almost all of the computation is done on the coach device. 

Make correction flows extremely easy
* swap players in a schedule, swap in a completed match 
* void a match 
* mark partial match (shorter / longer)

Putting session analysis (deep analysis that is) in a separate tab might make that loadable from csv, with a 'load from session' or 'load from csv' option
Likely there are four main sections for a coach view: configure matches, setup online session (optional), enter results and run matches, then analyze rankings/results

Need to be able to have a backend view of a history of all sessions. Likely a secret variable key that allows a simple dashboard to be viewed, from which csv files can be downloaded of session history
Maybe allow upload of history too to allow migration via csv (not necessarily for all details).

Allow download of session results as appropriate for coaches for their particular session

The visualization for rankings will need to be high quality to clearly show the uncertainty of the rankings. Ranking spread (ie 2nd to 11th for a given player) is perhaps the easiest way to show uncertainty of rankings (say at 90%). Also elements like posterior mean skill, probability of top K, are potentially useful. Ideally a ranking could be shown visually separated by uncertainty spread, showing which are very close and which are clearly separated.
This might be a rank-lane view or rank interval ladder (Use rank distributions, not skill intervals mapped loosely to ranks). Separately a table could report the raw statistics for each player

Static pages (perhaps as Cloudflare Workers Static Assets):

Landing Page (brief intro, links to tutorial, faq, and app)

Tutorial Page

FAQ page:
Basic intro, pointing to coaching books
Algorithm info - note that it does not account for all variables
Data saving notes - nicknames, abbreviations, jersey numbers all encouraged for privacy, deletion occurs when local storage full. We do not sell your data. For maximum privacy, considering self-hosting your own local version. Full software is free and (relatively) easy to use at github.

Include lightweight Google Analytics, and configure the app for SEO and for advertising it in search results (not advertising on the app, just promoting of the apps website).

Support a dark mode for the app
Follow the design language used in modern fitness apps like Strava, Apple Health, or professional timing software. It prioritizes data readability and fast interactions over decorative flair. Generally use higher contrast typography and color schemes. Apps may be used by touch screens, so elements should be touch friendly.

The domain owned for this project is pcplayerpicker.com

This note was present in a blog on details to keep in mind for Cloudflare deployment:
Leptos server binaries can get chunky. You will need to heavily optimize your release builds using lto = true, opt-level = 'z' (optimize for size), and use tools like wasm-opt to strip dead code so your Worker deploys successfully (3MB compressed for free tier, up to 10MB for paid, separately asset files up to 25 MiB).
You must ensure whatever linear algebra or math crates you use for your Bayesian logic (like nalgebra or ndarray) can compile to no_std or basic WASM without relying on C-bindings (like BLAS/LAPACK), as those won't compile to Cloudflare Workers
No multithreading: You cannot use std::thread or crates like Rayon in your Worker or your Leptos client. (WASM multithreading exists but is highly experimental and not supported in Workers).
No file system: You cannot use std::fs. (OPFS handles this on the Leptos client, and D1/KV handles this on the Worker).
Cloudflare Workers have strict CPU time limits (10ms on the free tier, 50ms on the paid Unbound tier). Worker merely acts as a lightweight API router. It receives the calculated results, validates them, and writes them to D1.

Proposed design for cloudflare workers deployment without vendor lockin and staying within the very lightweight requirements of the free tier:
Keep your UI, math, and data models in app_core (possibly separate into two an app_core and client_app)
Keep server_cloudflare as thin as possible—it should only contain your D1 database implementation, the worker::fetch entry point, and the code to inject the DB into the Leptos context, plus a few small validations
Run your deployment from the server_cloudflare directory: cd server_cloudflare && npx wrangler deploy.

There should be a unittest for full simulation of the offline functionality (generate schedule, enter results, update schedule, calculate ranking, etc)
Include a test that the cloudflare bundle for the worker, compressed, is less than 3MB

GitHub Actions CI/CD for Cloudflare: https://developers.cloudflare.com/workers/ci-cd/external-cicd/github-actions/ and https://github.com/cloudflare/wrangler-action

### Remaining (not blocking launch)
- wrangler.toml database_id is account-specific — document in README for anyone forking the project
- Admin dashboard UI — /api/admin/sessions returns JSON; a visual dashboard is future work
- Full SQLite/OPFS normalized query layer — app_core/db/queries.rs schema is ready; wiring it to a SQLite WASM runtime is future work (IDB + OPFS file backup covers the data-loss risk for now)
- Low: dashboard.rs (~2300 LOC) and worker lib.rs (~1100 LOC) are large — refactor candidates for a future pass, not blocking launch
