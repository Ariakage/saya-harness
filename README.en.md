<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/soyo-harness-readme-header-dark.png">
    <img src="docs/soyo-harness-readme-header-light.png" alt="Soyo Harness — Rust Agent Runtime">
  </picture>
</p>

<p align="center">
  <a href="https://github.com/Ariakage/soyo-harness/actions/workflows/ci.yml"><img src="https://github.com/Ariakage/soyo-harness/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/Ariakage/soyo-harness/releases"><img src="https://img.shields.io/github/v/release/Ariakage/soyo-harness?include_prereleases&display_name=tag&logo=github" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/Rust-1.99.1-DEA584?logo=rust&logoColor=white" alt="Rust 1.99.1">
  <img src="https://img.shields.io/badge/platform-macOS%2015%2B%20%7C%20Linux-lightgrey" alt="Platform">
</p>

<p align="center">
  <a href="https://github.com/Ariakage/soyo-harness/stargazers"><img src="https://img.shields.io/github/stars/Ariakage/soyo-harness?style=social" alt="Stars"></a>
  <a href="https://github.com/Ariakage/soyo-harness/network/members"><img src="https://img.shields.io/github/forks/Ariakage/soyo-harness?style=social" alt="Forks"></a>
  <a href="https://github.com/Ariakage/soyo-harness/watchers"><img src="https://img.shields.io/github/watchers/Ariakage/soyo-harness?style=social" alt="Watchers"></a>
  <a href="https://github.com/Ariakage/soyo-harness/issues"><img src="https://img.shields.io/github/issues/Ariakage/soyo-harness" alt="Issues"></a>
</p>

<p align="center">
  <a href="README.md">简体中文</a> · <b>English</b>
</p>

**Soyo Harness** is an Agent Runtime Harness built in Rust for both coding and general-purpose workflows. Soyo owns and drives the model-tool execution loop directly while sharing the same runtime core across its CLI and default TUI (GUI is planned downstream). Supported host targets are **macOS 15+** and **Linux (glibc 2.35+)** on `x86_64` and `aarch64`.

> **Rename notice**: this project was formerly named **Saya Harness** and was renamed to **Soyo Harness** after commit `1452a0b851ad40b385bf00e3765a4186509c4dd0`. Changes: binary `saya`→`soyo`, crates `saya-*`→`soyo-*`, data directory `.saya`→`.soyo` (`.soyo` preferred with automatic `.saya` fallback), environment variables `SAYA_*`→`SOYO_*` (`SOYO_*` preferred with automatic `SAYA_*` fallback). Storage and wire-format identifiers (`saya_*_v1`, `SAYAENV1`/`SAYAOUT1`/`SAYAMED1`, `saya_capture_*`, `saya/browser/*`, etc.) keep the legacy spelling for compatibility with existing data and protocols. Releases `v0.2.0` and earlier still ship the historical `saya` assets; see [docs/releases/](docs/releases/).

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
