# Changelog

## 0.0.3

- `fetch_url` blocks private, loopback, link-local and CGNAT addresses, also via redirects (max 5) and DNS, and ignores system proxies.
- Fetched pages are marked as untrusted for researchers.
- `rsc update` runs the installer from the release tag; `install.sh` is attached to releases.
- New `max_run_tokens` / `max_run_cost_usd` run budget, with an estimate on the Review screen.
- Release notes come from this file.
- Dependabot keeps Cargo dependencies and GitHub Actions up to date.
- A panic in a background task no longer poisons UI state.

## 0.0.2

- Follow-up chat on a finished report.
- Report style setting.
- Incognito mode removed.

## 0.0.1

- Install script, release workflow and update checker.
- Planner clarification loop, live agent inspector and stage tracker.
- Sources tab with numbered citations.
- Automatic report length; up to 99 agents.
