//! `__soyo-js-worker` payload. Runs entirely inside the prepared environment:
//! reads one bounded `WorkerInit` line, executes the script in QuickJS, and
//! answers `done`/`failed`. Every `call` line blocks until the host replies;
//! the worker never touches configuration, storage, logging or credentials.
//!
//! The bridge is sequential and synchronous: each `soyo.callTool` performs a
//! blocking round trip, so `Promise.all` composition stays correct but does
//! not multiplex (a documented v1 boundary).

use super::{
    MAX_WIRE_BYTES, WorkerInit, WorkerMessage,
    protocol::{self, HostMessage},
};
use rquickjs::{Ctx, Function, IntoJs, Promise, Value};
use serde_json::Value as JsonValue;
use std::{
    cell::RefCell,
    io::{BufRead, Write},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

/// Stdio entry point for `internal_command`. Returns a process exit code.
pub(crate) fn run_stdio() -> i32 {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let init = match read_init(&mut stdin.lock()) {
        Ok(init) => init,
        Err(message) => {
            let _ = write_message(&mut stdout.lock(), &WorkerMessage::Failed { message });
            return 64;
        }
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let watchdog = cancel.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(600));
        watchdog.store(true, Ordering::Relaxed);
    });
    let writer = Rc::new(RefCell::new(stdout.lock()));
    let outcome = run(&init, stdin.lock(), writer.clone(), cancel);
    match write_message(&mut *writer.borrow_mut(), &outcome) {
        Ok(()) => 0,
        Err(_) => 74,
    }
}

fn read_init(reader: &mut impl BufRead) -> Result<WorkerInit, String> {
    let line = read_line(reader, MAX_WIRE_BYTES).map_err(|_| "worker input failed".to_owned())?;
    let init: WorkerInit = protocol::decode(&line).map_err(|error| error.0.message)?;
    if init.schema_version != 1
        || init.script.is_empty()
        || init.script.len() > MAX_WIRE_BYTES
        || init.memory_bytes == 0
        || init.stack_bytes == 0
        || init.max_calls == 0
    {
        return Err("invalid worker initialization".into());
    }
    Ok(init)
}

/// Execute the initialized script. Exposed generically over the byte streams
/// so tests can drive the engine without a subprocess. The writer is shared
/// with the bridge so the terminal message can be emitted by the caller.
pub(crate) fn run<R: BufRead + 'static, W: Write + 'static>(
    init: &WorkerInit,
    reader: R,
    writer: Rc<RefCell<W>>,
    cancel: Arc<AtomicBool>,
) -> WorkerMessage {
    match evaluate(init, reader, writer, cancel) {
        Ok(value) => WorkerMessage::Done { value },
        Err(message) => WorkerMessage::Failed { message },
    }
}

struct Bridge<R: BufRead, W: Write> {
    reader: R,
    writer: Rc<RefCell<W>>,
    seq: u32,
    calls: u32,
    max_calls: u32,
}
impl<R: BufRead, W: Write> Bridge<R, W> {
    fn call(&mut self, name: &str, arguments: &str) -> Result<String, String> {
        self.calls += 1;
        if self.calls > self.max_calls {
            return Err(format!(
                "js_repl bridged call budget exceeded ({})",
                self.max_calls
            ));
        }
        if name.is_empty() || name.len() > 128 || arguments.len() > MAX_WIRE_BYTES / 2 {
            return Err("invalid bridged call".into());
        }
        if serde_json::from_str::<JsonValue>(arguments)
            .map(|value| !value.is_object())
            .unwrap_or(true)
        {
            return Err("bridged call arguments must be a JSON object".into());
        }
        let seq = self.seq;
        self.seq += 1;
        write_message(
            &mut *self.writer.borrow_mut(),
            &WorkerMessage::Call {
                seq,
                name: name.to_owned(),
                arguments: arguments.to_owned(),
            },
        )
        .map_err(|_| "bridged call could not be sent".to_owned())?;
        let line = read_line(&mut self.reader, MAX_WIRE_BYTES)
            .map_err(|_| "bridged reply could not be read".to_owned())?;
        match protocol::decode::<HostMessage>(&line)
            .map_err(|_| "bridged reply was not a valid message".to_owned())?
        {
            HostMessage::Result { seq: reply, result } if reply == seq => {
                serde_json::to_string(&result)
                    .map_err(|_| "bridged result could not be encoded".to_owned())
            }
            HostMessage::Abort { message } => Err(message),
            _ => Err("bridged reply sequence violated the protocol".into()),
        }
    }
}

