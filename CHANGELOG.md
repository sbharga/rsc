# Changelog

## 0.1.0

- Citation checking: cited links are probed (`check_links`, on by default) and dead ones are flagged; cited URLs no agent opened are marked model-supplied; `[n]` numbers missing from the Sources list are recorded; opt-in `verify_citations` / `rsc run --verify` checks cited pages against the claims that cite them (`v` on the Sources tab).
- Planner: `default_agents = 0` (new default) lets the planner choose 2–8 angles; `--agents auto`; the Review screen and `rsc run` warn about overlapping angles.
- Hierarchical synthesis: more than 8 finished reports are merged in groups before the final synthesis.
- Opt-in follow-up round (`gap_round`): a reviewer proposes up to 4 more researchers; approve or skip on the Review screen, with `--yes`, or at the prompt. New run status `gap review`: older binaries cannot read records saved in that status.
- `rsc run --json` adds `report_words`, `agents_failed`, `gap_agents`, `config` and `citations`.
- `evals/`: 20 fixed questions and `python3 evals/eval.py run|diff` to record and compare results across commits. Spends plan usage; run by hand.

## 0.0.4

- Headless commands: `rsc run`, `rsc resume`, `rsc ls` and `rsc show`, with `--yes`, `--no-clarify`, `-o` and `--json`. Exit codes: 0 done, 1 failed, 2 budget hit, 130 cancelled.
- Researchers checkpoint their progress after every turn; resume continues where an agent stopped.
- An agent restarts once after a retryable failure (at most 3 restarts per run).
- `RSC_API_BASE` and `RSC_BACKOFF_MS` environment overrides for testing against a mock API.
- End-to-end tests drive the real binary against a scripted mock API.
- `research/mod.rs` and `ui/app.rs` split into smaller modules.

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
