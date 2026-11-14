# Roadmap

State at v0.1.0 (pre-release): the pipeline (plan → clarify → review → research → synthesize), live agent timelines, source tracking with `[n]` citations, follow-up chat, resume, history, cost estimates, installer and self-update all work. Phase 0 (safety and trust) shipped in 0.0.3: `fetch_url` is SSRF-guarded, the updater is pinned to the release tag, and runs can be capped by tokens or cost. Phase 1 (foundations) shipped in 0.0.4: headless `rsc run`/`resume`/`ls`/`show`, a mock API with end-to-end tests, agent checkpoints and restarts, and the large files split. Single binary, Linux and macOS, ChatGPT sign-in only. Phase 2 (report quality) is implemented in 0.1.0: eval harness, citation checks, auto agent count with overlap warnings, hierarchical synthesis and an opt-in follow-up round. The live eval baseline and planner-prompt tuning against it are still to do.

Phase 3 (workflow) is implemented in 0.2.0 (pre-release): export, history search and sort, presets, editable prompts, local context and chat deep-dives.

Phase 4 (distribution) is implemented in 0.3.0 (pre-release): rustls on ring (no cmake), static musl Linux builds, Windows builds with `install.ps1`, and cosign-signed release assets.

This document orders the next work by when it should be done and why. Order follows three rules:

1. **Trust before features.** Fix anything that can leak data or burn the user's plan before adding capability.
2. **Build the measuring stick before tuning.** Output quality can't be improved if it can't be measured, and nothing can be measured without a headless run path and a mock API.
3. **Refactor only when the next feature needs it.** Split a file at the moment a phase would otherwise make it worse.

Sizes: **S** ≤ 1 day, **M** 2–5 days, **L** > 1 week. Phases are ordered; items inside a phase are ordered too. Version numbers are targets, not promises.

## Summary

| # | Phase | Target | Theme | Gate to leave the phase |
|---|---|---|---|---|
| 0 | Safety and trust | 0.0.3 (done) | Close known holes | `fetch_url` cannot reach private addresses; updater pinned to the release tag; run budget cap works |
| 1 | Foundations | 0.0.4 (done) | Testability, headless mode, resilience | A full run is exercised in `cargo test` against a mock API; `rsc run` works with no TTY |
| 2 | Report quality | 0.1.0 (pre-release) | Make the output measurably better | Eval suite shows citation validity and coverage improving release over release |
| 3 | Workflow | 0.2.0 (pre-release) | Export, search, presets, local context | Users can get a report out of rsc and into other tools without copy-paste |
| 4 | Distribution | 0.3.0 (pre-release) | Windows, packaging, supply chain | Install works without a C toolchain; signed artifacts |
| 5 | Extensibility | later | Providers, tools, knowledge reuse | Decide after real usage data |

---

## Phase 0 — Safety and trust (0.0.3) — done

Shipped in 0.0.3; see `CHANGELOG.md`.

| Item | What shipped |
|---|---|
| 0.1 Private addresses in `fetch_url` | `is_blocked` classifier (IPv4, IPv6, IPv4-mapped), guarded DNS resolver, redirect policy capped at 5, system proxies ignored. Tests cover the ranges and redirects to loopback against a local server. `fetch_url_enabled` stays `false` by default. |
| 0.2 Pinned updater | `rsc update` fetches `install.sh` from the release tag; `install.sh` is attached to each release. |
| 0.3 Untrusted fetches | Fetched text is wrapped with a per-call nonce, researcher instructions mark it as data, and README documents the residual risk. |
| 0.4 Run budget | `max_run_tokens` and `max_run_cost_usd`, `Record::budget_hit`, a notice on the report and Run screen, and an estimate on the Review screen. |
| 0.5 Housekeeping | Release notes come from `CHANGELOG.md`, Dependabot covers Cargo and Actions, and `Mutex` poisoning in background tasks no longer takes down the UI. |

Carried over:

- **Audit in CI.** Dependabot covers updates but not advisories. Add `cargo audit` or `cargo-deny` to `ci.yml`; the tree parses untrusted input via `pdf-extract`, `html2text` and `jsonwebtoken`.
- **README one-liner** still installs from `master`. Pin it once a stable release exists.
- **Per-run domain allow/deny list** for `fetch_url` [INFERENCE: optional, add only if users ask].

---

## Phase 1 — Foundations (0.0.4) — done

Shipped in 0.0.4; see `CHANGELOG.md`.

