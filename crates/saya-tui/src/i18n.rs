//! Centralized user-facing text for localization.
//!
//! Command syntax, protocol values, internal state keys and error codes are
//! intentionally not translated here. UI code maps them to display text.

pub(crate) mod command {
    pub const MODE_PLAN: &str = "Read-only investigation and planning";
    pub const MODE_BUILD: &str = "End-to-end implementation and delivery";
    pub const MODE_CODE: &str = "Write, modify, and debug code";
    pub const MODE_WORK: &str = "General task execution";
    pub const PERMISSIONS_AUTO: &str = "Evaluate tool calls with the configured policy";
    pub const PERMISSIONS_MANUAL: &str = "Require approval before every Agent tool call";
    pub const PERMISSIONS_BYPASS: &str = "Skip confirmations within explicit deny and mode rules";
    pub const SESSIONS: &str = "Open the saved session selector; an optional page number follows";
    pub const NEW: &str = "Create a session and wait for your message";
    pub const PROFILES: &str = "List configured model profiles";
    pub const TOOLS: &str = "Inspect committed tool records for this session";
    pub const JOBS: &str = "Inspect background-job records for this session";
    pub const STATUS: &str = "Show the current runtime and permission state";
    pub const CANCEL: &str = "Cancel the active run or UI operation";
    pub const HELP: &str = "Show commands and keyboard controls";
    pub const EXIT: &str = "Cancel owned work and leave the terminal interface";
    pub const COMPACT: &str = "Compact saved context now, or queue it during the active run";
    pub const MEMORY: &str = "Browse current memory, review changes and manage backups";
    pub const PROFILE: &str = "Add a configured profile name to select while idle";
    pub const RESUME: &str = "Add a saved session ID to restore and wait for input";
    pub const RESUME_ACCEPT: &str = "Add a session ID and explicitly accept configuration changes";
    pub const CANCEL_JOB: &str = "Add the background job ID owned by the active run";
    pub const MARKDOWN_RENDER: &str = "Render assistant Markdown, math, diagrams and footnotes";
    pub const MARKDOWN_SOURCE: &str = "Display the original assistant Markdown source";
    pub const MARKDOWN_SOURCE_VALUE: &str = "source";
    pub const MARKDOWN_RENDER_VALUE: &str = "render";
    pub const FOOTNOTE: &str = "Add a footnote number to jump to its definition";
    pub const FOOTNOTE_BACK: &str = "Return to the previous footnote reference";
    pub const EXPORT_JSON: &str = "Add a new target path for a JSON transcript export";
    pub const EXPORT_MARKDOWN: &str = "Add a new target path for a Markdown transcript export";
    pub const EXPORT_FULL_JSON: &str =
        "Add a new target path for a full JSON export with tool records";
    pub const EXPORT_FULL_MARKDOWN: &str =
        "Add a new target path for a full Markdown export with tool records";
    pub const CD: &str = "Add a directory to use as the workspace";
    pub const SANDBOX_STATUS: &str = "Show whether native sandbox execution is enabled";
    pub const SANDBOX_ON: &str = "Use the platform native sandbox for subsequent runs";
    pub const SANDBOX_OFF: &str = "Use host execution without native isolation for subsequent runs";
    pub const MODEL: &str = "Add a configured model profile name to select while idle";
    pub const EFFORT: &str = "Add low, medium, high, xhigh, max, or default for thinking effort";
    pub const THINKING: &str = "Alias for /effort thinking effort selection";
    pub const AUTOCOMPACT: &str = "Add a token threshold, auto, or off for automatic compaction";
    pub const SKILLS: &str = "Open the discovered Skill selector";
    pub const MCPS: &str = "Open the configured MCP server selector";
    pub const PLUGINS: &str = "Open the installed Plugin selector";
    pub const THINKING_LEVEL: &str = "Set thinking effort for subsequent requests";
    pub const AUTOCOMPACT_AUTO: &str = "Restore the configured percentage trigger";
    pub const AUTOCOMPACT_OFF: &str = "Disable automatic compaction";
}

pub(crate) mod common {
    pub const APP_NAME: &str = "Saya Harness";
    pub const WINDOW_NAME: &str = "Saya";
    pub const READY: &str = "Ready";
    pub const RUNNING: &str = "Running";
    pub const THINKING: &str = "Thinking";
    pub const RESPONDING: &str = "Responding";
    pub const REVIEWING: &str = "Reviewing";
    pub const COMPACTING: &str = "Compacting";
    pub const CANCELLING: &str = "Cancelling";
    pub const COMPLETED: &str = "Completed";
    pub const FAILED: &str = "Failed";
    pub const AWAITING_APPROVAL: &str = "Awaiting approval";
    pub const AWAITING_INPUT: &str = "Awaiting input";
    pub const NEW_SESSION: &str = "New session";
    pub const NO_PROFILE: &str = "No profile";
    pub const UNCONFIGURED: &str = "Unconfigured";
    pub const UNAVAILABLE: &str = "unavailable";
    pub const SENSITIVE_WITHHELD: &str = "Sensitive details withheld";
    pub const TRUNCATED: &str = "truncated";
    pub const TRUNCATED_SUFFIX: &str = " · truncated";
    pub const STREAMING: &str = "streaming";
    pub const TOKENS_PER_SECOND: &str = "tok/s";
    pub const TOKENS: &str = "tokens";
    pub const TRANSPORT_STDIO: &str = "stdio";
    pub const TRANSPORT_HTTP: &str = "HTTP";

