# rsc

Multi-agent deep research in your terminal, powered by your ChatGPT plan.

![rsc in the terminal: ask a question, review the planned angles, watch four agents research in parallel, then read the cited report](docs/demo.gif)

## What it does

You ask a question; rsc runs this pipeline:

1. A planner does a few web searches and may ask up to 3 clarifying questions, over at most 2 rounds. Submitting empty answers skips them.
2. You review and edit the research angles (1–99). With `default_agents = 0` (the default) the planner picks 2–8; the Review screen warns when two angles look alike.
3. Researchers run in parallel, each with web search. They can optionally use `fetch_url`.
4. Optionally, a reviewer reads the first-round reports and proposes follow-up researchers (`gap_round`); you approve or skip them.
5. A synthesizer writes a single cited Markdown report. With more than 8 reports it merges them in groups first.

The Run screen shows each agent's live timeline: reasoning summaries, searches, opened pages and the draft as it streams. Reports open in a viewer and can be saved as `.md` or exported as `.md`, `.html`, `.json` or `.pdf` (`e`). The final report cites sources by number and ends with a `## Sources` list; a Sources tab in the viewer lists every URL the agents cited or opened (Enter opens it, `c` copies it). Cited links are probed and marked when dead; a cited URL no agent opened or got from search is marked model-supplied; with `verify_citations` (or `rsc run --verify`) a model checks each cited page against the claims that cite it, and `v` on the Sources tab shows the verdicts. History (`Ctrl+R`) searches questions and report text with `/` and sorts by date or cost with `s`.

## Requirements

- Linux, macOS or Windows 10+ on x86_64 or arm64.
- A ChatGPT account. rsc uses your ChatGPT plan's usage; manage it at <https://chatgpt.com/settings/usage>.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/sbharga/rsc/master/install.sh | sh
```

On Windows, in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/sbharga/rsc/master/install.ps1 | iex"
```

This downloads the newest release binary (pre-releases included) for your OS and architecture, verifies its SHA-256, and also its Sigstore signature when `cosign` is on `PATH`. It installs to `~/.local/bin` (Windows: `%LOCALAPPDATA%\Programs\rsc`). On Linux it installs the static musl build, which runs on any distro; releases before 0.3.0 fall back to glibc. Set `RSC_INSTALL_DIR=/path` to change the directory and `RSC_VERSION=v0.3.0` to pin a version.

To verify a download by hand (replace the asset name for your platform):

