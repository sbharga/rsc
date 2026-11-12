# Roadmap

State at v0.0.3: the pipeline (plan → clarify → review → research → synthesize), live agent timelines, source tracking with `[n]` citations, follow-up chat, resume, history, cost estimates, installer and self-update all work. Phase 0 (safety and trust) shipped in 0.0.3: `fetch_url` is SSRF-guarded, the updater is pinned to the release tag, and runs can be capped by tokens or cost. Single binary, Linux and macOS, ChatGPT sign-in only.

This document orders the next work by when it should be done and why. Order follows three rules:

1. **Trust before features.** Fix anything that can leak data or burn the user's plan before adding capability.
2. **Build the measuring stick before tuning.** Output quality can't be improved if it can't be measured, and nothing can be measured without a headless run path and a mock API.
3. **Refactor only when the next feature needs it.** Split a file at the moment a phase would otherwise make it worse.

Sizes: **S** ≤ 1 day, **M** 2–5 days, **L** > 1 week. Phases are ordered; items inside a phase are ordered too. Version numbers are targets, not promises.

## Summary

| # | Phase | Target | Theme | Gate to leave the phase |
|---|---|---|---|---|
| 0 | Safety and trust | 0.0.3 (done) | Close known holes | `fetch_url` cannot reach private addresses; updater pinned to the release tag; run budget cap works |
| 1 | Foundations | 0.1.0 | Testability, headless mode, resilience | A full run is exercised in `cargo test` against a mock API; `rsc run` works with no TTY |
| 2 | Report quality | 0.2.0 | Make the output measurably better | Eval suite shows citation validity and coverage improving release over release |
| 3 | Workflow | 0.3.0 | Export, search, presets, local context | Users can get a report out of rsc and into other tools without copy-paste |
| 4 | Distribution | 0.4.0 | Windows, packaging, supply chain | Install works without a C toolchain; signed artifacts |
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

## Phase 1 — Foundations (0.1.0)

Nothing user-visible except `rsc run`. This phase is what makes every later phase safe to build.

### 1.1 Mock API and end-to-end tests — M
**Problem.** AGENTS.md documents verifying UI changes by editing `API_BASE` by hand, running a mock, and reverting. That is the highest-leverage test fixture in the repo and it is manual. There are 42 unit tests, none in `auth`, `paths`, or `ui`, and none that run the pipeline. `research/tools.rs` already has a small local test server (`serve`) that the mock can build on.

**Change.**
- Make the API base overridable (env var read once at startup, honored in all builds, or a constructor argument on `Client`). No `cfg(test)`-only hack; integration tests live outside the crate's `src/`.
- Ship a scripted SSE mock (a `tests/support` module with a tokio server): serves `/models` and `/responses`, replays fixture streams including a `function_call`, a 5xx, a `subscription_sharing_usage_limit_exceeded`, and a rejected optional capability.
- First tests: happy path to `Done`; one researcher fails and the run still synthesizes; usage limit fails the run; cancel mid-run; resume from `Interrupted` skips finished agents; capability degradation (reasoning summary, `context_management`, `fetch_url` namespace) retries without the field.

**Done when.** `cargo test` drives a full run on both CI OSes with no network.

### 1.2 Headless mode: `rsc run` — M
**Problem.** `Run` is already UI-independent (it speaks `RunEvent` and shared state through `Run::with`), but the only front end is the TUI. No scripting, no CI, no eval.

**Change.**
- `rsc run "<question>" [--agents N] [--yes] [--no-clarify] [-o report.md] [--json]`. Planner clarification is skipped unless an answer source is given. `--yes` auto-accepts the plan.
- Progress to stderr only when stderr is not the TUI (the "never print while TUI runs" rule does not apply here; keep the two paths separate).
- Exit codes: 0 done, 1 failed, 2 budget hit, 130 cancelled.
- Also `rsc ls` and `rsc show <id>` since they need no new logic (`store::list`).

**Why now.** Required by 1.1 (drive tests), 2.1 (eval) and 3.x (scripting).

### 1.3 Agent-level resilience — M
**Problem.**
- `Run::resume` resets a non-done agent to `Pending` with `turns = 0`; all of its research is lost. Interrupted runs (laptop sleep, crash) pay full cost again.
- A single transient failure that exhausts `call_with_retry` (3 tries, 2/4/8 s) permanently fails that agent for the run.

**Change.**
- Checkpoint each agent's input items to `research/<id>/agents/<n>.state.json` after every turn (atomic write, mode 0600, trimmed after compaction). Resume continues from the checkpoint. Add the file to the README data table.
- Automatic one-shot restart of an agent after a retryable failure, with jittered backoff longer than 8 s. Cap total restarts per run.
- Keep the `UsageLimit` behavior: fail the whole run, since further agents cannot succeed.

### 1.4 Split the two large files — M
`src/ui/app.rs` is 1,867 lines and `src/research/mod.rs` is 1,154. Phases 2 and 3 add screens and orchestration steps; adding them to these files will make review and testing worse.