    pub fn token_label(value: u64) -> String {
        for (divisor, suffix) in [
            (1_000_000_000_000, "T"),
            (1_000_000_000, "G"),
            (1_000_000, "M"),
            (1_000, "k"),
        ] {
            if value >= divisor {
                let whole = value / divisor;
                let decimal = value % divisor / (divisor / 10);
                return if decimal == 0 {
                    format!("{whole}{suffix}")
                } else {
                    format!("{whole}.{decimal}{suffix}")
                };
            }
        }
        value.to_string()
    }
    pub fn window_title(session: &str) -> String {
        format!("{WINDOW_NAME} | {session}")
    }
}

pub(crate) mod status {
    use super::common;

    pub fn display(value: &str) -> String {
        match value {
            "Ready" => common::READY,
            "Running" => common::RUNNING,
            "Thinking" => common::THINKING,
            "Responding" => common::RESPONDING,
            "Reviewing" => common::REVIEWING,
            "Compacting" => common::COMPACTING,
            "Cancelling" => common::CANCELLING,
            "Completed" => common::COMPLETED,
            "Failed" => common::FAILED,
            "Awaiting approval" => common::AWAITING_APPROVAL,
            "Awaiting input" => common::AWAITING_INPUT,
            "NeedsInput" => common::AWAITING_INPUT,
            "NeedsConfirmation" => common::AWAITING_APPROVAL,
            other => other,
        }
        .to_owned()
    }
}

pub(crate) mod permissions {
    pub const AUTO: &str = "auto";
    pub const MANUAL: &str = "Manual Approval";
    pub const BYPASS: &str = "bypass permissions";
    pub const MANUAL_SHORT: &str = "manual";
    pub const BYPASS_SHORT: &str = "bypass";

    pub fn short(value: &str) -> &str {
        match value {
            MANUAL => MANUAL_SHORT,
            BYPASS => BYPASS_SHORT,
            other => other,
        }
    }
}

pub(crate) mod mode {
    pub fn display(value: &str) -> String {
        match value {
            "Work" | "WORK" => "WORK",
            "Code" | "CODE" => "CODE",
            "Plan" | "PLAN" => "PLAN",
            "Build" | "BUILD" => "BUILD",
            other => other,
        }
        .to_owned()
    }
}

pub(crate) mod view {
    pub const DESCRIPTION_PLACEHOLDER: &str = "Describe a task…";
    pub const CONTEXT: &str = "Context";
    pub const CONTEXT_UNKNOWN: &str = " · Unknown";
    pub const CONTEXT_ESTIMATED: &str = " · Estimated";
    pub const CONTEXT_WINDOW: &str = "Window";
    pub const CONTEXT_UNKNOWN_BEFORE_REQUEST: &str = "Unknown before request";
    pub const CONTEXT_AWAITING_USAGE: &str = "Awaiting execution usage";
    pub const CONTEXT_TOTAL: &str = "Total";
    pub const CONTEXT_MORE: &str = "Additional entries omitted";
    pub const CONTEXT_ESTIMATE_OVERFLOW: &str = "estimate exceeds range";
    pub const EXTENSIONS: &str = "Extensions";
    pub const DETAILS: &str = "Details";
    pub const PROFILE: &str = "Profile";
    pub const MODEL: &str = "Model";
    pub const MODE: &str = "Mode";
    pub const PERMISSIONS: &str = "Permissions";
    pub const EFFORT: &str = "Effort";
    pub const STATUS: &str = "Status";
    pub const TASKS: &str = "Tasks";
    pub const NO_TASKS: &str = "No tasks";
    pub const YOU: &str = "You";
    pub const SAYA: &str = "Saya";
    pub const TOOL: &str = "Tool";
    pub const THINKING: &str = "Thinking";
    pub const SYSTEM: &str = "System";
    pub const MCP_NO_SERVERS: &str = "MCP  No servers";
    pub const MCP_CONFIGURED: &str = "Configured";
    pub const MCP_CONNECTING: &str = "Connecting";
    pub const MCP_CONNECTED: &str = "Connected";
    pub const MCP_STOPPED: &str = "Stopped";
    pub const MCP_NEEDS_AUTH: &str = "Needs auth";
    pub const MCP_FAILED: &str = "Failed";
    pub const PLUGINS_ENABLED: &str = "Plugins  Enabled in config";
    pub const PLUGINS_DISABLED: &str = "Plugins  Disabled in config";
    pub const PLUGINS_UNKNOWN: &str = "Plugins  Not inspected";
    pub const SKILLS: &str = "Skills";
    pub const SKILLS_DISCOVERED: &str = "discovered";
    pub const APPROVAL_REQUIRED: &str = "Approval required";
    pub const QUESTION: &str = "Question";
    pub const COMMANDS: &str = "Commands";
    pub const COMMAND_SUGGESTIONS: &str = "Command suggestions";
    pub const TRUNCATED_DISPLAY: &str = "[display truncated]";
    pub const EARLIER_LINES_OMITTED: &str = "[earlier display lines omitted]";
    pub const RUNNING: &str = "running";
    pub const DONE: &str = "done";
    pub const STATUS_INPUT: &str = "Input";
    pub const STATUS_CANCEL: &str = "Cancel";
    pub const STATUS_DONE: &str = "Done";
    pub const METER_INPUT: &str = "input";
    pub const OVER_100_PERCENT: &str = ">100%";
    pub const LESS_THAN_POINT_ONE_PERCENT: &str = "<0.1%";
    pub const MESSAGE: &str = "Message";
    pub const TRUNCATED_DETAIL_PREFIX: &str = "Truncated · ";
    pub const DETAIL_TRUNCATION_NOTICE: &str = "[details display truncated]";
    pub const COMPLETION_KEYS: &str = "↑↓ Tab/→ Esc";
    pub const COMPLETION_SHORT: &str = "Tab/→";
    pub const ESC_CLOSE: &str = " Esc close ";
    pub const PALETTE_NAV: &str = "↑↓ · Enter";
    pub const PALETTE_FULL_NAV: &str = "↑↓ select · Enter run · Esc close";
    pub const PALETTE_COMPLETION_NAV: &str = " ↑↓ Tab/→ Esc ";
    pub const ENTER_SEND: &str = "Enter";
    pub const ENTER_ACTION: &str = " send · ";
    pub const NEWLINE_KEY: &str = "Alt+Enter";
    pub const NEWLINE_ACTION: &str = " newline · ";
    pub const FOLD_KEY: &str = "Ctrl+O";
    pub const FOLD_ACTION: &str = " fold";
    pub const TAB_MODE: &str = "Tab mode · Ctrl+P commands";
    pub const TAB_MODE_SHORT: &str = "Tab mode · Ctrl+P";
    pub const CTRL_P: &str = "Ctrl+P";
    pub const COMMANDS_HINT: &str = "Ctrl+P commands";
    pub const TERMINAL_TOO_SMALL: &str = "Saya: enlarge terminal to at least 20 x 6";
    pub const MCP: &str = "MCP";
    pub const PASTE_TRUNCATED: &str = "Message · paste truncated at 64 KiB";
    pub const APPROVAL_SHORT: &str = "Approval";
    pub const MCP_MORE: &str = "more";
    pub const TASKS_MORE: &str = "more tasks";
    pub const EARLIER_VIEWPORT_MESSAGES: &str = "earlier viewport messages omitted";

