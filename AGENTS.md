# AGENTS.md

## Project

`rsc` is a terminal app for multi-agent research using a ChatGPT plan. Single binary crate: tokio runtime, ratatui TUI. A run moves through Planning → Clarifying → Review → Researching → Synthesizing → Done (`RunStatus` in `src/research/store.rs`).

## Commands

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt            # config in rustfmt.toml
```

CI runs `cargo fmt --check`, clippy and the tests on Ubuntu and macOS. All must pass before committing.

## Layout

| File | Role |
|---|---|
| `src/main.rs` | Entry point and `--version` |
| `src/ui/mod.rs` | Terminal setup and event loop (key, paste, `AppMsg`, `RunEvent`, 100 ms tick) |
| `src/ui/app.rs` | `App` state, screens, modals, key handling |
| `src/ui/draw.rs` | Rendering only, plus the `help_entries` and footer strings |
| `src/ui/fmt.rs` | Width-aware `trunc`, `pad` and `wrap` |
| `src/research/mod.rs` | `Run` orchestration, planner/clarify loop, researchers, synthesis, live timelines |
| `src/research/store.rs` | `Record`/`RunStatus` persistence and `progress` |
| `src/research/sources.rs` | Source extraction, dedup, `[n]` citation parsing and numbered lists (pure) |
| `src/research/tools.rs` | `web_search` and `fetch_url` tools |
| `src/api/mod.rs` | Responses API client, SSE streaming, error classification, retry |
| `src/api/sse.rs` | SSE parser |
| `src/auth/mod.rs` | Sign in with ChatGPT (OAuth + PKCE, local callback, token refresh) |
| `src/config.rs` | `config.toml` |
| `src/pricing.rs` | `pricing.toml` and cost estimates |
| `src/paths.rs` | `~/.rsc` layout, `atomic_write`, private directories |

## Invariants

- Build every Responses API request through `ResponseRequest::body()` (`src/api/mod.rs`). Bodies always have `store: false` and `stream: true`, and never `max_output_tokens`; the test `request_body_omits_forbidden_fields` covers this. `reasoning.summary` is sent only together with `effort`.
- Optional API capabilities degrade instead of failing. On a `Fatal` error with status 400 or code `subscription_sharing_unsupported_capability` whose `param` names the feature, turn the feature off for the run and retry. Follow `Run::call` (reasoning summaries) and `Run::agent_call` (context_management, fetch_url namespace) for any new optional request field.
- Write all files through `paths::atomic_write` (mode 0600) and `paths::create_private_dir` (mode 0700). Incognito runs must not write anything under `research/`; the `Record::save`/`write_*` methods return early.
- Every field added to `Record` needs `#[serde(default)]` so older `research.json` files still load; `record_without_planner_fields_loads` is the example. Live timelines (`RunState::planner_log` and `logs`) stay in memory only and are capped at `LOG_CAP` (500).
- Shared run state is accessed only through `Run::with`. Never hold it across `.await`. The UI reads state every frame; send a `RunEvent` only when the UI must act on something (change screen, toast, modal).
- Never print to stdout or stderr while the TUI is running, because ratatui owns the terminal. Never log or display tokens.
- When adding or changing a key binding, update the matching `help_entries` screen in `src/ui/draw.rs`, plus the footer if it is one of the screen's primary keys. `?` must not open help on screens where it is typed as text (`App::help_key_free`).

## Testing

- Unit tests live in inline `#[cfg(test)] mod tests` blocks next to the code. Test pure helpers such as `parse_plan`, `append_log`, `classify` and `ResponseRequest::body`. Tests must not touch the network or `~/.rsc`.
- To check UI changes end to end:
  1. Temporarily point `API_BASE` in `src/api/mod.rs` at a local SSE mock that serves `GET /models` and `POST /responses`.
  2. Run with `HOME` set to a scratch directory that holds a fake `~/.rsc/accounts/<id>.json` and `state.json`. `<id>` is `account_id(client_id)`, the first 12 hex characters of sha256.
  3. Drive the binary with `tmux send-keys` / `tmux capture-pane`.
  4. Revert the `API_BASE` change before committing.

## Style

- Match the surrounding code.
- Avoid `unwrap` on I/O and network results.
- Error messages shown to the user are short sentences.
- Do not add dependencies without need.
