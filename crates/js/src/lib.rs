//! Code-mode tools: the model writes one JavaScript program, we run it in a
//! fresh QuickJS VM where every tool is an async function.
//!
//! Why code instead of one tool call per action (the idea Tura's `command_run`
//! proved: fewer round trips, fewer replayed tokens), but stronger: `await` is a
//! dependency step, `Promise.all` is a parallel step, variables bind outputs,
//! a thrown error stops the batch, and the script returns only what the model
//! needs to see instead of every raw tool output.
//!
//! The VM has no filesystem, network or process access of its own; the tool
//! functions below are the only way out. Each finished call is reported as a
//! [`Call`] for the UI's tool log.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rquickjs::prelude::Async;
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Function, Promise};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

/// Tool functions the script can call, for the model-facing description.
pub const TOOLS: &str = "\
read(path, {offset?, limit?}) -> string      lines offset..offset+limit (1-based) or whole file
write(path, text)                            create or overwrite, makes parent dirs
edit(path, old, new, {all?})                 exact text replace; throws if old is missing or ambiguous
ls(path='.') -> string[]                     sorted names, directories end with /
sh(cmd, {cwd?, timeout?}) -> {code, out, err}  bash -c; timeout ms (default 120000); never throws on exit code
console.log(...)                             add a line to the output";

/// Model-visible output is cut to this many characters (head + tail kept).
pub const MAX_OUTPUT: usize = 10_000;

/// One finished tool call, for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// read, write, edit, ls, sh
    pub verb: String,
    /// Path or command, single line.
    pub target: String,
    /// Short result: "412 lines", "+3 −1", "✓ 1.2s", or the error.
    pub info: String,
    pub ok: bool,
}

/// What the model gets back, plus totals for the log.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// Console lines + return value (or the error), truncated to MAX_OUTPUT.
    pub text: String,
    pub error: Option<String>,
    pub calls: usize,
    pub elapsed: Duration,
}

pub struct Limits {
    pub timeout: Duration,
    pub memory: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            memory: 256 << 20,
        }
    }
}

struct Env {
    cwd: PathBuf,
    log: UnboundedSender<Call>,
}

/// Script title for the log: its first `//` comment, else its first line.
pub fn title(code: &str) -> String {
    let first = code
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let text = first.strip_prefix("//").map(str::trim).unwrap_or(first);
    text.chars().take(80).collect()
}

/// Run `code` as the body of an async function in a fresh VM rooted at `cwd`.
pub async fn run(code: &str, cwd: &Path, limits: Limits, log: UnboundedSender<Call>) -> Outcome {
    let start = Instant::now();
    let env = Arc::new(Env {
        cwd: cwd.to_path_buf(),
        log,
    });
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let result =
        tokio::time::timeout(limits.timeout, execute(code, env, calls.clone(), &limits)).await;
    let (out, error) = match result {
        Ok(Ok((out, value))) => (join(&out, value.as_deref()), None),
        Ok(Err((out, err))) => (join(&out, Some(&format!("error: {err}"))), Some(err)),
        Err(_) => {
            let err = format!("timed out after {}s", limits.timeout.as_secs());
            (format!("error: {err}"), Some(err))
        }
    };
    let calls = calls.load(std::sync::atomic::Ordering::Relaxed);
    let text = if out.trim().is_empty() {
        format!("ok · {calls} calls · no output (return or console.log what you need to see)")
    } else {
        clip(&out, MAX_OUTPUT)
    };
    Outcome {
        text,
        error,
        calls,
        elapsed: start.elapsed(),
    }
}

fn join(out: &str, value: Option<&str>) -> String {
    match value {
        Some(value) if out.is_empty() => value.to_string(),
        Some(value) => format!("{out}\n{value}"),
        None => out.to_string(),
    }
}

