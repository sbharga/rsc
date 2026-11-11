# rsc

Multi-agent deep research in your terminal, powered by your ChatGPT plan.

## What it does

You ask a question; rsc runs this pipeline:

1. A planner does a few web searches and may ask up to 3 clarifying questions, over at most 2 rounds. Submitting empty answers skips them.
2. You review and edit the research angles (1–99).
3. Researchers run in parallel, each with web search. They can optionally use `fetch_url`.
4. A synthesizer writes a single cited Markdown report.

The Run screen shows each agent's live timeline: reasoning summaries, searches, opened pages and the draft as it streams. Reports open in a viewer and can be saved as `.md`. The final report cites sources by number and ends with a `## Sources` list; a Sources tab in the viewer lists every URL the agents cited or opened (Enter opens it, `c` copies it).

## Requirements

- Linux or macOS. Windows is unsupported (`src/paths.rs` and `src/auth/mod.rs` use `std::os::unix`).
- Stable Rust with edition 2024.
- A C toolchain and `cmake`, because `aws-lc-sys` is built for rustls.
- A ChatGPT account. rsc uses your ChatGPT plan's usage; manage it at <https://chatgpt.com/settings/usage>.

## Install

```sh
cargo install --git https://github.com/sbharga/rsc
```

Or from a clone:

```sh
cargo build --release   # produces target/release/rsc
```

## Usage

Run `rsc`. `rsc --version` prints the version.

First launch opens the browser for "Sign in with ChatGPT". A local callback runs on `127.0.0.1` on a random port. If the browser can't open, press `c` to copy the URL.

Then type a question and press Enter. Press `F1` (or `?` where it isn't typed as text) for the keys on any screen.

Global keys:

| Key | Action |
|---|---|
| `Ctrl+G` | Current research |
| `Ctrl+R` | History |
| `Ctrl+O` | Settings |
| `Ctrl+U` | Manage usage |
| `Ctrl+C` | Quit |

## Data and privacy

Everything lives under `~/.rsc` (directories 0700, files 0600, written atomically):

| Path | Contents |
|---|---|
| `accounts/<id>.json` | OAuth tokens |
| `state.json` | Active account |
| `config.toml` | Settings |
| `pricing.toml` | Cost-estimate rates, seeded on first run |
| `research/<id>/research.json` | Run record |
| `research/<id>/agents/<n>.md` | Per-agent reports |
| `research/<id>/report.md` | Final report |

- Incognito runs write nothing under `research/`.
- API requests are sent with `store: false`.
- `fetch_url` is off by default. When enabled, it downloads any http(s) URL the model picks, from your machine. That can include addresses on your local network.

## Configuration

`~/.rsc/config.toml` keys and defaults:

| Key | Default | Notes |
|---|---|---|
| `planner` | `{model = "", effort = "medium"}` | Empty model = first model in the catalog |
| `researcher` | `{model = "", effort = "medium"}` | Empty model = first model in the catalog |
| `compactor` | `{model = "", effort = "low"}` | Empty model = first model in the catalog |
| `synthesizer` | `{model = "", effort = "high"}` | Empty model = first model in the catalog |
| `default_agents` | `4` | Clamped to 1–99 |
| `max_parallel` | `3` | Clamped to 1–99 |
| `report_words` | `0` | `0` = the researcher sizes each report to the problem; otherwise minimum 100 |
| `max_turns` | `30` | |
| `compact_threshold_tokens` | `200000` | |
| `synth_input_budget_tokens` | `120000` | |
| `fetch_url_enabled` | `false` | |
| `incognito_default` | `false` | |

Most of these are editable in Settings (`Ctrl+O`). If `config.toml` is invalid, rsc falls back to the defaults and shows a banner.

`pricing.toml` holds per-model USD rates per 1M tokens plus web-search cost. Displayed costs are estimates; models without rates show token counts instead.

## Development

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

See [AGENTS.md](AGENTS.md) for architecture and conventions.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
