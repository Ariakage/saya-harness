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
    ./scripts/test-tooling.sh

# 验证显式 Node 与固定 worker 的私有协议和通道范围
test-n5-worker-protocol node worker_root:
    python3 -I -B scripts/n5/test_worker_protocol.py --node "$1" --worker-root "$2" --temp-root "$PWD/target/tmp/n5-worker-protocol"

# 验证固定 worker 的内部角色查询与受控 CDP 生命周期
test-n5-worker-process-info node worker_root:
    "$1" scripts/n5/test_worker_process_info.cjs "$2"

# 验证固定 worker 的原生截图遮蔽私密请求与受控 CDP 生命周期
test-n5-worker-capture node worker_root:
    "$1" scripts/n5/test_worker_capture.cjs "$2"

# 验证显式指定程序的 CLI、会话、审查回归和 TUI
smoke binary:
    python3 -B scripts/release/smoke.py "$1"
    python3 -B scripts/release/journey.py "$1"
    python3 -B scripts/release/review_regressions.py "$1"
    python3 -B scripts/release/tui_journey.py "$1"