/// Glue: one host function `__call(name, argsJson) -> resultJson`; the
/// prelude turns it into throwing async functions and captures console.
const PRELUDE: &str = r#"
const __out = [];
const __fmt = (x) => typeof x === 'string' ? x : JSON.stringify(x);
globalThis.console = { log: (...a) => { __out.push(a.map(__fmt).join(' ')); } };
console.error = console.warn = console.info = console.log;
for (const name of ['read', 'write', 'edit', 'ls', 'sh']) {
  globalThis[name] = async (...args) => {
    const r = JSON.parse(await __call(name, JSON.stringify(args)));
    if ('error' in r) throw new Error(r.error);
    return r.value;
  };
}
"#;

type Lines = (String, Option<String>);

async fn execute(
    code: &str,
    env: Arc<Env>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    limits: &Limits,
) -> Result<Lines, (String, String)> {
    let fail = |err: String| (String::new(), err);
    let runtime = AsyncRuntime::new().map_err(|err| fail(err.to_string()))?;
    runtime.set_memory_limit(limits.memory).await;
    // CPU loops never yield to the tokio timeout; the interrupt handler stops them.
    let deadline = Instant::now() + limits.timeout;
    runtime
        .set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)))
        .await;
    let context = AsyncContext::full(&runtime)
        .await
        .map_err(|err| fail(err.to_string()))?;
    let script = format!(
        "{PRELUDE}\n(async () => {{\n  let v;\n  try {{ v = await (async () => {{\n{code}\n}})(); }}\n  catch (e) {{ return JSON.stringify({{ out: __out.join('\\n'), error: String(e && e.message !== undefined ? e.message : e) }}); }}\n  return JSON.stringify({{ out: __out.join('\\n'), value: v === undefined ? null : __fmt(v) }});\n}})()"
    );
    let raw: Result<String, String> = context
        .async_with(async move |ctx| {
            let call = Function::new(
                ctx.clone(),
                Async(move |name: String, args: String| {
                    let env = env.clone();
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        dispatch(&env, &name, &args).await.to_string()
                    }
                }),
            );
            let call = match call {
                Ok(call) => call,
                Err(err) => return Err(err.to_string()),
            };
            if let Err(err) = ctx.globals().set("__call", call) {
                return Err(err.to_string());
            }
            let promise: Promise = match ctx.eval(script).catch(&ctx) {
                Ok(promise) => promise,
                Err(err) => return Err(err.to_string()),
            };
            promise
                .into_future::<String>()
                .await
                .catch(&ctx)
                .map_err(|err| err.to_string())
        })
        .await;
    let raw = raw.map_err(fail)?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|err| fail(err.to_string()))?;
    let out = parsed["out"].as_str().unwrap_or_default().to_string();
    match parsed.get("error").and_then(Value::as_str) {
        Some(err) => Err((out, err.to_string())),
        None => Ok((out, parsed["value"].as_str().map(String::from))),
    }
}

/// Run one tool, log it, and answer `{"value": …}` or `{"error": "…"}`.
async fn dispatch(env: &Env, name: &str, args: &str) -> Value {
    let args: Vec<Value> = serde_json::from_str(args).unwrap_or_default();
    let text = |index: usize| {
        args.get(index)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let target = match name {
        "sh" => one_line(&text(0)),
        "ls" if text(0).is_empty() => ".".into(),
        _ => text(0),
    };
    let result = match name {
        "read" => read(env, &text(0), args.get(1)),
        "write" => write(env, &text(0), &text(1)),
        "edit" => edit(env, &text(0), &text(1), &text(2), args.get(3)),
        "ls" => ls(env, &target),
        "sh" => sh(env, &text(0), args.get(1)).await,
        other => Err(format!("unknown tool {other}")),
    };
    let (answer, info, ok) = match result {
        Ok((value, info)) => (json!({ "value": value }), info, true),
        Err(err) => (json!({ "error": err }), err, false),
    };
    let _ = env.log.send(Call {
        verb: name.to_string(),
        target,
        info: one_line(&info),
        ok,
    });
    answer
}

type Done = Result<(Value, String), String>;

fn path(env: &Env, path: &str) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("missing path".into());
    }
    Ok(env.cwd.join(path))
}