    pub fn mcp_summary(status: &str, count: usize, server: &str) -> String {
        let suffix = if count > 1 {
            format!(" +{} {}", count - 1, MCP_MORE)
        } else {
            String::new()
        };
        format!("{MCP} {status}{suffix} · {server}")
    }
    pub fn mcp_count(count: usize, visible: usize) -> String {
        let suffix = if visible < count {
            format!(" · +{} {}", count - visible, MCP_MORE)
        } else {
            String::new()
        };
        format!("{MCP} {count}{suffix}")
    }
    pub fn more_tasks(count: usize) -> String {
        format!("+{count} {TASKS_MORE}")
    }
    pub fn earlier_viewport_messages(count: usize) -> String {
        format!("[{count} {EARLIER_VIEWPORT_MESSAGES}]")
    }
    pub fn tool_count(tools: usize) -> String {
        format!("{tools}t")
    }
}

pub(crate) mod picker {
    pub const SKILLS_TITLE: &str = "Skills";
    pub const MCPS_TITLE: &str = "MCP servers";
    pub const PLUGINS_TITLE: &str = "Plugins";
    pub const SESSIONS_TITLE: &str = "Sessions";
    pub const NO_SKILLS: &str = "No applicable Skills were discovered.";
    pub const NO_MCPS: &str = "No MCP servers are configured.";
    pub const NO_PLUGINS: &str = "No installed Plugins are available.";
    pub const PLUGIN_STORE_MISSING: &str =
        "No Plugin store exists. Install a local package with `saya plugin install`.";
    pub const SESSION_EMPTY: &str = "No sessions are saved on this page.";
    pub const ENTER_DETAILS: &str = "Enter details · ↑↓ select · Esc close";
    pub const ENTER_TOGGLE: &str = "Enter toggle · ↑↓ select · Esc close";
    pub const ENTER_RESUME: &str = "Enter resume · ↑↓ select · Esc close";
    pub const DRAFT_RETAINED: &str =
        "Selection is explicit; the unsubmitted chat draft is retained.";
    pub const ENABLED: &str = "enabled";
    pub const DISABLED: &str = "disabled";
    pub const SKILL_DETAIL_HINT: &str = "Selection opens details. Use the Agent Skill tools to activate context; selection grants no tools or permissions.";
    pub const PLUGIN_DETAIL_HINT: &str =
        "Selection opens details. Plugin enablement and current permissions remain authoritative.";
    pub const PLUGIN_CLI_HINT: &str =
        "Selection opens details. Use `saya plugin enable|disable` for explicit capability grants.";
    pub const MCP_DETAIL_HINT: &str = "Enter toggles enablement for subsequent runs. Existing approvals and permissions are not reused.";
    pub const SESSION_CURRENT: &str = "This is the currently open session.";
    pub const SESSION_RESUME: &str = "Press Enter to resume this session.";
    pub const ZERO_ITEMS: &str = "0 items";
    pub const ARGV: &str = "argv";
    pub const SOURCE: &str = "Source";
    pub const SCOPE: &str = "Scope";
    pub const SHA256: &str = "SHA-256";
    pub const PLUGIN: &str = "Plugin";
    pub const VERSION: &str = "Version";
    pub const PACKAGE_SHA256: &str = "Package SHA-256";
    pub const ENABLED_SCOPES: &str = "Enabled scopes";
    pub const REQUESTED_CAPABILITIES: &str = "Requested capabilities";
    pub const NETWORK_ORIGINS: &str = "network origins";
    pub const CREDENTIAL_SLOTS: &str = "credential slots";
    pub const TRANSPORT: &str = "Transport";
    pub const READ_ONLY_DECLARATIONS: &str = "Read-only declarations";
    pub const SERVER: &str = "Server";
    pub const SELECTED: &str = "selected";
    pub const STATE: &str = "State";
    pub const PLUGIN_STORE_CHECK: &str = "Plugin store availability could not be checked";
    pub const PLAN_PLUGIN_STORAGE: &str = "PLAN plugin storage must be outside the workspace";
    pub const UNKNOWN_SELECTOR: &str = "Unknown selector";
    pub const MCP_UNAVAILABLE: &str = "Selected MCP server is unavailable";
    pub fn mcp_changed(server: &str, enabled: bool) -> String {
        format!(
            "MCP server {server} {} for subsequent runs.",
            if enabled { ENABLED } else { DISABLED }
        )
    }
    pub fn selected_title(title: &str) -> String {
        format!("{title} · selected")
    }
}