| Item | What shipped |
|---|---|
| 1.1 Mock API and e2e tests | `RSC_API_BASE` overrides the API base and `RSC_BACKOFF_MS` scales retry delays. `tests/support` is a scripted SSE mock; `tests/e2e.rs` spawns the real binary against it with a scratch `HOME`. Covers the happy path, a failed researcher, usage limit, cancel, resume, capability degradation, 5xx retry, budget cap, checkpoints and restarts. |
| 1.2 Headless mode | `rsc run`, `resume`, `ls` and `show` in `src/cli.rs` with `--yes`, `--no-clarify`, `-o` and `--json`. Progress on stderr, report on stdout. Exit codes 0 done, 1 failed, 2 budget hit, 130 cancelled. Prompts only when stdin and stderr are terminals. |
| 1.3 Agent resilience | Per-agent checkpoints in `agents/<n>.state.json`; resume continues from the last finished turn. One automatic restart per agent after a retryable failure (15–30 s delay), at most 3 per run. `UsageLimit` still fails the run. |
| 1.4 File split | `research/mod.rs` split into `plan.rs`, `agent.rs` and `synth.rs`; `ui/app.rs` split into one file per screen under `src/ui/app/`. Pure transitions (`History::filter_key`, `Viewer::nav_key`, `upsert_angle`, `Num::clamp`) have unit tests. |

Carried over:

- **Live smoke script.** Mock fixtures are hand-written; record real responses and run a manual live check before each release (see Standing risks).
- **CI toolchain drift.** CI builds with floating `stable`, and the first 0.0.4 push failed clippy on a method deprecated after the local toolchain (`fetch_update` → `try_update`). The `v0.0.4` tag was moved once to the fixed commit. Before tagging a release, check that CI is green on the exact commit, or pin the toolchain in `ci.yml` and bump it deliberately.
- **Headless prompts have no automated test.** They need a TTY; verified by hand in tmux.
- **`rsc resume` on a run open in another rsc** is not guarded; the README says not to.

---

## Phase 2 — Report quality (0.1.0) — implemented, pre-release

The product's value is the report. Measure first, then change.

### 2.1 Eval harness — M
- A `evals/` directory with 15–25 fixed questions across types (factual lookup, comparison, recent events, contested topic, narrow technical).
- A script that runs each through `rsc run --json` and records: wall time, tokens, cost, agent failures, number of cited sources, citation validity (2.2), report length.
- Results stored as JSON and diffed across commits. Run manually (it spends plan usage); never in CI.
- Rule from here on: a prompt or pipeline change in this phase ships with an eval diff in the PR description.

### 2.2 Citation checking — M
**Problem.** `sources.rs` records which URLs were cited (`in_final`) and which agents opened them, but nothing verifies that a cited URL exists or supports the claim. The synthesizer can cite `[7]` for a claim source 7 does not make.

**Change, cheapest first.**
1. **Liveness:** after synthesis, HEAD/GET each `in_final` source (same SSRF guard as 0.1, concurrency-limited). Mark dead links in the Sources tab; add `#[serde(default)] status` to `Source`.
2. **Provenance:** flag a cited source that no agent opened or got as a search annotation (`agents` empty after `synthesize` pruning) as "model-supplied".
3. **Support check (opt-in):** for each cited sentence, a low-effort `compactor`-tier call checks whether the opened page text supports it. Costly; only on a `--verify` flag or Settings toggle. Produces a per-citation verdict in the viewer rather than rewriting the report.

### 2.3 Planner quality — S/M
- Detect overlapping angles before the Review screen (cheap model call or embedding-free heuristic on titles/briefs) and warn.
- Make the number of angles a function of question complexity. The planner already sizes `report_words` when set to 0; do the same for agent count when the user has not pinned it.
- Use the eval harness to tune the planner instructions, not intuition.

### 2.4 Hierarchical synthesis — M
**Problem.** `synthesize` divides `synth_input_budget_tokens` (120k) by the number of successful agents and condenses any longer report. At 30+ agents each report gets a few thousand tokens and detail is lost before the synthesizer sees it.

**Change.** Above a threshold (say 8 reports), synthesize in groups, then synthesize the group summaries. Keep source numbering global (it already is, via `render_list`). Verify with 2.1 that quality does not regress at low agent counts, and keep the single-pass path for those.