```sh
cosign verify-blob rsc-x86_64-unknown-linux-musl.tar.gz --bundle rsc-x86_64-unknown-linux-musl.tar.gz.sigstore.json \
  --certificate-identity https://github.com/sbharga/rsc/.github/workflows/release.yml@refs/tags/v0.3.0 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

### From source

Requires stable Rust with edition 2024 and a C compiler for `ring` (rustls' crypto): `cc` on Linux and macOS, the Visual Studio C++ build tools on Windows, plus `clang` for ARM64.

```sh
cargo install --git https://github.com/sbharga/rsc --tag v0.3.0
```

Or from a clone:

```sh
cargo build --release   # produces target/release/rsc
```

## Updating

On launch rsc checks GitHub releases. When a newer one exists, the home screen shows `rsc X available — run rsc update`. `rsc update` installs the newest release over the current binary by running the install script from that release's tag. Turn the check off with `check_updates = false` or in Settings. On Windows, `rsc update` runs `install.ps1` through PowerShell, and the old binary is left as `rsc.exe.old` until the next update.

## Usage

Run `rsc`. `rsc --version` prints the version; `rsc update` updates rsc.

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

## Headless use

```sh
rsc run "<question>" [--agents N|auto] [--preset NAME] [--context FILE]... [--verify] [--yes] [--no-clarify] [-o FILE] [--json]
rsc resume <id> [--yes] [--no-clarify] [-o FILE] [--json]
rsc ls
rsc show <id>
rsc export <id> [--format md|html|json|pdf] [-o FILE]
```

`rsc run` works without a terminal. Progress goes to stderr and the report to stdout (or to `-o FILE`). Without a terminal on stdin and stderr, clarifying questions are skipped and `--yes` is required to accept the plan; in a terminal, rsc asks. `--json` prints a summary object on stdout instead of the report; it includes `citations` (live/dead/unknown links, model-supplied and invalid citation numbers, verdict counts). `python3 evals/eval.py run` records these per question and `python3 evals/eval.py diff` compares two result files; it spends plan usage, so run it by hand. `rsc resume` continues a failed, cancelled or interrupted run; don't resume a run that is still open in another rsc.

Exit codes: `0` done, `1` failed or usage error, `2` done but a budget cap stopped research early, `130` cancelled (Ctrl+C or declining the plan).

`rsc export <id>` prints a report as Markdown with YAML front matter (title, models, cost, cited sources), HTML (a self-contained page; raw HTML in the report is shown as text) or JSON (record plus report), or writes it with `-o FILE`. Without `--format` the format follows the extension of `-o`, and is Markdown otherwise. PDF needs `pandoc` (and a LaTeX engine) on `PATH` and `-o FILE`. In the viewer, `e` exports the final report and `c` copies the current tab. `rsc run --preset NAME` applies a preset (see Configuration) and `--context FILE` (up to 4 `.md`, `.txt` or `.pdf` files, repeatable) gives the planner, researchers and synthesizer your own material; the report cites it as `[local: name]`.

## Follow-up chat

On a finished research's report (after it completes, or from History with `Ctrl+R`), press `a` to ask follow-up questions. Answers stream in, may use web search, and are saved per research, so reopening it continues the conversation. Chat cost is shown on the chat screen and is not added to the research's total.

When a question needs research the report doesn't cover, the answer may end with up to 3 proposed angles. `Ctrl+T` asks for confirmation (y/n), runs one researcher per angle, and writes a new report revision from all reports; the tab then reads `Final v2` and the previous report stays in `report.v1.md`. It is refused while another research runs or when the research hit its budget cap.

## Data and privacy

Everything lives under `~/.rsc` (`%USERPROFILE%\.rsc` on Windows; directories 0700, files 0600, written atomically). On Windows rsc sets no ACLs; files inherit your profile folder's permissions (by default you, SYSTEM and Administrators):

| Path | Contents |
|---|---|
| `accounts/<id>.json` | OAuth tokens |
| `state.json` | Active account |
| `config.toml` | Settings |
| `pricing.toml` | Cost-estimate rates, seeded on first run |
| `prompts/*.md` | Editable stage prompts, seeded on first run |
| `research/<id>/research.json` | Run record |
| `research/<id>/agents/<n>.md` | Per-agent reports |
| `research/<id>/agents/<n>.state.json` | Researcher progress for resume; removed when the agent finishes or fails |
| `research/<id>/report.md` | Latest final report |
| `research/<id>/report.v<n>.md` | Earlier revisions, written when a chat deep-dive replaces the report |
| `research/<id>/chat.json` | Follow-up chat |

- API requests are sent with `store: false`. Whether OpenAI may train on your data is set by your ChatGPT account's Data Controls.
- `fetch_url` is off by default. When on, it downloads http(s) URLs the model picks, from your machine. rsc refuses loopback, private, link-local, unique-local, CGNAT and other non-public addresses, including through redirects (max 5) and DNS. It ignores system proxy settings so this check always applies.
- Pages can contain instructions aimed at the model (prompt injection). rsc marks fetched text as untrusted, but a prompt cannot fully prevent a model from, for example, putting your question into a URL it fetches. The site would then see it. Leave `fetch_url` off if that matters.
- `check_links` (on by default) sends a HEAD, then if needed a GET, to each URL cited in the final report, from your machine; sites see your IP. It uses the same address guard as `fetch_url` and never reads the body. `verify_citations` additionally downloads cited pages and sends their text to the compactor model.
- Files you attach (`--context`, Files chip) are read from disk once; their text (up to 30,000 characters each) is sent to the model and stored in `research.json`. Resume does not re-read them.
- Unless `check_updates` is off, each launch sends one unauthenticated request to `api.github.com` to check for releases.

## Configuration

`~/.rsc/config.toml` keys and defaults:

| Key | Default | Notes |
|---|---|---|
| `planner` | `{model = "", effort = "medium"}` | Empty model = first model in the catalog |
| `researcher` | `{model = "", effort = "medium"}` | Empty model = first model in the catalog |
| `compactor` | `{model = "", effort = "low"}` | Empty model = first model in the catalog |
| `synthesizer` | `{model = "", effort = "high"}` | Empty model = first model in the catalog |
| `default_agents` | `0` | `0` = the planner chooses 2–8; otherwise clamped to 1–99 |
| `max_parallel` | `3` | Clamped to 1–99 |
| `report_words` | `0` | `0` = the researcher sizes each report to the problem; otherwise minimum 100 |
| `report_style` | `""` | Instructions for how the final report and chat answers are written, e.g. `Explain like I'm 5`. Applies to new runs |
| `max_turns` | `30` | |
| `compact_threshold_tokens` | `200000` | |
| `synth_input_budget_tokens` | `120000` | |
| `max_run_tokens` | `0` | Run budget in tokens (all stages); `0` = no cap. When reached, no new researchers start, running ones write their report, and synthesis completes. The report notes it. |
| `max_run_cost_usd` | `0` | Same cap in estimated USD from `pricing.toml`; models without rates count only web-search cost. |
| `fetch_url_enabled` | `false` | |
| `check_links` | `true` | Probe cited URLs and flag dead ones |
| `verify_citations` | `false` | Check each cited page against the claims that cite it (extra model calls) |
| `gap_round` | `false` | After the first round, a reviewer may propose follow-up researchers for you to approve |
| `check_updates` | `true` | Check GitHub for a newer release on launch |
| `presets` | `quick`, `standard`, `deep` | `[presets.<name>]` tables, see below |

Most of these are editable in Settings (`Ctrl+O`). If `config.toml` is invalid, rsc falls back to the defaults and shows a banner.

Presets are named run profiles in `[presets.<name>]` tables. Each field is optional and replaces the matching setting for that run: `agents`, `effort` (planner, researcher and synthesizer; models stay as configured), `report_words`, `max_turns`, `gap_round`, `verify_citations`. Pick one with the Preset chip on the home screen or `rsc run --preset NAME`. If `config.toml` has no `presets` key, rsc offers `quick`, `standard` and `deep`; defining any `[presets.*]` table replaces all of them.

```toml
[presets.quick]
agents = 2
effort = "low"
report_words = 500
max_turns = 10
```

Each prompt can be edited in `~/.rsc/prompts/` (`planner.md`, `researcher.md`, `synthesizer.md`, `chat.md`). rsc writes the built-in text there on first launch and never overwrites it; a missing or blank file means the built-in. Placeholders such as `{team}` and `{step3}` (planner) and `{title}`, `{brief}` and `{length}` (researcher) must stay. A run stores the prompts it started with, so resume and chat use those. The note that web content is untrusted is always added to the researcher prompt.

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