fn evaluate<R: BufRead + 'static, W: Write + 'static>(
    init: &WorkerInit,
    reader: R,
    writer: Rc<RefCell<W>>,
    cancel: Arc<AtomicBool>,
) -> Result<JsonValue, String> {
    let runtime =
        rquickjs::Runtime::new().map_err(|error| format!("engine unavailable: {error}"))?;
    runtime.set_memory_limit(init.memory_bytes);
    runtime.set_max_stack_size(init.stack_bytes);
    runtime.set_interrupt_handler(Some(Box::new(move || cancel.load(Ordering::Relaxed))));
    let context = rquickjs::Context::full(&runtime)
        .map_err(|error| format!("engine context unavailable: {error}"))?;
    context.with(|ctx| -> Result<JsonValue, String> {
        let bridge = Rc::new(RefCell::new(Bridge {
            reader,
            writer,
            seq: 0,
            calls: 0,
            max_calls: init.max_calls,
        }));
        let native = {
            let bridge = bridge.clone();
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'_>, name: String, arguments: String| -> rquickjs::Result<String> {
                    match bridge.borrow_mut().call(&name, &arguments) {
                        Ok(reply) => Ok(reply),
                        Err(message) => {
                            let thrown = message
                                .into_js(&ctx)
                                .unwrap_or_else(|_| Value::new_undefined(ctx.clone()));
                            Err(ctx.throw(thrown))
                        }
                    }
                },
            )
            .map_err(|error| format!("bridge registration failed: {error}"))?
        };
        ctx.globals()
            .set("__soyo_call", native)
            .map_err(|error| format!("bridge registration failed: {error}"))?;
        ctx.eval::<(), _>(
            r#"globalThis.soyo={callTool:(name,args)=>Promise.resolve(JSON.parse(__soyo_call(name,JSON.stringify(args===undefined?{}:args))))};"#,
        )
        .map_err(|error| format!("bridge prelude failed: {error}"))?;
        let wrapped = format!(
            "(async()=>{{const soyo=globalThis.soyo;\n{script}\n}})()",
            script = init.script
        );
        let value = ctx
            .eval::<Value<'_>, _>(wrapped.as_bytes())
            .map_err(|error| js_error(&ctx, error))?;
        let settled = match Promise::from_value(value.clone()) {
            Ok(promise) => settle(&ctx, promise)?,
            Err(_) => value,
        };
        let text = match ctx
            .json_stringify(settled)
            .map_err(|error| format!("result is not serializable: {error}"))?
        {
            Some(text) => text
                .to_string()
                .map_err(|_| "result is not serializable".to_owned())?,
            None => "null".to_owned(),
        };
        serde_json::from_str(&text).map_err(|_| "result is not serializable".to_owned())
    })
}

/// Run the QuickJS job queue until the script's promise settles. `finish`
/// drives pending jobs only; a still-pending promise surfaces `WouldBlock`,
/// which maps to a bounded script-level failure.
fn settle<'js>(ctx: &Ctx<'js>, promise: Promise<'js>) -> Result<Value<'js>, String> {
    let mut current = promise;
    loop {
        match current.finish::<Value>() {
            Ok(value) => match Promise::from_value(value.clone()) {
                Ok(inner) => current = inner,
                Err(_) => return Ok(value),
            },
            Err(rquickjs::Error::Exception) => {
                return Err(describe(ctx.catch()));
            }
            Err(rquickjs::Error::WouldBlock) => {
                return Err("script promise did not resolve".into());
            }
            Err(error) => return Err(format!("engine error: {error}")),
        }
    }
}