pub(crate) mod tool {
    pub const NULL: &str = "null";
    pub const UNKNOWN: &str = "unknown";
    pub const UNKNOWN_TOOL: &str = "unknown tool";
    pub const STATUS: &str = "Status";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
    pub const OUTPUT: &str = "Output";
    pub const ERROR: &str = "Error";
    pub const MESSAGE: &str = "Message";
    pub const OUTPUT_TRUNCATED: &str = "Output truncated by configured limits.";
    pub const NO_CALLS: &str = "No committed tool calls.";
    pub const ARGUMENTS: &str = "arguments";
    pub const RUN: &str = "run";
    pub const CALL: &str = "call";
    pub const TASK: &str = "task";
    pub const TASKS: &str = "tasks";
    pub const DONE: &str = "done";
    pub const ACTIVE: &str = "active";
    pub const LINES: &str = "lines";
    pub const BYTES: &str = "bytes";
    pub const CREATED: &str = "created";
    pub const UPDATED: &str = "updated";
    pub const ITEM: &str = "item";
    pub const ITEMS: &str = "items";
    pub const FILE: &str = "file";
    pub const FILES: &str = "files";
    pub const MATCH: &str = "match";
    pub const MATCHES: &str = "matches";
    pub const ENTRY: &str = "entry";
    pub const ENTRIES: &str = "entries";
    pub const EXIT: &str = "exit";
    pub const TIMEOUT: &str = "timeout";
    pub const CANCELLED: &str = "cancelled";
}

