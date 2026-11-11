set shell := ["sh", "-eu", "-c"]
set positional-arguments

export MACOSX_DEPLOYMENT_TARGET := env("MACOSX_DEPLOYMENT_TARGET", "15.0")
export CARGO_PROFILE_DEV_DEBUG := env("CARGO_PROFILE_DEV_DEBUG", "line-tables-only")

# 列出可用开发任务
default:
    @just --list

# 执行完整 Rust 检查与优化构建
check:
    ./scripts/check.sh

# 格式化 Rust 源码
fmt:
    cargo fmt --all

# 检查 Rust 格式
fmt-check:
    cargo fmt --all -- --check

# 执行 workspace 全目标严格 Clippy
clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# 执行 workspace 测试，可追加过滤条件和测试参数
test *args:
    cargo test --workspace --locked --no-fail-fast "$@"

# 构建开发版本，可追加 Cargo 构建参数
build *args:
    cargo build --workspace --locked "$@"

# 构建优化版本，可追加 Cargo 构建参数
release *args:
    cargo build --workspace --release --locked "$@"

# 运行开发版 Saya，可追加 CLI 参数
run *args:
    cargo run --locked -p saya-cli -- "$@"

# 打开 debug TUI 预览，可指定场景和 --source
tui-preview *args:
    cargo run --locked -p saya-cli -- tui-preview "$@"

# 输出离线完整模型提示词，可指定 work、plan、code、build、compact、memory、review 或 all
prompt type="all":
    cargo run --locked -p saya-core --example prompt_preview -- "$1"

# 输出离线 TUI 快照，可指定宽、高、场景和 --json
render-preview *args:
    cargo run --locked -p saya-tui --example render_preview -- "$@"

# 执行发行、N2、N3 与提示词评估脚本的离线 Python 回归
test-tooling:
    python3 -B scripts/test_prompt_eval.py
    python3 -B scripts/n2/test_install_linux_tools.py
    python3 -B scripts/n2/test_native_acceptance.py
    python3 -B scripts/n2/test_verify_performance.py
    python3 -B scripts/release/test_package.py
    python3 -B scripts/release/test_verify_macos15.py
    python3 -B scripts/release/test_verify_linux.py
    python3 -B scripts/release/test_script_helpers.py
    python3 -B -m unittest discover -s scripts/n3 -p 'test_*.py'

# 验证显式指定程序的 CLI、会话、审查回归和 TUI
smoke binary:
    python3 -B scripts/release/smoke.py "$1"
    python3 -B scripts/release/journey.py "$1"
    python3 -B scripts/release/review_regressions.py "$1"
    python3 -B scripts/release/tui_journey.py "$1"
