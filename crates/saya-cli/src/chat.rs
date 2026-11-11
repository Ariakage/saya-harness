//! Line-oriented CLI session. Terminal input is read once and routed to either
//! control commands, approval requests or clarification, never competing reads.
use super::*;
use saya_core::{boundary::StoreChange, contract::RunResult, runtime::RuntimeHandle};
use std::{
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    sync::atomic::{AtomicBool, Ordering},
    thread::JoinHandle,
};
use tokio::sync::mpsc;

use saya_app::interaction::{Request as UiRequest, input_channel};
struct Console {
    tty: std::fs::File,
    secrets: Vec<String>,
    lines: mpsc::UnboundedReceiver<Result<String, &'static str>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}
impl Console {
    fn open(secrets: Vec<String>) -> io::Result<Self> {
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/tty")?;
        let mut input = tty.try_clone()?;
        let (sender, lines) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let reader = std::thread::spawn(move || {
            let mut line = Vec::new();
            while !stopped.load(Ordering::Acquire) {
                let mut fd = libc::pollfd {
                    fd: input.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: fd references the owned terminal and the timeout is bounded.
                let ready = unsafe { libc::poll(&mut fd, 1, 100) };
                if ready < 0 {
                    if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    let _ = sender.send(Err("terminal polling failed"));
                    break;
                }
                if ready == 0 {
                    continue;
                }
                let mut byte = [0u8; 1];
                match input.read(&mut byte) {
                    Ok(0) => break,
                    Ok(_) if byte[0] == b'\n' => {
                        let value = std::mem::take(&mut line);
                        let result = String::from_utf8(value)
                            .map(|s| s.trim_end_matches('\r').to_owned())
                            .map_err(|_| "terminal input is not UTF-8");
                        if sender.send(result).is_err() {
                            break;
                        }
                    }
                    Ok(_) if line.len() < 65536 => line.push(byte[0]),
                    Ok(_) => {
                        let _ = sender.send(Err("terminal input is too long"));
                        break;
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(_) => {
                        let _ = sender.send(Err("terminal read failed"));
                        break;
                    }
                }
            }
        });
        Ok(Self {
            tty,
            secrets,
            lines,
            stop,
            reader: Some(reader),
        })
    }
    fn print(&mut self, text: &str) -> io::Result<()> {
        self.tty
            .write_all(redacted_text(text, &self.secrets).as_bytes())?;
        self.tty.flush()
    }
}
impl Drop for Console {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
struct Active {
    kind: RunKind,
    handle: RuntimeHandle,
    cancel: Arc<Cancel>,
    join: tokio::task::JoinHandle<(Result<RunResult, BoundaryError>, Session)>,
    requests: mpsc::Receiver<UiRequest>,
    requests_open: bool,
    streamed: bool,
    cancelling: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Execution,
    Compaction,
}
struct RunSettings<'a> {
    permissions: PermissionMode,
    store: Arc<dyn Store>,
    personal_context: &'a PersonalContext,
    anthropic_prompt_cache: bool,
}
fn start_run(
    config: &Config,
    session: &mut Option<Session>,
    summary: &str,
    kind: RunKind,
    settings: RunSettings<'_>,
) -> Result<Active, Box<dyn std::error::Error>> {
    let current = session.as_ref().ok_or("session missing")?;
    let cancel = Arc::new(Cancel::new());
    let (ui, requests) = input_channel(16)?;
    let launch = Launch {
        permissions: settings.permissions,
        store: settings.store,
        cancellation: cancel.clone(),
        confirmation: Some(ui.clone()),
        input: Some(ui),
        approvals: Preauthorizations::default(),
        personal_context: settings.personal_context.clone(),
        anthropic_prompt_cache: settings.anthropic_prompt_cache,
        events: None,
        run_id: None,
    };
    let (mut runtime, streamed) = build_runtime(config, current, summary, launch)?;
    let handle = runtime.handle();
    let mut current = session.take().ok_or("session missing")?;
    let join = tokio::spawn(async move {
        let result = match kind {
            RunKind::Execution => runtime.run(&mut current).await,
            RunKind::Compaction => runtime.compact(&mut current).await,
        };
        (result, current)
    });
    Ok(Active {
        kind,
        handle,
        cancel,
        join,
        requests,
        requests_open: true,
        streamed: streamed && kind == RunKind::Execution,
        cancelling: false,
    })
}
impl Active {
    fn cancel(&mut self) {
        self.cancel.cancel();
        self.requests.close();
    }
    async fn shutdown(mut self) -> Result<(), BoundaryError> {
        self.cancel();
        let _ = (&mut self.join).await;
        self.shutdown_joined().await
    }
    async fn shutdown_joined(mut self) -> Result<(), BoundaryError> {
        self.cancel();
        self.handle.shutdown_background().await
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        self.cancel();
    }
}
/// Returns true when a second interrupt must force the process to exit.
fn interrupt_active(cancelling: &mut bool, cancel: &Cancel) -> bool {
    if *cancelling {
        return true;
    }
    *cancelling = true;
    cancel.cancel();
    false
}
fn work_mode(input: &str) -> Option<WorkMode> {
    match input.to_ascii_lowercase().as_str() {
        "plan" => Some(WorkMode::Plan),
        "build" => Some(WorkMode::Build),
        "code" => Some(WorkMode::Code),
        "work" => Some(WorkMode::Work),
        _ => None,
    }
}
fn permission(input: &str) -> Option<PermissionMode> {
    match input.to_ascii_lowercase().as_str() {
        "auto" => Some(PermissionMode::Auto),
        "bypass" => Some(PermissionMode::Bypass),
        "manual" => Some(PermissionMode::Manual),
        _ => None,
    }
}
fn display_mode(mode: WorkMode) -> &'static str {
    match mode {
        WorkMode::Plan => "plan",
        WorkMode::Build => "build",
        WorkMode::Code => "code",
        WorkMode::Work => "work",
    }
}
fn display_permission(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Auto => "auto",
        PermissionMode::Bypass => "bypass",
        PermissionMode::Manual => "manual",
    }
}
fn show_prompt(console: &mut Console, request: &UiRequest, config: &Config) -> io::Result<()> {
    match request {
        UiRequest::Approval(request, _) => {
            let args = serde_json::to_string(&request.arguments)
                .unwrap_or_else(|_| "<unavailable>".into());
            let text = format!(
                "\nApproval required\nTool: {}\nArguments: {}\nWorking directory: {}\nMode: {}\nAllow this call? [y/N] ",
                safe_terminal_text(&request.tool),
                args,
                safe_terminal_text(&request.cwd.to_string_lossy()),
                display_mode(request.work_mode)
            );
            console.print(&safe_output(&text, config))
        }
        UiRequest::Input(request, _) => {
            let mut text = format!("\n{}\n", safe_terminal_text(&request.question));
            for (index, option) in request.options.iter().enumerate() {
                text.push_str(&format!(
                    "  {}. {}\n",
                    index + 1,
                    safe_terminal_text(option)
                ));
            }
            text.push_str("Answer: ");
            console.print(&safe_output(&text, config))
        }
    }
}
enum ProfileSelection {
    Selected,
    Exit,
    Interrupted,
}
async fn choose_profile(
    config: &mut Config,
    console: &mut Console,
    interrupts: &mut signals::Signals,
) -> Result<ProfileSelection, Box<dyn std::error::Error>> {
    if config.profile.is_some() {
        return Ok(ProfileSelection::Selected);
    }
    let names: Vec<_> = config.profiles.keys().cloned().collect();
    if names.is_empty() {
        return Err("no profiles configured".into());
    }
    console.print("Select a model profile:\n")?;
    for (index, name) in names.iter().enumerate() {
        console.print(&format!("  {}. {}\n", index + 1, safe_terminal_text(name)))?;
    }
    loop {
        console.print("Profile (number or name; /exit to quit): ")?;
        let line = tokio::select! { biased; _=interrupts.recv()=>return Ok(ProfileSelection::Interrupted), item=console.lines.recv()=>item };
        let Some(line) = line else {
            return Ok(ProfileSelection::Exit);
        };
        let line = line.map_err(io::Error::other)?;
        let text = line.trim();
        if text == "/exit" {
            return Ok(ProfileSelection::Exit);
        }
        let selected = text
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|n| names.get(n))
            .or_else(|| names.iter().find(|n| n.as_str() == text));
        if let Some(name) = selected {
            config.profile = Some(name.clone());
            return Ok(ProfileSelection::Selected);
        }
        console.print("Unknown profile.\n")?;
    }
}
fn answer_pending(pending: UiRequest, line: String) {
    match pending {
        UiRequest::Approval(_, reply) => {
            let yes = matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes");
            let _ = reply.send(Ok(Some(yes)));
        }
        UiRequest::Input(_, reply) => {
            let _ = reply.send(Ok(Some(line)));
        }
    }
}
fn invalidate_approval(pending: &mut Option<UiRequest>) {
    if matches!(pending, Some(UiRequest::Approval(..)))
        && let Some(UiRequest::Approval(_, reply)) = pending.take()
    {
        let _ = reply.send(Err(cli_error("stale_approval", "work mode changed")));
    }
}
fn help(console: &mut Console) -> io::Result<()> {
    console.print("/mode [plan|build|code|work]  /permissions [auto|bypass|manual]\n/compact  /status  /cancel  /jobs  /job ID  /cancel-job ID  /help  /exit\n/compact summarizes saved context, or queues at the next model boundary during a run.\nUse // to send text beginning with /.\n")
}
fn job_display(status: Option<saya_core::background::JobStatus>, config: &Config) -> String {
    use saya_core::background::JobStatus;
    let mut data = match status {
        None => serde_json::json!({"status":"not_found"}),
        Some(JobStatus::Running) => serde_json::json!({"status":"running"}),
        Some(JobStatus::Finished(result)) => {
            serde_json::json!({"status":"finished","result":result})
        }
        Some(JobStatus::Failed(error)) => {
            serde_json::json!({"status":"failed","error":error.0,"observed_result":error.2})
        }
    };
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>();
    let Ok(secrets) = secrets else {
        return "status unavailable".into();
    };
    redact_value(&mut data, &secrets, 0);
    let rendered = data.to_string();
    if contains_encoded_secret(&rendered, &secrets) {
        "sensitive status details withheld".into()
    } else {
        safe_terminal_text(&rendered)
    }
}
/// Start a terminal-only persistent session with policy-controlled tools.
pub async fn chat(
    mut config: Config,
    personal_context: PersonalContext,
    anthropic_prompt_cache: bool,
    existing: Option<session::Prepared>,
) -> Result<u8, Box<dyn std::error::Error>> {
    if config.output != Output::Text {
        return Err("chat requires text output".into());
    }
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    let mut console = Console::open(secrets).map_err(|_| "chat requires a controlling terminal")?;
    let mut interrupts = signals::Signals::new()?;
    let prepared = if let Some(existing) = existing {
        config.profile = Some(existing.session.profile.clone());
        existing
    } else {
        match choose_profile(&mut config, &mut console, &mut interrupts).await? {
            ProfileSelection::Selected => {}
            ProfileSelection::Exit => return Ok(0),
            ProfileSelection::Interrupted => return Ok(130),
        }
        session::create(&config, new_session(&config)?).await?
    };
    if prepared.recovery.interrupted_runs > 0 {
        console.print(&format!("Recovered {} interrupted run(s); {} tool outcome(s) unknown; no tools were replayed.\n", prepared.recovery.interrupted_runs, prepared.recovery.unknown_tools))?;
    }
    let workspace = prepared.session.workspace.clone();
    let media = prepared.media.clone();
    let media_capabilities = prepared.media_capabilities;
    let mut session = Some(prepared.session);
    let store = prepared.store;
    let _session_lock = prepared._lock;
    let mut permissions = config.permissions_mode;
    let mut active: Option<Active> = None;
    let mut pending: Option<UiRequest> = None;
    let mut exit_after_run: Option<u8> = None;
    let mut terminal_open = true;
    let outcome = async {
        console.print("Saya chat (persistent session). Type /help for commands.\n> ")?;
        loop {
            enum Event {
                Line(Option<Result<String, &'static str>>),
                Request(Option<UiRequest>),
                Finished(Result<(Result<RunResult, BoundaryError>, Session), tokio::task::JoinError>),
                Interrupt(Option<signals::Received>),
            }
            let event = if let Some(run) = active.as_mut() {
                tokio::select! { biased;
                    sig=interrupts.recv()=>Event::Interrupt(sig),
                    result=&mut run.join=>Event::Finished(result),
                    request=run.requests.recv(), if run.requests_open=>Event::Request(request),
                    line=console.lines.recv(), if terminal_open=>Event::Line(line),
                }
            } else {
                tokio::select! { biased; sig=interrupts.recv()=>Event::Interrupt(sig), line=console.lines.recv()=>Event::Line(line) }
            };
            match event {
                Event::Interrupt(result) => {
                    if result.is_none() {
                        return Err("signal listener closed".into());
                    }
                    if let Some(run) = active.as_mut() {
                        if result == Some(signals::Received::Terminate) {
                            exit_after_run = Some(130);
                            run.cancelling = true;
                            run.cancel.cancel();
                        } else if interrupt_active(&mut run.cancelling, &run.cancel) {
                            std::process::exit(130);
                        }
                        pending = None;
                        console.print("\nCancelling run...\n")?;
                    } else {
                        console.print("\nInterrupted.\n")?;
                        return Ok(130);
                    }
                }
                Event::Finished(result) => {
                    let old = active.take().ok_or("missing active run")?;
                    let compact_after_run = old.kind == RunKind::Execution && !old.cancelling && !old.cancel.is_cancelled() && old.handle.compaction_pending();
                    let (outcome, finished) = match result {
                        Ok(result) => result,
                        Err(_) => {
                            old.shutdown_joined().await?;
                            eprintln!("saya: run task failed");
                            return Ok(1);
                        }
                    };
                    pending = None;
                    let mut completed = false;
                    match outcome {
                        Ok(result) => {
                            completed = result.status == RunStatus::Completed;
                            log_run_result(&config, &result);
                            report_result(&result, &finished, old.streamed, &config);
                        }
                        Err(error) => {
                            log_warning(
                                &config,
                                "error",
                                "run.failed",
                                serde_json::json!({"session_id":finished.id,"error":error.0}),
                            );
                            report_runtime_error(&error, &config);
                            if error.0.code == "storage_error" || error.2.is_some() {
                                return Ok(1);
                            }
                        }
                    }
                    session = Some(finished);
                    if let Some(code) = exit_after_run {
                        if terminal_open {
                            console.print("Goodbye.\n")?;
                        }
                        return Ok(code);
                    }
                    if compact_after_run && completed {
                        match start_run(&config, &mut session, "Compact the saved session context", RunKind::Compaction, RunSettings {
                            permissions, store: store.clone(), personal_context: &personal_context, anthropic_prompt_cache,
                        }) {
                            Ok(run) => { active = Some(run); console.print("Compacting context...\n")?; continue; }
                            Err(error) => console.print(&format!("saya: {}\n", safe_terminal_text(&error.to_string())))?,
                        }
                    }
                    console.print("> ")?;
                }
                Event::Request(Some(request)) => {
                    if terminal_open && pending.is_none() {
                        show_prompt(&mut console, &request, &config)?;
                        pending = Some(request);
                    }
                    // Concurrent requests are not expected; dropping the reply fails closed.
                }
                Event::Request(None) => {
                    if let Some(run) = active.as_mut() {
                        run.requests_open = false;
                    }
                }
                Event::Line(None) => {
                    if let Some(run) = active.as_mut() {
                        terminal_open = false;
                        run.cancelling = true;
                        run.cancel.cancel();
                        pending = None;
                        exit_after_run = Some(0);
                        continue;
                    }
                    return Ok(0);
                }
                Event::Line(Some(Err(error))) => {
                    if let Some(run) = active.as_mut() {
                        eprintln!("saya: {error}");
                        terminal_open = false;
                        run.cancelling = true;
                        run.cancel.cancel();
                        pending = None;
                        exit_after_run = Some(1);
                        continue;
                    }
                    return Err(error.into());
                }
                Event::Line(Some(Ok(line))) => {
                    let escaped = line.starts_with("//");
                    let text = if escaped { line[1..].to_owned() } else { line };
                    if !escaped && text.starts_with('/') {
                        let mut words = text.split_whitespace();
                        let command = words.next().unwrap_or("");
                        let arg = words.next();
                        if words.next().is_some()
                            || (arg.is_some()
                                && matches!(
                                    command,
                                    "/help" | "/status" | "/cancel" | "/exit" | "/jobs" | "/compact"
                                ))
                        {
                            console.print("Invalid command arguments.\n")?;
                            if active.is_none() {
                                console.print("> ")?;
                            } else if let Some(request) = pending.as_ref() {
                                show_prompt(&mut console, request, &config)?;
                            }
                            continue;
                        }
                        match command {
                            "/help" => help(&mut console)?,
                            "/exit" => {
                                if let Some(run) = active.as_mut() {
                                    run.cancelling = true;
                                    run.cancel.cancel();
                                    pending = None;
                                    exit_after_run = Some(0);
                                    console.print("Cancelling run...\n")?;
                                } else {
                                    console.print("Goodbye.\n")?;
                                    return Ok(0);
                                }
                            }
                            "/cancel" => {
                                if let Some(run) = active.as_mut() {
                                    run.cancelling = true;
                                    run.cancel.cancel();
                                    pending = None;
                                    console.print("Cancelling run...\n")?;
                                } else {
                                    console.print("No active run.\n")?;
                                }
                            }
                            "/compact" => {
                                if let Some(run) = active.as_ref() {
                                    if run.cancelling {
                                        console.print("Wait for cancellation cleanup before compacting.\n")?;
                                    } else if run.kind == RunKind::Compaction {
                                        console.print("Compaction is already running.\n")?;
                                    } else {
                                        run.handle.request_compaction()?;
                                        console.print("Compaction requested for the next model boundary.\n")?;
                                    }
                                } else {
                                    match start_run(&config, &mut session, "Compact the saved session context", RunKind::Compaction, RunSettings {
                                        permissions, store: store.clone(), personal_context: &personal_context, anthropic_prompt_cache,
                                    }) {
                                        Ok(run) => { active = Some(run); console.print("Compacting context...\n")?; }
                                        Err(error) => console.print(&format!("saya: {}\n", safe_terminal_text(&error.to_string())))?,
                                    }
                                }
                            }
                            "/jobs" => {
                                if let Some(run) = active.as_ref() {
                                    let mut count = 0usize;
                                    for event in run.handle.background_events()? {
                                        if let saya_core::background::BackgroundEventKind::Accepted {
                                            tool,
                                            ..
                                        } = event.kind
                                        {
                                            count += 1;
                                            let status = job_display(
                                                run.handle.background_status(&event.job_id)?,
                                                &config,
                                            );
                                            console.print(&format!(
                                                "{} {} {}\n",
                                                safe_terminal_text(&event.job_id),
                                                safe_terminal_text(&tool),
                                                status
                                            ))?;
                                        }
                                    }
                                    if count == 0 {
                                        console.print("No background jobs.\n")?;
                                    }
                                } else {
                                    console.print("No active run.\n")?;
                                }
                            }
                            "/job" => {
                                if let (Some(run), Some(id)) = (active.as_ref(), arg) {
                                    console.print(&format!(
                                        "{}\n",
                                        job_display(run.handle.background_status(id)?, &config)
                                    ))?;
                                } else {
                                    console.print("Usage: /job ID during an active run.\n")?;
                                }
                            }
                            "/cancel-job" => {
                                if let (Some(run), Some(id)) = (active.as_ref(), arg) {
                                    match run.handle.background_status(id)? {
                                        Some(saya_core::background::JobStatus::Running) => {
                                            run.handle.cancel_background(id)?;
                                            console.print("Background cancellation requested.\n")?;
                                        }
                                        Some(_) => {
                                            console.print("Background job already finished.\n")?
                                        }
                                        None => console.print("Background job not found.\n")?,
                                    }
                                } else {
                                    console.print("Usage: /cancel-job ID during an active run.\n")?;
                                }
                            }
                            "/mode" => {
                                if let Some(arg) = arg {
                                    if let Some(mode) = work_mode(arg) {
                                        if let Err(e) = session::check_roots(&config, &workspace, mode)
                                        {
                                            console.print(&format!(
                                                "saya: {}\n> ",
                                                safe_terminal_text(&e.to_string())
                                            ))?;
                                            continue;
                                        }
                                        if let Some(run) = active.as_ref() {
                                            let before = run.handle.mode()?;
                                            run.handle.request_mode(mode)?;
                                            let after = run.handle.mode()?;
                                            if after.revision != before.revision
                                                || after.pending.is_some()
                                            {
                                                invalidate_approval(&mut pending);
                                            }
                                            console.print(&format!(
                                                "Mode: {}{}\n",
                                                display_mode(after.current),
                                                after.pending.map_or(String::new(), |m| format!(
                                                    "; pending {}",
                                                    display_mode(m)
                                                ))
                                            ))?;
                                        } else if let Some(current) = session.as_mut() {
                                            current.work_mode = mode;
                                            store
                                                .commit(&[StoreChange::Session(current.clone())])
                                                .await?;
                                            console
                                                .print(&format!("Mode: {}\n", display_mode(mode)))?;
                                        }
                                    } else {
                                        console.print("Unknown mode.\n")?;
                                    }
                                } else if let Some(run) = active.as_ref() {
                                    let snap = run.handle.mode()?;
                                    console.print(&format!(
                                        "Mode: {}{}\n",
                                        display_mode(snap.current),
                                        snap.pending.map_or(String::new(), |m| format!(
                                            "; pending {}",
                                            display_mode(m)
                                        ))
                                    ))?;
                                } else if let Some(current) = session.as_ref() {
                                    console.print(&format!(
                                        "Mode: {}\n",
                                        display_mode(current.work_mode)
                                    ))?;
                                }
                            }
                            "/permissions" => {
                                if let Some(arg) = arg {
                                    if active.is_some() {
                                        console.print("Permissions can only change while idle.\n")?;
                                    } else if let Some(value) = permission(arg) {
                                        permissions = value;
                                        console.print(&format!(
                                            "Permissions: {}\n",
                                            display_permission(value)
                                        ))?;
                                    } else {
                                        console.print("Unknown permission mode.\n")?;
                                    }
                                } else {
                                    console.print(&format!(
                                        "Permissions: {}\n",
                                        display_permission(permissions)
                                    ))?;
                                }
                            }
                            "/status" => {
                                let (mode, pending_mode) = if let Some(run) = active.as_ref() {
                                    let snap = run.handle.mode()?;
                                    (snap.current, snap.pending)
                                } else {
                                    (session.as_ref().ok_or("session missing")?.work_mode, None)
                                };
                                console.print(&format!(
                                    "Status: {}; mode: {}; pending: {}; permissions: {}\n",
                                    if active.is_some() { "running" } else { "idle" },
                                    display_mode(mode),
                                    pending_mode.map_or("none", display_mode),
                                    display_permission(permissions)
                                ))?;
                            }
                            _ => console.print("Unknown command; use /help.\n")?,
                        }
                        if active.is_none() {
                            console.print("> ")?;
                        } else if let Some(request) = pending.as_ref() {
                            show_prompt(&mut console, request, &config)?;
                        }
                        continue;
                    }
                    if let Some(request) = pending.take() {
                        answer_pending(request, text);
                        continue;
                    }
                    if active.is_some() {
                        console.print("Run in progress; use /compact, /status, /mode or /cancel.\n")?;
                        continue;
                    }
                    if text.trim().is_empty() {
                        console.print("> ")?;
                        continue;
                    }
                    let current_id = session.as_ref().ok_or("session missing")?.id;
                    let capture = saya_core::media::capture_user(
                        &text,
                        media_capabilities,
                        &media,
                        current_id,
                    )?;
                    let item = HistoryItem::UserText(capture.text.clone());
                    store
                        .commit(&[StoreChange::History {
                            session_id: current_id,
                            item: item.clone(),
                        }])
                        .await?;
                    drop(capture);
                    session = Some(store
                        .load_session(current_id)
                        .await?
                        .ok_or("session not found")?);
                    match start_run(&config, &mut session, &text, RunKind::Execution, RunSettings {
                        permissions, store: store.clone(), personal_context: &personal_context, anthropic_prompt_cache,
                    }) {
                        Ok(run) => active = Some(run),
                        Err(e) => {
                            console
                                .print(&format!("saya: {}\n> ", safe_terminal_text(&e.to_string())))?;
                            continue;
                        }
                    }
                }
            }
        }
    }.await;
    pending.take();
    if let Some(run) = active.take() {
        run.shutdown().await?;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_interrupt_cancels_and_second_forces_exit() {
        let cancel = Cancel::new();
        let mut cancelling = false;
        assert!(!interrupt_active(&mut cancelling, &cancel));
        assert!(cancel.is_cancelled());
        assert!(interrupt_active(&mut cancelling, &cancel));
    }

    #[tokio::test]
    async fn exceptional_cleanup_awaits_the_task_before_releasing_the_session_lock() {
        for panic in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("config.toml");
            std::fs::write(&path, "schema_version=1\n[defaults]\nprofile='fixture'\n[profiles.fixture]\nprotocol='openai-chat-completions'\nbase_url='http://127.0.0.1:1/v1'\nmodel='fixture'\nauth='none'\ncontext_window_tokens=128000\n").unwrap();
            let config = config::load(
                Overrides {
                    config: Some(path),
                    ..Default::default()
                },
                Environment::from_pairs([("HOME".into(), root.path().as_os_str().into())]),
                root.path(),
            )
            .unwrap();
            let prepared = session::create(&config, new_session(&config).unwrap())
                .await
                .unwrap();
            let cancel = Arc::new(Cancel::new());
            let (runtime, _) = build_runtime(
                &config,
                &prepared.session,
                "fixture",
                Launch {
                    permissions: PermissionMode::Auto,
                    store: prepared.store.clone(),
                    cancellation: cancel.clone(),
                    confirmation: None,
                    input: None,
                    approvals: Preauthorizations::default(),
                    personal_context: PersonalContext::default(),
                    anthropic_prompt_cache: false,
                    events: None,
                    run_id: None,
                },
            )
            .unwrap();
            let handle = runtime.handle();
            let current = prepared.session.clone();
            let data = config.data_dir.clone();
            let task_cancel = cancel.clone();
            let completed = Arc::new(AtomicBool::new(false));
            let task_completed = completed.clone();
            let join = tokio::spawn(async move {
                if panic {
                    panic!("controlled task failure");
                }
                task_cancel.cancelled().await;
                tokio::task::yield_now().await;
                assert_eq!(
                    saya_core::sqlite::SqliteStore::lock_existing(&data, current.id)
                        .err()
                        .unwrap()
                        .0
                        .code,
                    "session_busy"
                );
                task_completed.store(true, Ordering::Release);
                (Err(cli_error("fixture", "controlled failure")), current)
            });
            let (_, requests) = mpsc::channel(1);
            let mut active = Active {
                kind: RunKind::Execution,
                handle,
                cancel: cancel.clone(),
                join,
                requests,
                requests_open: true,
                streamed: false,
                cancelling: false,
            };
            if panic {
                assert!((&mut active.join).await.is_err());
                active.shutdown_joined().await.unwrap();
            } else {
                active.shutdown().await.unwrap();
                assert!(completed.load(Ordering::Acquire));
            }
            assert!(cancel.is_cancelled());
            let id = prepared.session.id;
            drop(prepared);
            assert!(saya_core::sqlite::SqliteStore::lock_existing(&config.data_dir, id).is_ok());
        }
    }
}