### 2.5 Gap-filling round — L
After the first reports, a reviewer step lists unanswered sub-questions and contradictions, and the user (Review-style screen) or `--auto` approves a second round of 1–N targeted agents before synthesis.
- Adds a `RunStatus` variant. Older binaries will not parse new `research.json` status values; note it in the changelog. New `Record` fields get `#[serde(default)]`.
- Needs 1.4 (done; the extra round goes in the split modules rather than bloating `research/mod.rs`) and 0.4 (so the extra round obeys the budget).
- Do it last in this phase; it is the largest change and the eval harness will show whether it earns its cost.

---

## Phase 3 — Workflow (0.2.0) — implemented, pre-release

Shipped in 0.2.0; see `CHANGELOG.md`. Built-in prompt text is unchanged (checked byte for byte), so no eval diff is needed.

| Item | What shipped |
|---|---|
| 3.1 Export | `src/export.rs`: Markdown with YAML front matter, self-contained HTML (raw HTML in the report is shown as text, non-http(s)/mailto links are neutralised), JSON, and PDF through `pandoc`. `rsc export <id> [--format] [-o]`; `e` in the viewer opens an export prompt (default `.html`); `c` copies the current tab over OSC 52. |
| 3.2 History | `/` also matches report text; `s` sorts by date or cost; rows show the agent count. No index: see below. |
| 3.3 Presets | `[presets.<name>]` in `config.toml` (defaults `quick`, `standard`, `deep`), a Preset chip on Home and `rsc run --preset`. A preset sets agents, effort and the run settings the chips don't show; the record stores the preset name. |
| 3.4 Editable prompts | `~/.rsc/prompts/{planner,researcher,synthesizer,chat}.md`, seeded on first run. `Record::prompts` holds the text a run started with, so resume and chat use it. The untrusted-data note stays outside the editable researcher prompt. |
| 3.5 Local context | `rsc run --context FILE` (repeatable) and a Files chip on Home attach up to 4 `.md`, `.txt` or `.pdf` files (30,000 characters each). The text joins the task brief and is cited as `[local: name]`; the Review screen and `rsc run` list the files and say they are sent to the model. |
| 3.6 Chat deep-dive | A chat answer may end with up to 3 proposed angles. `Ctrl+T` confirms, runs one researcher per angle through the gap-round mechanism and re-synthesizes everything into a new revision. `report.md` is always the latest; the previous one is archived as `report.v<n>.md`. |

Carried over:

- **History index not built.** `rsc ls` over 300 generated runs (8 agents and 60 sources each, 20 KB reports) took 0.01 s best of 3 on a release build. Listing is not slow, so `index.json` stays unbuilt; revisit if it passes ~100 ms. The History screen additionally reads every `report.md` when it opens.
- **PDF export is only checked for the missing-`pandoc` message.** `pandoc` is not installed on the development machine, so a produced PDF was never seen. It needs `pandoc` plus a LaTeX engine on `PATH`.
- **Deep-dive has no automated test.** It is TUI-only. It was checked in tmux against a scripted mock: proposal shown, `Ctrl+T` then `y`, third agent run, tab `Final v2`, `report.v1.md` archived, `research.json` has `revision: 2`.
- **OSC 52 clipboard** is limited in size by some terminals and needs `set-clipboard on` in tmux.
- **Presets set effort, not model slugs.** Slugs differ per account; models stay as configured.
- **Attached files are snapshotted** in `research.json`; resume does not re-read them.

---

## Phase 4 — Distribution (0.3.0) — implemented, pre-release

Shipped in 0.3.0; see `CHANGELOG.md`.

| Item | What shipped |
|---|---|
| 4.1 TLS without cmake | reqwest uses `rustls-no-provider` and rustls' `ring` provider, installed by `api::http_builder()`, which every HTTP client goes through. `Cargo.lock` has no `aws-lc`. `cargo install` still needs a C compiler (`ring` compiles C); the prebuilt install needs nothing. |
| 4.2 Static Linux | `x86_64`/`aarch64-unknown-linux-musl` release builds (with `musl-tools`). `install.sh` tries musl first and falls back to glibc for older releases. |
| 4.3 Windows | `x86_64`/`aarch64-pc-windows-msvc` builds, `.zip` assets, `install.ps1`, `cfg(unix)` file modes, Ctrl+C through `tokio::signal::windows`, key releases ignored in the TUI, `rsc update` through PowerShell, CI on `windows-latest`. |
| 4.4 Supply chain | Every archive is signed with cosign (keyless, GitHub OIDC) as `<asset>.sigstore.json`; the publish job verifies each signature before upload, and both installers verify when `cosign` is on `PATH`. One `publish` job uploads everything, so a failed build publishes nothing. `docs/RELEASING.md` is the checklist; the Release workflow can be run by hand as a dry run. |