- `research/mod.rs` → `plan.rs` (planner/clarify loop, `parse_plan`), `agent.rs` (`run_agent`, compaction), `synth.rs` (synthesis, condense), keeping `Run`/`RunState` in `mod.rs`.
- `ui/app.rs` → one module per screen for key handling (`home`, `review`, `run`, `viewer`, `history`, `settings`, `chat`), `app.rs` keeps shared state and routing.
- Pure move, no behavior change, one commit, tests green before and after.
- Extract pure state transitions (selection, filtering, edit buffers) out of key handlers so they can be unit tested; this is where `ui` has zero coverage today.

---

## Phase 2 — Report quality (0.2.0)

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
- Needs 1.4 (so it does not bloat `research/mod.rs`) and 0.4 (so the extra round obeys the budget).
- Do it last in this phase; it is the largest change and the eval harness will show whether it earns its cost.

---

## Phase 3 — Workflow (0.3.0)

### 3.1 Export — S/M
- `rsc export <id> [--format md|html|json]`, plus an `e` key in the viewer. Markdown output with front matter (question, date, models, cost, source list). Self-contained HTML with inlined CSS.
- Clipboard copy of the whole report (the Sources tab already copies URLs, so a clipboard path exists).
- PDF only through an external tool (`pandoc` if present); not worth a dependency.

### 3.2 History that scales — S/M
- Today `/` filters by question substring only (`App::history` filtering in `ui/app.rs`), and `store::list` reads and parses every `research.json` on every call.
- Add full-text search across `report.md`, and a cached index (`~/.rsc/index.json`, rebuilt from the directory when stale) once listing becomes measurably slow. Don't build the index first; measure with a few hundred runs.
- Sort by cost and date; show agent count and status per row.

### 3.3 Presets — S
Named profiles in `config.toml`, e.g. `quick` (2 agents, low effort), `standard`, `deep` (more agents, high effort, longer reports). Selectable from the Home option chips and `rsc run --preset`. Replaces tuning four stage models per run.

### 3.4 Editable prompts — S
Planner, researcher, synthesizer and chat instructions are `const` strings (e.g. `SYNTH_INSTRUCTIONS`). Seed them into `~/.rsc/prompts/*.md` on first run, as is already done for `pricing.toml`; a missing file falls back to the built-in. Fits `report_style`, which already covers tone but not method. Keep prompts versioned in the record so `resume` uses the prompt the run began with.

### 3.5 Local context — M
`rsc run --context path` / a Home action to attach local `.md`, `.txt` or PDF files (reuse the `pdf-extract` path in `tools.rs`). Content becomes part of the task brief and is cited as `[local: name]`. This is a privacy-relevant change: show the attached files on the Review screen and state that they are sent to the model.

### 3.6 Chat that can dig deeper — M
Let a follow-up chat answer say "this needs new research" and, on user confirmation, launch one or more extra agents and fold the result into the report as a new section or a new revision (`report.v2.md`). Reuses `Run::drive`. Depends on 1.4 and 2.5's round mechanism.

---

## Phase 4 — Distribution (0.4.0)

### 4.1 Remove the C toolchain requirement — M
README requires `cmake` and a C toolchain because `aws-lc-sys` backs rustls. Evaluate switching the rustls crypto provider to `ring` (or using prebuilt `aws-lc` with `fips` off) so `cargo install` works on a plain Rust install. Confirm it still passes the release matrix.

### 4.2 Linux portability — S
Release binaries are built on `ubuntu-22.04` against glibc. Add static `x86_64`/`aarch64-unknown-linux-musl` builds if the TLS stack permits (depends on 4.1), so older distros work. Extend `install.sh` target detection.

### 4.3 Windows — L
`src/paths.rs` and `src/auth/mod.rs` use `std::os::unix` for file modes. Needed: `cfg(windows)` paths with user-only ACLs (or documented fallback), a different `rsc update` (no `sh`), Windows Terminal verification for crossterm/ratatui, a `.zip` release asset, and CI on `windows-latest`. Defer until demand exists; the permission model is the real work, not compilation.

### 4.4 Supply chain — S/M
- Sign release artifacts (cosign keyless via GitHub OIDC) and verify in `install.sh` when `cosign` is present. SHA-256 alone only proves the file matches the release.
- Homebrew tap and `cargo-binstall` metadata. Do these after tagging a non-pre-release 0.1.
- Publish a release checklist (`docs/RELEASING.md`): bump `Cargo.toml`, changelog, tag, verify assets.

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
| The ChatGPT-plan Responses API is not a documented public contract [INFERENCE: based on the `subscription_sharing_*` error codes handled in `src/api/mod.rs`] | Behavior can change without notice and break every run | Keep degradation paths for optional fields (AGENTS.md invariant); mock fixtures in 1.1 should encode observed real responses; add a manual live smoke script run before each release |
| Plan usage exhaustion | Parallel agents spend a shared allowance quickly | 0.4 budget cap, pre-run estimate, headless exit code 2 |
| Prompt injection through fetched or searched pages | Model output steers tool calls | 0.1, 0.3; keep tools read-only; no tool should write to disk or run commands |
| Local data under `~/.rsc` includes questions and reports | Sensitive by nature | Existing 0700/0600 + atomic writes; export (3.1) writes where the user says, never implicitly; no telemetry (see below) |
| Large-file drift | `app.rs` and `research/mod.rs` already dominate review effort | 1.4 before phases 2–3 |

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