fn read(env: &Env, file: &str, opts: Option<&Value>) -> Done {
    let text = std::fs::read_to_string(path(env, file)?).map_err(|err| err.to_string())?;
    let total = text.lines().count();
    let offset = opts.and_then(|o| o["offset"].as_u64()).unwrap_or(1).max(1) as usize;
    let limit = opts.and_then(|o| o["limit"].as_u64()).map(|n| n as usize);
    if offset == 1 && limit.is_none() {
        return Ok((json!(text), format!("{total} lines")));
    }
    let lines: Vec<&str> = text
        .lines()
        .skip(offset - 1)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    let end = offset - 1 + lines.len();
    Ok((
        json!(lines.join("\n")),
        format!("L{offset}-{end} of {total}"),
    ))
}

fn write(env: &Env, file: &str, text: &str) -> Done {
    let target = path(env, file)?;
    let existed = target.exists();
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    std::fs::write(&target, text).map_err(|err| err.to_string())?;
    let lines = text.lines().count();
    let info = if existed {
        format!("{lines} lines")
    } else {
        format!("new · {lines} lines")
    };
    Ok((Value::Null, info))
}

fn edit(env: &Env, file: &str, old: &str, new: &str, opts: Option<&Value>) -> Done {
    let target = path(env, file)?;
    let text = std::fs::read_to_string(&target).map_err(|err| err.to_string())?;
    if old.is_empty() {
        return Err("old text is empty".into());
    }
    let count = text.matches(old).count();
    let all = opts.and_then(|o| o["all"].as_bool()).unwrap_or(false);
    match count {
        0 => return Err(format!("old text not found in {file}")),
        n if n > 1 && !all => {
            return Err(format!(
                "old text matches {n} times in {file}; add context or pass {{all:true}}"
            ));
        }
        _ => {}
    }
    std::fs::write(&target, text.replace(old, new)).map_err(|err| err.to_string())?;
    let lines = |s: &str| s.lines().count().max(1) * count;
    Ok((Value::Null, format!("+{} −{}", lines(new), lines(old))))
}

fn ls(env: &Env, dir: &str) -> Done {
    let mut names: Vec<String> = std::fs::read_dir(path(env, dir)?)
        .map_err(|err| err.to_string())?
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    names.sort();
    let info = format!("{} entries", names.len());
    Ok((json!(names), info))
}