Carried over:

- **Homebrew tap and cargo-binstall** deferred until a non-pre-release.
- **Windows e2e tests don't run.** `dirs::home_dir` ignores `HOME` on Windows, so `tests/e2e.rs` is `cfg(unix)`. An `RSC_HOME`-style override would enable them.
- **Windows TUI not exercised interactively.** Windows is covered by CI (fmt, clippy, unit tests), release builds, and a smoke of the released `rsc.exe` on Windows ARM64 (`install.ps1`, `--version`, `update`).
- **Paste on Windows** may arrive as key presses, so a pasted newline acts as Enter [INFERENCE: crossterm reads console input records on Windows].
- **No explicit Windows ACLs.** Privacy relies on the inherited `%USERPROFILE%` ACLs.
- **`rsc update` on Windows** is first exercisable once a release newer than 0.3.0 exists.
- **musl allocator performance** not measured.

---

## Phase 5 — Extensibility (later)

Do not start these before phases 0–2 are done; they widen the surface the earlier phases must keep correct.

- **Provider abstraction.** `src/api` is tied to the ChatGPT-plan Responses API behavior, including `subscription_sharing_*` error codes and `store: false`. Introducing a trait for "stream a response" would allow an API-key mode or other providers, but it moves every invariant in AGENTS.md. Decide on user demand.
- **More tools.** Alternative search providers, a local-file search tool, or MCP client support for researchers. Each new tool needs the same SSRF and prompt-injection treatment as `fetch_url` (0.1, 0.3).
- **Cross-run memory.** "Build on research X": seed a new run with a prior report and its sources so agents do not re-discover them. Requires deciding what is trusted and how to mark stale facts.
- **Multiple concurrent runs.** The UI assumes one active run (`Ctrl+G`, "Can't delete the active research"). Parallel runs need a run manager, a shared concurrency/budget limit across runs, and a way to show background progress.
- **Scheduled/recurring research.** Re-run a question weekly and diff reports. Builds on headless mode and a cross-run store; needs a daemon or cron integration, which is a different product posture.

---

## Standing risks

| Risk | Why it matters | Mitigation |
|---|---|---|
| The ChatGPT-plan Responses API is not a documented public contract [INFERENCE: based on the `subscription_sharing_*` error codes handled in `src/api/mod.rs`] | Behavior can change without notice and break every run | Keep degradation paths for optional fields (AGENTS.md invariant); mock fixtures (1.1, shipped) are hand-written and should be updated to encode observed real responses; add a manual live smoke script run before each release |
| Plan usage exhaustion | Parallel agents spend a shared allowance quickly | 0.4 budget cap, pre-run estimate, headless exit code 2 |
| Prompt injection through fetched or searched pages | Model output steers tool calls | 0.1, 0.3; keep tools read-only; no tool should write to disk or run commands |
| Local data under `~/.rsc` includes questions and reports | Sensitive by nature | Existing 0700/0600 + atomic writes; export (3.1) writes where the user says, never implicitly; no telemetry (see below) |
| CI toolchain drift | Floating `stable` can fail clippy on a commit that passed locally, as happened for 0.0.4 | Check CI on the release commit before tagging; consider pinning the toolchain |
| Large-file drift | `app.rs` and `research/mod.rs` already dominate review effort | 1.4 shipped in 0.0.4; add phase 2–3 code to the split modules (`plan.rs`, `agent.rs`, `synth.rs`, `src/ui/app/`), not back into `mod.rs` / `app.rs` |

## Proposed non-goals

- No telemetry or analytics. The update check is the only outbound call besides the API and `fetch_url`, and it is switchable.
- No hosted service or account system beyond ChatGPT sign-in.
- No tools that write files or execute code on the user's machine.
- No bundled GUI; the TUI plus headless CLI is the surface.

## Per-item checklist

Every item above, when implemented:

- Passes `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- Adds `#[serde(default)]` to any new `Record` field and an old-file loading test (see `record_without_planner_fields_loads`).
- Builds API requests only through `ResponseRequest::body()`, and degrades optional request fields on 400 / `subscription_sharing_unsupported_capability`.
- Updates `help_entries` and footers for any new key, and README configuration/data tables for any new setting or file.
- Adds a `CHANGELOG.md` entry.