fn js_error(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    if matches!(error, rquickjs::Error::Exception) {
        describe(ctx.catch())
    } else {
        format!("engine error: {error}")
    }
}

fn describe(value: Value<'_>) -> String {
    if let Some(exception) = value.as_exception() {
        return exception
            .message()
            .unwrap_or_else(|| "script raised an exception".into());
    }
    value
        .as_string()
        .and_then(|text| text.to_string().ok())
        .unwrap_or_else(|| "script raised an exception".into())
}

fn read_line(reader: &mut impl BufRead, cap: usize) -> std::io::Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let (done, used) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(index) => {
                    line.extend_from_slice(&available[..index]);
                    (true, index + 1)
                }
                None => {
                    line.extend_from_slice(available);
                    (false, available.len())
                }
            }
        };
        reader.consume(used);
        if line.len() > cap {
            return Err(std::io::Error::from_raw_os_error(libc::EFBIG));
        }
        if done {
            return Ok(line);
        }
    }
}

fn write_message(writer: &mut impl Write, message: &WorkerMessage) -> std::io::Result<()> {
    let bytes =
        protocol::encode(message).map_err(|_| std::io::Error::from_raw_os_error(libc::EFBIG))?;
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ToolResult;
    use serde_json::json;
    use std::time::Instant;

    fn init(script: &str) -> WorkerInit {
        WorkerInit {
            schema_version: 1,
            script: script.into(),
            memory_bytes: 32 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            max_calls: 8,
        }
    }
    fn run_script(script: &str) -> (WorkerMessage, Vec<u8>) {
        run_script_with(script, Vec::new(), Arc::new(AtomicBool::new(false)))
    }
    fn run_script_with(
        script: &str,
        replies: Vec<u8>,
        cancel: Arc<AtomicBool>,
    ) -> (WorkerMessage, Vec<u8>) {
        let writer = Rc::new(RefCell::new(Vec::new()));
        let shared = writer.clone();
        let message = run(&init(script), std::io::Cursor::new(replies), writer, cancel);
        (message, shared.borrow().clone())
    }
    fn reply_wire(seq: u32, result: ToolResult) -> Vec<u8> {
        protocol::encode(&HostMessage::Result { seq, result }).unwrap()
    }

    #[test]
    fn plain_values_settle_without_a_bridge() {
        let (message, output) = run_script("return 42;");
        assert!(output.is_empty());
        assert_eq!(message, WorkerMessage::Done { value: json!(42) });
        let (message, _) = run_script("return {ok: true, names: ['a','b']};");
        assert_eq!(
            message,
            WorkerMessage::Done {
                value: json!({"ok": true, "names": ["a", "b"]})
            }
        );
        let (message, _) = run_script("return await Promise.resolve('async value');");
        assert_eq!(
            message,
            WorkerMessage::Done {
                value: json!("async value")
            }
        );
        let (message, _) = run_script("return undefined;");
        assert_eq!(message, WorkerMessage::Done { value: json!(null) });
    }

    #[test]
    fn engine_errors_become_failed() {
        for (script, fragment) in [
            ("throw new Error('script broke');", "script broke"),
            ("return missing_name;", "missing_name"),
            ("syntax error here", "expecting"),
        ] {
            let WorkerMessage::Failed { message } = run_script(script).0 else {
                panic!("{script} should fail");
            };
            assert!(message.contains(fragment), "{message}");
        }
    }

    #[test]
    fn no_host_capabilities_are_exposed() {
        let (message, _) = run_script(
            "return [typeof std,typeof os,typeof require,typeof __soyo_host,typeof process].join('|');",
        );
        assert_eq!(
            message,
            WorkerMessage::Done {
                value: json!("undefined|undefined|undefined|undefined|undefined")
            }
        );
    }

    #[test]
    fn bridged_calls_block_until_a_reply_arrives() {
        let script = "const r = await soyo.callTool('read_file', {path: 'a.txt'}); \
                      const s = await soyo.callTool('list_dir', {}); \
                      return [r.data, s.error ? s.error.code : 'missing'];";
        let replies = [
            reply_wire(0, ToolResult::success(Some(json!("FILE-DATA")))),
            reply_wire(
                1,
                ToolResult::failure("permission_denied", "operation denied"),
            ),
        ]
        .concat();
        let (message, output) = run_script_with(script, replies, Arc::new(AtomicBool::new(false)));
        let calls: Vec<_> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| protocol::decode::<WorkerMessage>(line).unwrap())
            .collect();
        assert_eq!(calls.len(), 2);
        match &calls[0] {
            WorkerMessage::Call {
                seq,
                name,
                arguments,
            } => {
                assert_eq!(*seq, 0);
                assert_eq!(name, "read_file");
                assert_eq!(arguments.as_str(), "{\"path\":\"a.txt\"}");
            }
            _ => panic!("expected a call"),
        }
        match &calls[1] {
            WorkerMessage::Call { seq, name, .. } => {
                assert_eq!(*seq, 1);
                assert_eq!(name, "list_dir");
            }
            _ => panic!("expected a call"),
        }
        assert_eq!(
            message,
            WorkerMessage::Done {
                value: json!(["FILE-DATA", "permission_denied"])
            }
        );
    }

    #[test]
    fn call_budget_and_argument_validation_are_enforced() {
        let mut init = init("soyo.callTool('read_file', {}); return 'done';");
        init.max_calls = 0;
        let writer = Rc::new(RefCell::new(Vec::new()));
        let message = run(
            &init,
            std::io::Cursor::new(Vec::new()),
            writer,
            Arc::new(AtomicBool::new(false)),
        );
        let WorkerMessage::Failed { message } = message else {
            panic!("budget violation should fail");
        };
        assert!(message.contains("budget"), "{message}");
        let (message, _) =
            run_script("soyo.callTool('read_file', 'not-an-object'); return 'done';");
        let WorkerMessage::Failed { message } = message else {
            panic!("invalid arguments should fail");
        };
        assert!(message.contains("JSON"), "{message}");
    }

    #[test]
    fn engine_memory_limit_fails_closed() {
        let mut init = init("const big = new Array(64 * 1024 * 1024).fill(7); return big[0];");
        init.memory_bytes = 8 * 1024 * 1024;
        let writer = Rc::new(RefCell::new(Vec::new()));
        let message = run(
            &init,
            std::io::Cursor::new(Vec::new()),
            writer,
            Arc::new(AtomicBool::new(false)),
        );
        let WorkerMessage::Failed { .. } = message else {
            panic!("memory exhaustion should fail");
        };
    }

    #[test]
    fn interrupt_flag_stops_unbounded_work() {
        let cancel = Arc::new(AtomicBool::new(true));
        let started = Instant::now();
        let (message, _) = run_script_with("while (true) {}", Vec::new(), cancel);
        assert!(started.elapsed() < Duration::from_secs(30));
        let WorkerMessage::Failed { .. } = message else {
            panic!("interrupted script should fail");
        };
    }

    #[test]
    fn abort_reply_surfaces_as_a_failure() {
        let replies = protocol::encode(&HostMessage::Abort {
            message: "host aborted".into(),
        })
        .unwrap();
        let (message, _) = run_script_with(
            "return await soyo.callTool('read_file', {});",
            replies,
            Arc::new(AtomicBool::new(false)),
        );
        let WorkerMessage::Failed { message } = message else {
            panic!("abort should fail");
        };
        assert_eq!(message, "host aborted");
    }
}
