# pcplayerpicker.com Agent Guidelines

### Small Files
- **Aim to keep files < 500 lines.**

### Hyper-Descriptive Naming
- **Favor Explicit Over Concise:** Use long, descriptive names that explain intent

### High-Signal Comments
- **Explain "Why", Not "What":** Comments should explain the reasoning behind complex algorithms or architectural decisions.
- **Be Token-Efficient in Comments:** Use concise, informative language. Focus on documenting interface contracts and capability tiers.

### Testing & Benchmarking
- **Full Offline Simulation:** tests should make sure the offline app functionality is fully tested end-to-end

### Dependencies
- **Latest Versions** Build for latest versions (for example, for iOS 26, Chromium >= 140.x, Android 16), legacy version support is not important here.

### Production Targets
- **Cloudflare Workers** The goal is for the client app (coach) to do the compute, not the cloud server, and be able to do so fully offline, efficiently. Cloudflare workers need to remain small (< 3 MiB) and very fast (< 10 ms) and aim to keep api calls minimal.
- **Portability:** The goal is that this library could be ported off Cloudflare, to a VM and local SQLite, easily.