pub(crate) mod controller {
    pub const IDLE_ONLY: &str = "Runtime settings can be changed while idle";
    pub const PENDING_REQUEST: &str = "Answer or dismiss the current Agent request first";
    pub const SESSION_PROFILE_NOT_FOUND: &str = "session profile not found";
    pub const WORKSPACE_MISSING: &str = "Workspace directory does not exist or is unavailable";
    pub const WORKSPACE_NOT_DIRECTORY: &str = "Workspace must be a directory";
    pub const WORKSPACE_CHANGED: &str =
        "Workspace changed for subsequent runs; Context will refresh on the next execution.";
    pub const PROFILE_REQUIRED: &str = "Choose a configured model profile first";
    pub const PROFILE_UNAVAILABLE: &str = "Selected profile is unavailable";
    pub const EFFORT_USAGE: &str = "Use /effort low|medium|high|xhigh|max|default";
    pub const SANDBOX_USAGE: &str = "Use /sandbox on|off|status";
    pub const SANDBOX_ON: &str = "Sandbox enabled for subsequent runs: platform native backend.";
    pub const SANDBOX_OFF: &str = "Sandbox disabled for subsequent runs. Host execution has no filesystem, network, or syscall isolation.";
    pub const AUTOCOMPACT_USAGE: &str = "Use /autocompact TOKENS|auto|off, for example 950k or 1m";
    pub const AUTOCOMPACT_OFF: &str = "Automatic compaction disabled for subsequent runs.";
    pub const AUTOCOMPACT_TARGET_THRESHOLD: &str =
        "Automatic compaction trigger must be above the target threshold";
    pub const PERMISSIONS_UPDATED: &str = "Permissions updated for subsequent tool calls.";
    pub const GUIDANCE_QUEUED: &str = "Guidance queued for the next model boundary.";
    pub const GUIDANCE_APPROVAL_PENDING: &str =
        "Guidance queued for the next model boundary; the current approval remains pending.";
    pub const MODE_CHANGE_UI: &str = "Mode change waits for the UI operation to stop.";
    pub const UNAVAILABLE_ARGUMENTS: &str = "Unavailable arguments";
    pub const TOOL_LABEL: &str = "Tool";
    pub const CALL_LABEL: &str = "Call";
    pub const APPROVAL_LABEL: &str = "Approval";
    pub const MODE_LABEL: &str = "Mode";
    pub const WORKSPACE_LABEL: &str = "Workspace";
    pub const APPROVAL_HELP: &str = "y approve · n / Esc deny · /mode MODE changes mode";
    pub const INPUT_HELP: &str = "Type a reply and press Enter. Ctrl+C cancels.";
    pub const PENDING_EXPIRED: &str = "The pending question or approval expired.";
    pub const PENDING_MODE: &str =
        "Wait for the pending mode change before answering the current request.";
    pub const WORK_MODE_CHANGED: &str = "work mode changed";
    pub const REVIEW_REQUEST: &str = "Review the current request details before replying.";
    pub const APPROVE_OR_CONTROL: &str = "Approve with y, deny with n, or enter a control command.";
    pub const ACTIVE_RUN_REQUIRED: &str = "active run is unavailable";
    pub const GUIDANCE_ACTIVE_ONLY: &str =
        "Guidance can be added only while an Agent run is active";
    pub const COMPACT_NO_ARGS: &str = "Use /compact without arguments";
    pub const MODE_USAGE: &str = "Use /mode plan|build|code|work";
    pub const PERMISSIONS_IDLE: &str = "Permissions can be changed while idle";
    pub const PERMISSIONS_USAGE: &str = "Use /permissions auto|manual|bypass";
    pub const MARKDOWN_USAGE: &str = "Use /markdown source|render";
    pub const SANDBOX_NATIVE: &str = "on (native at run start)";
    pub const SANDBOX_SEATBELT: &str = "on (Seatbelt)";
    pub const SANDBOX_BUBBLEWRAP: &str = "on (Bubblewrap)";
    pub const SANDBOX_HOST: &str = "off (host compatibility)";
    pub const AUTOCOMPACT_STATUS_OFF: &str = "off";
    pub const AUTOCOMPACT_STATUS_ON_PERCENT: &str = "on at";
    pub const AUTOCOMPACT_STATUS_ON_TOKENS: &str = "on at";
    pub const AUTOCOMPACT_TOKENS_SUFFIX: &str = "tokens";
    pub const STATUS_SESSION: &str = "session";
    pub const STATUS_MODE: &str = "mode";
    pub const STATUS_PENDING: &str = "pending";
    pub const STATUS_PERMISSIONS: &str = "permissions";
    pub const STATUS_SANDBOX: &str = "sandbox";
    pub const STATUS_AUTOCOMPACT: &str = "autocompact";
    pub const STATUS_MARKDOWN: &str = "Markdown";
    pub const FOOTNOTE_USAGE: &str = "Use /footnote NUMBER or /footnote back";
    pub const FOOTNOTE_UNAVAILABLE: &str =
        "Footnote target is unavailable in the retained rendered transcript";
    pub const NO_SESSION: &str = "No session";
    pub const UNKNOWN_COMMAND: &str = "Unknown command. Ctrl+P shows available commands.";
    pub const HELP_TITLE: &str = "Help";
    pub const PROFILE_FIRST_NOTICE: &str =
        "Choose a configured model profile before sending a message.";
    pub const COMPACT_CLEANUP_WAIT: &str = "Wait for cancellation cleanup before compacting";
    pub const COMPACT_RUNNING: &str = "Compaction is already running.";
    pub const COMPACT_REQUESTED: &str = "Compaction requested for the next model boundary.";
    pub const COMPACT_SESSION_REQUIRED: &str = "Create or resume a session before using /compact";
    pub const COMPACT_UI_WAIT: &str = "Compaction waits for the UI operation to finish.";
    pub const COMPACT_SUMMARY: &str = "Compact the saved session context";
    pub const RUN_ACTIVE: &str = "A run is active. Ctrl+C cancels it.";
    pub const UI_WAIT_MESSAGE: &str =
        "Wait for the UI operation or cancel it before sending a message";
    pub const PROFILE_SELECT_MESSAGE: &str =
        "Choose a configured profile with /profiles and /profile NAME";
    pub const SESSION_UNAVAILABLE: &str = "session is unavailable";
    pub const CONVERSATION_SAVE_FAILED: &str = "conversation could not be saved or reloaded";
    pub const RUN_UNAVAILABLE: &str = "run is unavailable";
    pub const RUN_TASK_FAILED_NOTICE: &str =
        "Run task failed. Inspect stored tool records before retrying.";
    pub const RUN_TASK_FAILED: &str = "run task failed";
    pub const CLEAR_DRAFT_FIRST: &str =
        "Clear the current draft before entering command arguments.";
    pub const TUI_TEXT_OUTPUT: &str = "TUI requires text output";
    pub const TERMINAL_BLOCKED: &str = "terminal output remained blocked for two seconds";
    pub const HELP_LINES: &[&str] = &[
        "Enter send · Alt+Enter / Ctrl+J newline",
        "Empty draft · Tab cycles WORK → CODE → PLAN → BUILD",
        "Ctrl+P commands · Ctrl+C cancel · Ctrl+D exit",
        "PgUp/PgDn scroll · End follow output",
        "Click thinking / long tool headers to expand or collapse",
        "Ctrl+O toggles the latest foldable card · wheel scrolls the pane under the pointer",
        "Click a footnote number to jump to its definition; click its definition marker to return",
        "/footnote NUMBER jumps in the newest matching assistant message; /footnote back returns",
        "/markdown source|render selects the assistant presentation",
        "/cd PATH changes this session workspace; /sandbox on|off|status controls native isolation",
        "/effort|/thinking LEVEL changes reasoning effort; /model PROFILE changes model profile",
        "/autocompact TOKENS|auto|off changes the next-run compaction trigger",
        "/skills, /mcps and /plugins open selectors · ↑↓ move · Enter selects · Esc closes",
        "/compact summarizes saved history, or queues at the next model boundary during a run",
        "/memory opens current memory, source/scope, review, forgetting, history and backup controls",
        "Type / for suggestions · ↑/↓ select · Tab/→ complete · Esc dismiss",
    ];