async fn sh(env: &Env, cmd: &str, opts: Option<&Value>) -> Done {
    if cmd.trim().is_empty() {
        return Err("empty command".into());
    }
    let cwd = match opts.and_then(|o| o["cwd"].as_str()) {
        Some(dir) => env.cwd.join(dir),
        None => env.cwd.clone(),
    };
    let timeout = opts.and_then(|o| o["timeout"].as_u64()).unwrap_or(120_000);
    let start = Instant::now();
    let child = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(Duration::from_millis(timeout), child).await {
        Ok(output) => output.map_err(|err| err.to_string())?,
        Err(_) => return Err(format!("timed out after {}s", timeout / 1000)),
    };
    let secs = start.elapsed().as_secs_f64();
    let code = output.status.code().unwrap_or(-1);
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    let info = if code == 0 {
        format!("✓ {secs:.1}s · {} lines", out.lines().count())
    } else {
        format!("exit {code} · {secs:.1}s")
    };
    let value = json!({
        "code": code,
        "out": clip(out.trim_end(), MAX_OUTPUT),
        "err": clip(err.trim_end(), MAX_OUTPUT),
    });
    // A failing command is still a successful call: the script decides.
    Ok((value, info))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Keep head and tail of long output, Tura-style: the model sees how much
/// was there and both ends, where errors and summaries live.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep = max / 2;
    let head: String = text.chars().take(keep).collect();
    let tail: String = {
        let mut tail: Vec<char> = text.chars().rev().take(keep).collect();
        tail.reverse();
        tail.into_iter().collect()
    };
    let cut = text.chars().count() - 2 * keep;
    format!(
        "Total output lines: {}\n\n{head}…{cut} characters truncated…{tail}",
        text.lines().count()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn script(code: &str, dir: &Path) -> (Outcome, Vec<Call>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = run(
            code,
            dir,
            Limits {
                timeout: Duration::from_secs(5),
                ..Limits::default()
            },
            tx,
        )
        .await;
        let mut calls = Vec::new();
        while let Ok(call) = rx.try_recv() {
            calls.push(call);
        }
        (outcome, calls)
    }

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("smash-js-{}-{}", std::process::id(), rand()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rand() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[tokio::test]
    async fn tools_round_trip() {
        let dir = temp();
        let (outcome, calls) = script(
            r#"// edit a file
            await write('a/b.txt', 'one\ntwo\nthree');
            await edit('a/b.txt', 'two', 'TWO');
            const [names, part] = await Promise.all([ls('a'), read('a/b.txt', { offset: 2, limit: 1 })]);
            const r = await sh('echo hi && exit 3');
            console.log(names, part);
            return { code: r.code, out: r.out };"#,
            &dir,
        )
        .await;
        assert_eq!(outcome.error, None, "{}", outcome.text);
        assert_eq!(outcome.text, "[\"b.txt\"] TWO\n{\"code\":3,\"out\":\"hi\"}");
        assert_eq!(outcome.calls, 5);
        let verbs: Vec<_> = calls.iter().map(|c| (c.verb.as_str(), c.ok)).collect();
        assert_eq!(verbs[..2], [("write", true), ("edit", true)]);
        assert!(
            calls
                .iter()
                .any(|c| c.verb == "sh" && c.info.starts_with("exit 3"))
        );
        assert_eq!(
            title(
                "  // edit a file
x"
            ),
            "edit a file"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn errors_stop_the_script_and_keep_output() {
        let dir = temp();
        let (outcome, calls) = script(
            "console.log('before'); await read('missing.txt'); console.log('after');",
            &dir,
        )
        .await;
        assert!(outcome.error.is_some());
        assert!(
            outcome.text.starts_with("before\nerror: "),
            "{}",
            outcome.text
        );
        assert!(!outcome.text.contains("after"));
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].ok);
        let (edit, _) = script("await write('x', 'a a'); await edit('x', 'a', 'b');", &dir).await;
        assert!(edit.text.contains("matches 2 times"), "{}", edit.text);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn sandbox_and_limits() {
        let dir = temp();
        let (outcome, _) = script(
            "return [typeof require, typeof process, typeof std, typeof os].join(',')",
            &dir,
        )
        .await;
        assert_eq!(outcome.text, "undefined,undefined,undefined,undefined");
        let (syntax, _) = script("return (", &dir).await;
        assert!(syntax.error.is_some());
        let (looping, _) = script_with("while (true) {}", &dir, Duration::from_millis(200)).await;
        assert!(looping.error.is_some(), "{}", looping.text);
        let (empty, _) = script("await ls('.')", &dir).await;
        assert!(empty.text.contains("no output"), "{}", empty.text);
        std::fs::remove_dir_all(dir).unwrap();
    }

    async fn script_with(code: &str, dir: &Path, timeout: Duration) -> (Outcome, Vec<Call>) {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        (
            run(
                code,
                dir,
                Limits {
                    timeout,
                    ..Limits::default()
                },
                tx,
            )
            .await,
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn parallel_calls_overlap() {
        let dir = temp();
        let started = Instant::now();
        let (outcome, _) = script(
            "await Promise.all([sh('sleep 0.3'), sh('sleep 0.3'), sh('sleep 0.3')]); return 'ok'",
            &dir,
        )
        .await;
        assert_eq!(outcome.text, "ok");
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "{:?}",
            started.elapsed()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn clip_keeps_both_ends() {
        let text = format!("{}END", "x".repeat(50));
        let clipped = clip(&text, 10);
        assert!(clipped.starts_with("Total output lines: 1"));
        assert!(clipped.ends_with("xxEND"));
        assert!(clipped.contains("43 characters truncated"));
    }
}
