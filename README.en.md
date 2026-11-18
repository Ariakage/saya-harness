<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/soyo-harness-readme-header-dark.png">
    <img src="docs/soyo-harness-readme-header-light.png" alt="Soyo Harness — Rust Agent Runtime">
  </picture>
</p>

<p align="center">
  <a href="https://github.com/Ariakage/soyo-harness/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/Ariakage/soyo-harness/ci.yml?branch=main&style=flat-square&logo=githubactions&logoColor=white&label=CI" alt="CI"></a>
  <a href="https://github.com/Ariakage/soyo-harness/releases"><img src="https://img.shields.io/github/v/release/Ariakage/soyo-harness?include_prereleases&display_name=tag&style=flat-square&logo=github&logoColor=white" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-2ea043?style=flat-square" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/Rust-1.99.1-DEA584?style=flat-square&logo=rust&logoColor=black" alt="Rust 1.99.1">
  <img src="https://img.shields.io/badge/platform-macOS_15%2B_%7C_Linux-6e7681?style=flat-square" alt="Platform">
</p>

<p align="center">
  <a href="https://github.com/Ariakage/soyo-harness/stargazers"><img src="https://img.shields.io/github/stars/Ariakage/soyo-harness?style=social" alt="Stars"></a>
  <a href="https://github.com/Ariakage/soyo-harness/network/members"><img src="https://img.shields.io/github/forks/Ariakage/soyo-harness?style=social" alt="Forks"></a>
  <a href="https://github.com/Ariakage/soyo-harness/watchers"><img src="https://img.shields.io/github/watchers/Ariakage/soyo-harness?style=social" alt="Watchers"></a>
  <a href="https://github.com/Ariakage/soyo-harness/issues"><img src="https://img.shields.io/github/issues/Ariakage/soyo-harness" alt="Issues"></a>
</p>

<p align="center">
  <a href="https://github.com/Ariakage/soyo-harness/graphs/contributors"><img src="https://img.shields.io/github/contributors/Ariakage/soyo-harness?style=flat-square&label=Contributors&color=0f62fe" alt="Contributors"></a>
  <a href="https://github.com/Ariakage/soyo-harness/commits/main"><img src="https://img.shields.io/github/commit-activity/t/Ariakage/soyo-harness?style=flat-square&label=Commits&color=0f62fe" alt="Commits"></a>
  <a href="https://github.com/Ariakage/soyo-harness/commits/main"><img src="https://img.shields.io/github/last-commit/Ariakage/soyo-harness?style=flat-square&label=Last%20Commit&color=0f62fe" alt="Last Commit"></a>
  <img src="https://img.shields.io/github/repo-size/Ariakage/soyo-harness?style=flat-square&label=Repo%20Size&color=0f62fe" alt="Repo Size">
  <img src="https://img.shields.io/github/languages/top/Ariakage/soyo-harness?style=flat-square&logo=rust" alt="Top Language">
  <img src="https://img.shields.io/github/languages/count/Ariakage/soyo-harness?style=flat-square&label=Languages&color=0f62fe" alt="Languages">
  <a href="https://trendshift.io/repositories?q=soyo-harness"><img src="https://img.shields.io/badge/Trendshift-rank-0f62fe?style=flat-square&logo=data:image/png;base64,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" alt="Trendshift"></a>
</p>

<p align="center">
  <a href="README.md">简体中文</a> · <b>English</b>
</p>

**Soyo Harness** is an Agent Runtime Harness built in Rust for both coding and general-purpose workflows. Soyo owns and drives the model-tool execution loop directly while sharing the same runtime core across its CLI and default TUI (GUI is planned downstream). Supported host targets are **macOS 15+** and **Linux (glibc 2.35+)** on `x86_64` and `aarch64`.

> **Rename notice**: this project was formerly named **Saya Harness** and was renamed to **Soyo Harness** after commit `1452a0b851ad40b385bf00e3765a4186509c4dd0`. Changes: binary `saya`→`soyo`, crates `saya-*`→`soyo-*`, data directory `.saya`→`.soyo` (`.soyo` preferred with automatic `.saya` fallback), environment variables `SAYA_*`→`SOYO_*` (`SOYO_*` preferred with automatic `SAYA_*` fallback). Storage and wire identifiers have been migrated to the new names in two stages (`soyo_*_v1`, `SOYOENV1`/`SOYOOUT1`/`SOYOMED1`, `soyo_capture_*`, `soyo/browser/*`, `__soyo_*`); readers still accept stored `saya_*` spellings for compatibility with existing data, while hash-bound domain constants (`saya-*-v1`, etc.) and memory-internal file names keep the legacy spelling permanently to preserve sealed identities. Releases `v0.2.0` and earlier still ship the historical `saya` assets; see [docs/releases/](docs/releases/).

> Documentation under [`docs/`](docs/README.md) is maintained primarily in Simplified Chinese, with bilingual release notes where published.

## Quickstart

### 1. Build or Install

The workspace pins Rust `1.99.1` via `rust-toolchain.toml`:

```sh
cargo build --workspace --release --locked
# or install into ~/.cargo/bin
cargo install --path crates/soyo-cli --locked
```

### 2. Configure and Validate

Copy [`examples/config.example.toml`](examples/config.example.toml) (or run `soyo config init`), fill in your provider profile (`protocol`, `base_url`, `model`, `context_window`), and supply secrets solely via environment variables named by `api_key_env`:

```sh
mkdir -p ~/.config/soyo
cp examples/config.example.toml ~/.config/soyo/config.toml
./target/release/soyo config check
```

`config check` validates TOML syntax, permission rules, and required environment variable references **without contacting any model endpoint**.

Optional opt-in flags you may want to add:

- **Developer toolchains inside the sandbox**: under `[sandbox]`, set `ca_certs = true` and `toolchain_presets = ["auto"]` (or pick from `"rust"`, `"python"`, `"node"`, `"homebrew"`, `"xcode_clt"`, `"minimal_etc"`). Strict defaults remain unchanged (`network.mode = "deny"`, `/proc` omitted).
- **Anthropic Official & Prompt Caching**: set `prompt_cache = true` (or pass `--anthropic-prompt-cache`) for ephemeral prompt caching alone, or set `anthropic_official_provider = true` (alias `anthropic_offical_provider` accepted; requires `api_version = "2026-10-21"`) on an `anthropic-messages` profile to enable prompt caching, `thinking: {"type": "adaptive", "display": "summarized"}`, and bounded `max_tokens` text continuation (off by default to keep compatibility with DeepSeek's Anthropic-compatible endpoint).

### 3. Run

```sh
# Launch the default interactive TUI
./target/release/soyo --cwd /path/to/project

# Launch plain-text interactive chat
./target/release/soyo --cwd /path/to/project chat

# Run a single non-interactive command
./target/release/soyo --cwd /path/to/project --mode code run "Inspect this repository"
```

## Key Capabilities

- **Four Switchable Work Modes**: `WORK` (default general tasks), `CODE` (coding & debugging), `PLAN` (strictly read-only investigation and planning), and `BUILD` (end-to-end delivery). Switching preserves conversation history and takes effect on the next runtime turn.
- **Fail-Closed Permission & OS Sandboxing**:
  - Permission modes: `auto` (rule evaluation + Tree-sitter Bash CST analysis + optional independent model review + confirmation), `bypass` (still respects `PLAN` read-only, explicit `deny` rules, and OS sandbox boundaries), and `manual` (human approval per tool call).
  - Native OS backends: **macOS Seatbelt** (`/usr/bin/sandbox-exec`) and **Linux Bubblewrap + Seccomp + socat** wired through a host `NetworkBroker` with exact-origin allowlisting and fail-closed DNS/IP checks.
- **Model Protocols**: OpenAI Chat Completions, OpenAI Responses API, and Anthropic Messages (`2026-10-21` and `2023-06-01`), with explicit `output_config.effort`, unified context budgeting, idle/stream timeouts, and bounded retries.
- **Tools & Long-Horizon Execution**: `read_file`, `list_dir`, `find_files`, `search_text`, `write_file`, `edit_file`, `apply_patch`, `exec`, `ask_user`, background jobs (`background_start` / `background_status` / `background_cancel`), preapproved consecutive read batching in non-manual modes, stdio/Streamable HTTP MCP with private credential management, `soyo module <browser|vm> <enable|disable|status>` / `soyo module list` runtime capability switches, SQLite WAL persistence & session export, context compaction, plugins/hooks, global memory, and Goal orchestration.

## Development

Common tasks live in [`justfile`](justfile):

```sh
just                 # List recipes
just check           # fmt-check, strict clippy, workspace tests, release build
just test-tooling    # Offline Python validation & prompt checks
just tui-preview syntax
```

## Status & Documentation

- **Latest tagged release**: [`v0.2.0`](https://github.com/Ariakage/soyo-harness/releases/tag/v0.2.0) ([Bilingual release notes](docs/releases/release-notes-0.2.0.md)).
- Subsequent development on `main` (`version = "0.2.0"` per [`docs/progress.md`](docs/progress.md)) includes N2 native isolation & MCP, N3 context/plugins/memory, N4 Goal orchestration, and N5 controlled Web/Browser work (with cross-platform CI/packaging evidence tracked in [`docs/progress.md`](docs/progress.md) and [`docs/n5-validation.md`](docs/n5-validation.md)).
- Full architecture, specifications, and guides are indexed in [`README.md`](README.md) and [`docs/README.md`](docs/README.md).

## Star History

<p align="center">
  <a href="https://star-history.com/#Ariakage/soyo-harness&Date">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=Ariakage/soyo-harness&type=Date&theme=dark">
      <img src="https://api.star-history.com/svg?repos=Ariakage/soyo-harness&type=Date" alt="Star History Chart" width="600">
    </picture>
  </a>
</p>

## License

Licensed under the [MIT License](LICENSE).