    pub fn workspace(path: &str) -> String {
        format!("Workspace — {path}")
    }
    pub fn thinking_effort(current: &str) -> String {
        format!("Thinking effort — {current} · use low|medium|high|xhigh|max|default")
    }
    pub fn thinking_effort_set(value: &str) -> String {
        format!("Thinking effort set to {value} for subsequent requests.")
    }
    pub fn sandbox(value: &str) -> String {
        format!("Sandbox — {value}")
    }
    pub fn autocompact(value: &str) -> String {
        format!("Automatic compaction — {value}")
    }
    pub fn autocompact_auto(percent: u16) -> String {
        format!("Automatic compaction restored to {percent}%")
    }
    pub fn autocompact_set(tokens: u64) -> String {
        format!("Automatic compaction trigger set to {tokens} input tokens.")
    }
    pub fn mode_requested(mode: &str) -> String {
        format!("Mode requested — {mode}")
    }
    pub fn approval_field(label: &str, value: impl std::fmt::Display) -> String {
        format!("{label} {value}")
    }
    pub fn status_line(status: &str, fields: &[(&str, &str)]) -> String {
        std::iter::once(status.to_owned())
            .chain(
                fields
                    .iter()
                    .map(|(label, value)| format!("{label} {value}")),
            )
            .collect::<Vec<_>>()
            .join(" · ")
    }
    pub fn background_cleanup_failed(error: impl std::fmt::Display) -> String {
        format!("Background cleanup failed — {error}")
    }
    pub fn compact_start_failed(error: impl std::fmt::Display) -> String {
        format!("Compaction could not start — {error}")
    }
    pub fn clipboard_failed(error: impl std::fmt::Display) -> String {
        format!("Clipboard request failed — {error}")
    }
    pub fn command_without_arguments(name: &str) -> String {
        format!("Use {name} without arguments")
    }
    pub fn markdown_display(value: &str) -> String {
        format!("Markdown display — {value}")
    }
    pub fn autocompact_on_tokens(tokens: u64) -> String {
        format!("{AUTOCOMPACT_STATUS_ON_TOKENS} {tokens} {AUTOCOMPACT_TOKENS_SUFFIX}")
    }
    pub fn autocompact_on_percent(percent: u16) -> String {
        format!("{AUTOCOMPACT_STATUS_ON_PERCENT} {percent}%")
    }
    pub fn status_autocompact_tokens(tokens: u64) -> String {
        format!("{tokens} {AUTOCOMPACT_TOKENS_SUFFIX}")
    }
}

pub(crate) mod services {
    pub const UNSENT_DRAFT: &str = "Unsent draft";
    pub const UNSENT_DRAFT_RETAINED: &str =
        "The earlier unsent prompt is retained above. Your current draft is preserved.";
    pub const CANCEL_BEFORE_SESSION: &str = "Cancel the current run before changing sessions";
    pub const UI_ACTIVE: &str = "A UI operation is active; Ctrl+C cancels it";
    pub const LOADING_UI: &str = "Loading UI data. Ctrl+C cancels the operation.";
    pub const CONFIGURED_MODELS_TITLE: &str = "Configured models · UI";
    pub const CONFIGURED_PROFILES_TITLE: &str = "Configured profiles · UI";
    pub const SELECT_MODEL: &str = "Select with /model PROFILE while idle";
    pub const SELECT_PROFILE: &str = "Select with /model PROFILE or /profile PROFILE while idle";
    pub const UNKNOWN_PROFILE: &str =
        "Unknown configured model profile; use /profiles or /model PROFILE";
    pub const MODEL_SELECTED: &str =
        "Model profile selected. New session is waiting for your message.";
    pub const SESSION_ALREADY_OPEN: &str = "Session is already open and waiting for input.";
    pub const SESSION_PREPARED_CANCELLED: &str =
        "Session metadata prepared; view activation cancelled";
    pub const SESSION_PAGE_POSITIVE: &str = "Session page must be positive";
    pub const SESSION_LIST_CANCELLED: &str = "Session list cancelled";
    pub const UNTITLED_SESSION: &str = "Untitled session";
    pub const NO_COMPLETED_RUN: &str = "No completed run";
    pub const WAIT_MODE_EXPORT: &str = "Wait for the pending mode change before exporting";
    pub const EXPORT_USAGE: &str = "Use /export json|markdown PATH";
    pub const SESSION_REQUIRED: &str = "Create or resume a session first";
    pub const BACKGROUND_CANCEL_REQUESTED: &str = "Background cancellation requested";
    pub const BACKGROUND_NOT_FOUND: &str = "Background job was not found";
    pub const NO_ACTIVE_BACKGROUND: &str = "No active run owns a background job";
    pub const UNKNOWN_UI: &str = "Unknown UI command";
    pub const SELECTOR_CANCELLED: &str = "Selector loading cancelled";
    pub const UI_QUERY_CANCELLED: &str = "UI query cancelled";
    pub const HISTORY_OMITTED: &str = "Earlier stored history is omitted from this view. Use session export to inspect the full history.";
    pub const SESSION_RESTORED: &str = "Session restored. Waiting for new input.";
    pub const NEW_SESSION_READY: &str = "New session ready. Waiting for your message.";
    pub const RUNS_TITLE: &str = "Background jobs · UI snapshot";
    pub const TOOLS_TITLE: &str = "Tools · UI snapshot";
    pub const LAST_RUN: &str = "Last run";
    pub const SESSION_LABEL: &str = "Session";
    pub const WORKSPACE_LABEL: &str = "Workspace";
    pub const LAST_STATUS_LABEL: &str = "Last status";
    pub const UPDATED_AT_LABEL: &str = "Updated at";
    pub const UI_OPERATION_WORKER_FAILED: &str = "UI operation worker failed";
    pub const SESSION_LIST_WORKER_FAILED: &str = "Session list worker failed";
    pub const CANNOT_ENCODE_BACKGROUND: &str = "Cannot encode background records";
    pub const CANNOT_DISPLAY_BACKGROUND: &str = "Cannot display background records";
    pub const TOOL_DISPLAY_WORKER_FAILED: &str = "Tool display worker failed";
    pub const EXPORT_ARGUMENTS_UNAVAILABLE: &str = "Export arguments unavailable";
    pub const EXPORT_WORKER_FAILED: &str = "Export worker failed";
    pub const MEMORY_FAILED: &str = "Memory operation failed";
    pub const MEMORY_RECOVERY_HINT: &str = "The pending input was not retried. Retain its operation ID; unfinished WAL recovery uses the explicit saya memory recover CLI command.";

    pub fn sessions_title(page: u64) -> String {
        format!("{} · page {page}", super::picker::SESSIONS_TITLE)
    }
    pub fn stored_records_header(id: &str, change: u64, event: u64) -> String {
        format!("Session {id} · committed change {change} · event {event}")
    }
    pub fn exported(receipt: &str) -> String {
        format!("Exported {receipt}")
    }
    pub fn last_memory_operation(id: &str) -> String {
        format!("Last explicitly confirmed memory operation {id}")
    }
    pub fn memory_failed(message: &str) -> String {
        format!("{MEMORY_FAILED} · {message}")
    }
}

pub(crate) mod messages {
    pub const WAITING_PROVIDER: &str = "waiting for provider thought text";
    pub const WAITING_PROVIDER_TEXT: &str = "Waiting for provider thinking text.";
    pub const PROVIDER_THOUGHT: &str = "provider thought text";
    pub const UNAVAILABLE: &str = "unavailable";
    pub const THINKING_LIMIT: &str = "Thinking text exceeded the display limit or was withheld.";
    pub const RECEIVING_THOUGHT: &str = "Receiving provider thinking text.";
    pub const THINKING_NOT_RETURNED: &str =
        "This provider did not return displayable thinking text.";
    pub const THINKING_UNAVAILABLE_LOG: &str =
        "Thinking text is unavailable in the retained display log.";
    pub const PARTIAL_THOUGHT: &str = "provider thought text · retained partial";
    pub const STORED_THOUGHT: &str = "stored provider thought text";
    pub const TOOL_RUNNING: &str = "Tool execution is running.";
    pub const TOOL_ENDED_UNKNOWN: &str =
        "The run ended before a completion record was displayed. Inspect /tools before retrying.";
    pub const STORED_REQUEST: &str = "stored request";
    pub const STORED_COMPLETED: &str = "stored completed";
    pub const STORED_FAILED: &str = "stored failed";
    pub const STORED_RESULT: &str = "Stored result";
    pub const STORED_RECORDS_HINT: &str =
        "Read by the UI from stored records. Tool calls are identified by run_id and call_id.";
    pub const INTERRUPTED_PARTIAL: &str = "Assistant · interrupted partial";

    pub fn stored_request(call_id: &str) -> String {
        format!("Stored request {call_id} · /tools shows linked records")
    }
}

pub(crate) mod summary {
    use super::common;
    use std::time::Duration;

    pub fn rate(rate: f64, estimated: bool) -> String {
        format!(
            "{}{rate:.1} {}",
            if estimated { "~" } else { "" },
            common::TOKENS_PER_SECOND
        )
    }
    pub fn no_rate() -> String {
        format!("— {}", common::TOKENS_PER_SECOND)
    }
    pub fn tokens(tokens: u64, estimated: bool) -> String {
        format!(
            "{}{} {}",
            if estimated { "~" } else { "" },
            common::token_label(tokens),
            common::TOKENS
        )
    }
    pub fn no_tokens() -> String {
        format!("— {}", common::TOKENS)
    }
    pub fn duration(duration: Duration) -> String {
        let seconds = duration.as_secs();
        if seconds >= 3600 {
            format!(
                "{}h {}m {}s",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60
            )
        } else if seconds >= 60 {
            format!("{}m {}s", seconds / 60, seconds % 60)
        } else {
            format!("{:.1}s", duration.as_secs_f64())
        }
    }
}

pub(crate) mod memory {
    pub const TITLE: &str = " Memory · explicit user administration ";
    pub const HELP: &str =
        "list · search · show · sources · correct · forget · history · rollback · export · restore";
    pub const BUSY: &str = "Working · Ctrl+C cancels; waits for publication outcome";
    pub const CONFIRM: &str = "Review the complete diff · confirm OP_ID REVISION · reject discards";
    pub const COMMANDS: &str =
        "Enter command · PgUp/PgDn scroll · next continues query · Esc closes · help";
    pub const DRAFT_RETAINED: &str =
        "Memory edits are separate from Agent tool approvals. Message draft is retained.";
    pub const VIEW_TRUNCATED: &str = "[memory view display truncated]";
    pub const PENDING_REQUEST: &str =
        "Answer or dismiss the current Agent request before opening memory administration";
    pub const UI_ACTIVE_WAIT: &str = "A UI operation is active; wait for it to finish";
    pub const UI_ACTIVE_CANCEL: &str = "A UI operation is active; wait or cancel it";
    pub const READING_DISABLED: &str =
        "Memory reading is disabled. Use set read on to enable the current process.";
    pub const ESC_DRAFT_HINT: &str =
        "help lists commands; Esc returns to the retained message draft.";
    pub const WORKER_FAILED: &str = "Memory administration worker failed";
    pub const COMMAND_TOO_LONG: &str = "Memory command exceeds its byte limit";
    pub const OPERATION_ACTIVE: &str =
        "Memory operation is still active; Ctrl+C cancels and waits for its outcome";
    pub const IDLE_SETTINGS: &str = "Memory settings can be changed while idle";
    pub const SET_USAGE: &str = "Use set read|inject|write on|off";
    pub const ON_OFF_USAGE: &str = "Use on or off";
    pub const PENDING_DISCARDED: &str = "Pending change discarded.";
    pub const PREPARE_BEFORE_REVIEW: &str = "Prepare a change before review";
    pub const PUBLICATION_IDLE: &str = "Memory publication requires an idle runtime";
    pub const PREPARE_BEFORE_CONFIRM: &str = "Prepare and review a change before confirmation";
    pub const CONFIRMATION_MISMATCH: &str =
        "Confirmation must match the displayed operation ID and complete revision";
    pub const REVIEW_EXPIRED: &str = "Memory review expired";
    pub const CHANGE_IDLE: &str = "Memory changes can be prepared and confirmed while idle";
    pub const NO_NEXT_PAGE: &str = "No further page is available for this query";
    pub const UNKNOWN_COMMAND: &str = "Unknown memory command; use help";
    pub const QUERY_CANCELLED: &str = "Memory query or uncommitted proposal cancelled.";
    pub const PUBLICATION_UNKNOWN: &str = "Memory publication is unknown. Retain its operation receipt and use the explicit saya memory recover command; input will not be replayed.";
    pub const FINISHED_DURING_CANCEL: &str =
        "Memory operation finished during cancellation. Review the recorded outcome.";
    pub const PRESENTATION_TOO_LONG: &str = "Memory presentation exceeds its byte limit";
    pub const HELP_LINES: &[&str] = &[
        "list [PAGE] · search QUERY · show ENTRY_ID|FACT_ID · sources ID · next",
        "correct FACT_ID replacement text · forget FACT_ID",
        "diff MEMORY_CHANGE_JSON · review · confirm OP_ID REVISION · reject",
        "history [PAGE] · rollback VERSION_DIGEST · export PATH · restore PATH",
        "operation OP_ID · unfinished WAL recovery uses the explicit saya memory recover CLI command",
        "set read|inject|write on|off (idle; current process only)",
        "Writes require an explicit version-bound review and confirmation. PLAN is read-only.",
        "Backups cover current global/workspace view, sources, forget constraints and topic references.",
        "Topic bodies, original sessions, existing exports and retained index history keep their owners.",
        "PgUp/PgDn or wheel scroll this modal. Esc preserves your unsent message draft.",
    ];

    pub fn settings(read: bool, inject: bool, write: bool, mode: &str) -> String {
        format!(
            "read {read} · inject {inject}{} · write {write} · {mode} · changes last for this process",
            if read {
                ""
            } else {
                " (inactive while read is off)"
            }
        )
    }
    pub fn set_result(setting: &str, value: bool) -> String {
        format!(
            "{setting} set to {value} for subsequent requests in this process. Reopen list to query the current view."
        )
    }
    pub fn outcome(value: &str) -> String {
        format!("Memory administration outcome — {value}")
    }
    pub fn error(value: &str) -> String {
        format!("Error · {value}")
    }
}

pub(crate) mod rich_text {
    pub const MARKDOWN_SOURCE_BUDGET: &str = "[Markdown source budget exceeded]";
    pub const MATH_SOURCE_FALLBACK: &str = "[math source fallback]";
    pub const MARKDOWN_STRUCTURE_BUDGET: &str = "[Markdown structure budget exceeded]";
    pub const CELL_TRUNCATED: &str = "[cell display truncated]";
    pub const TABLE_TRUNCATED: &str = "[table display truncated]";
    pub const IMAGE_LABEL: &str = "Image · ";
    pub const SOURCE_FALLBACK_SUFFIX: &str = " · source fallback";
    pub const EARLIER_LINES_OMITTED: &str = "[earlier Markdown display lines omitted]";
    pub const CODE_LABEL: &str = "code";
    pub const MATH_LABEL: &str = "math";
    pub const MERMAID_LABEL: &str = "mermaid";
    pub const TABLE_LABEL: &str = "table";
}

pub(crate) mod terminal {
    pub const CONTROL_WRITE_BUDGET: &str = "terminal control write budget exceeded";
    pub const OUTPUT_STATE_UNAVAILABLE: &str = "terminal output state unavailable";
    pub const RESTORATION_FAILED: &str = "terminal restoration previously failed";
    pub const OUTPUT_WRITE_BUDGET: &str = "terminal output write budget exceeded";
    pub const ENTER_DURING_UNWINDING: &str = "cannot enter terminal during unwinding";
    pub const REQUIRES_TTY: &str = "TUI requires terminal stdin and stdout";
    pub const PRESENTATION_ACTIVE: &str = "terminal presentation is already active";
    pub const OWNERSHIP_UNAVAILABLE: &str = "terminal ownership unavailable";
    pub const INITIALIZATION_INCOMPLETE: &str = "terminal initialization is incomplete";
    pub const PRESENTATION_ENDED: &str = "terminal presentation has ended";
    pub const CLIPBOARD_LIMIT: &str = "selection exceeds the 64 KiB clipboard limit";
    pub const INPUT_DURING_UNWINDING: &str = "cannot start terminal input during unwinding";
    pub const INPUT_OWNERSHIP_UNAVAILABLE: &str = "terminal input ownership unavailable";
    pub const INPUT_ACTIVE: &str = "terminal input is already active";
}
