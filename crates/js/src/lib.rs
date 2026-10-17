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
read(path, {offset?, limit?}) -> string      lines offset..offset+limit (1-based); long files come in 2000-line pages
write(path, text)                            create or overwrite, makes parent dirs
edit(path, old, new, {all?})                 exact text replace; throws if old is missing or ambiguous
ls(path='.') -> string[]                     sorted names, directories end with /
sh(cmd, {cwd?, timeout?}) -> {code, out, err}  bash -c; timeout ms (default 120000); never throws on exit code
console.log(...)                             add a line to the output
Every call starts immediately and runs concurrently: await only where you need the value.
Calls you never await still finish before the program ends; their failures are reported.
Each tool also takes one object, e.g. edit({path, old, new}) or sh({cmd, timeout}).";

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
    /// Finished calls, for the "what already happened" footer after an error.
    done: std::sync::Mutex<Vec<Call>>,
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
        done: std::sync::Mutex::new(Vec::new()),
    });
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let secs = limits.timeout.as_secs();
    let result =
        tokio::time::timeout(limits.timeout, execute(code, env.clone(), calls.clone(), &limits)).await;
    let (out, error) = match result {
        Ok(Ok((out, value))) => (join(&out, value.as_deref()), None),
        Ok(Err((out, err))) => {
            let err = if err.contains("interrupted") {
                format!("stopped after {secs}s: the program never finished (endless loop?)")
            } else {
                err
            };
            (join(&out, Some(&format!("error{err}"))), Some(err))
        }
        Err(_) => {
            let err = format!(": the program ran longer than {secs}s and was stopped");
            (format!("error{err}"), Some(err))
        }
    };
    // After a failure, list what already ran: those effects are real.
    let out = match &error {
        Some(_) => {
            let done = env.done.lock().map(|done| done.clone()).unwrap_or_default();
            if done.is_empty() {
                format!("{out}\nnothing ran before the error")
            } else {
                let mut footer = format!("{out}\ncalls that ran before the error (their effects are kept):");
                for call in done.iter().take(12) {
                    let mark = if call.ok { "✓" } else { "✗" };
                    footer.push_str(&format!("\n  {mark} {} {} · {}", call.verb, call.target, call.info));
                }
                if done.len() > 12 {
                    footer.push_str(&format!("\n  … {} more", done.len() - 12));
                }
                footer
            }
        }
        None => out,
    };
    let error = error.map(|err| err.trim_start_matches([':', ' ']).to_string());
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
const __pending = [];
const __fmt = (x) => {
  if (typeof x === 'string') return x;
  if (x && typeof x.then === 'function') return '<' + (x.__label || 'call') + ' not awaited>';
  try { return JSON.stringify(x); } catch (e) { return String(x); }
};
globalThis.console = { log: (...a) => { __out.push(a.map(__fmt).join(' ')); } };
console.error = console.warn = console.info = console.debug = console.log;
// One object argument works too: read({path}), edit({path, old, new}), sh({cmd}).
const __norm = (name, a) => {
  if (a.length === 1 && a[0] && typeof a[0] === 'object' && !Array.isArray(a[0])) {
    const o = a[0];
    const pick = (...ks) => { for (const k of ks) if (o[k] !== undefined) return o[k]; };
    if (name === 'read') a = [pick('path', 'file', 'file_path'), { offset: pick('offset', 'start', 'from'), limit: pick('limit', 'count', 'lines') }];
    if (name === 'write') a = [pick('path', 'file', 'file_path'), pick('content', 'text', 'data', 'contents')];
    if (name === 'edit') a = [pick('path', 'file', 'file_path'), pick('old', 'old_text', 'old_string', 'search', 'find'), pick('new', 'new_text', 'new_string', 'replace', 'replacement'), { all: pick('all', 'replace_all') }];
    if (name === 'ls') a = [pick('path', 'dir', 'directory')];
    if (name === 'sh') a = [pick('cmd', 'command', 'script'), { cwd: pick('cwd', 'dir', 'workdir'), timeout: pick('timeout', 'timeout_ms') }];
  }
  if (name === 'sh' && Array.isArray(a[0])) {
    a = [a[0].map(x => /^[\w@%+=:,./-]+$/.test(String(x)) ? String(x) : "'" + String(x).replace(/'/g, "'\\''") + "'").join(' '), ...a.slice(1)];
  }
  if (name === 'read' && typeof a[1] === 'number') a = [a[0], { offset: a[1], limit: a[2] }];
  if (name === 'write' && typeof a[1] !== 'string') a = [a[0], a[1] === undefined ? '' : JSON.stringify(a[1], null, 2)];
  return a;
};
// Calls start at once. The returned thenable records whether anyone awaited
// it, so failures nobody looked at are still reported.
const __tool = (name) => (...args) => {
  const norm = __norm(name, args);
  const label = name + '(' + JSON.stringify(norm[0] === undefined ? '' : norm[0]).slice(0, 60) + ')';
  const entry = { label, used: false, error: null };
  entry.inner = __call(name, JSON.stringify(norm)).then((s) => {
    const r = JSON.parse(s);
    if ('error' in r) throw new Error(label + ': ' + r.error);
    return r.value;
  });
  entry.inner.catch((e) => { entry.error = e; });
  __pending.push(entry);
  return {
    __label: label,
    then(ok, ko) { entry.used = true; return entry.inner.then(ok, ko); },
    catch(ko) { entry.used = true; return entry.inner.catch(ko); },
    finally(f) { entry.used = true; return entry.inner.finally(f); },
  };
};
for (const name of ['read', 'write', 'edit', 'ls', 'sh']) globalThis[name] = __tool(name);
// A returned value may hold calls that were never awaited: resolve them.
const __settle = async (v, d = 0) => {
  if (v && typeof v.then === 'function') v = await v;
  if (d > 2 || !v || typeof v !== 'object') return v;
  if (Array.isArray(v)) return Promise.all(v.map((x) => __settle(x, d + 1)));
  if (Object.getPrototypeOf(v) === Object.prototype) {
    const o = {};
    for (const k of Object.keys(v)) o[k] = await __settle(v[k], d + 1);
    return o;
  }
  return v;
};
"#;

/// Wraps the model's code; line numbers in errors are shifted back by the
/// lines before the code.
const WRAP_HEAD: &str = "
(async () => {
  let v, err;
  try { v = await __settle(await (async () => {
";
const WRAP_TAIL: &str = r#"
})()); } catch (e) { err = e; }
  await Promise.allSettled(__pending.map((p) => p.inner));
  const lost = __pending.filter((p) => !p.used && p.error).map((p) => 'not awaited and failed: ' + p.error.message);
  const out = __out.concat(lost).join('\n');
  if (err !== undefined) {
    const msg = err && err.message !== undefined ? err.message : String(err);
    return JSON.stringify({ out, error: msg, name: String((err && err.name) || ''), stack: String((err && err.stack) || '') });
  }
  return JSON.stringify({ out, value: v === undefined ? null : __fmt(v) });
})()
"#;

/// "eval_script:LINE" in an error, as `: <name> at line N\n  N | code`.
fn locate(error: &str, name: &str, stack: &str, code: &str) -> String {
    let offset = PRELUDE.matches('\n').count() + WRAP_HEAD.matches('\n').count();
    let mut lines = Vec::new();
    for text in [stack, error] {
        for (at, marker) in text.match_indices("eval_script:") {
            let digits: String = text[at + marker.len()..].chars().take_while(char::is_ascii_digit).collect();
            if let Ok(line) = digits.parse::<usize>() {
                lines.push(line);
            }
        }
    }
    let count = code.lines().count();
    let line = lines
        .into_iter()
        .find_map(|line| line.checked_sub(offset).filter(|line| *line >= 1 && *line <= count));
    let message = error.lines().next().unwrap_or(error).trim();
    let kind = if name.is_empty() || message.starts_with(name) { String::new() } else { format!("{name}: ") };
    match line {
        Some(line) => {
            let text = code.lines().nth(line - 1).unwrap_or_default().trim();
            format!(" at line {line}: {kind}{message}\n  {line} | {text}")
        }
        None => format!(": {kind}{message}"),
    }
}

type Lines = (String, Option<String>);

async fn execute(
    code: &str,
    env: Arc<Env>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    limits: &Limits,
) -> Result<Lines, (String, String)> {
    let fail = |err: String| (String::new(), format!(": {err}"));
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
    let script = format!("{PRELUDE}{WRAP_HEAD}{code}{WRAP_TAIL}");
    let source = code.to_string();
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
    // Syntax errors never start the program: point at the line.
    let raw = raw.map_err(|err| fail(locate(&err, "", "", &source)))?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|err| fail(format!(": {err}")))?;
    let out = parsed["out"].as_str().unwrap_or_default().to_string();
    match parsed.get("error").and_then(Value::as_str) {
        Some(err) => {
            let name = parsed["name"].as_str().unwrap_or_default();
            let stack = parsed["stack"].as_str().unwrap_or_default();
            Err((out, locate(err, name, stack, &source)))
        }
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
    let call = Call {
        verb: name.to_string(),
        target,
        info: one_line(&info),
        ok,
    };
    if let Ok(mut done) = env.done.lock() {
        done.push(call.clone());
    }
    let _ = env.log.send(call);
    answer
}

type Done = Result<(Value, String), String>;

fn path(env: &Env, path: &str) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("missing path".into());
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Ok(PathBuf::from(home).join(rest));
    }
    Ok(env.cwd.join(path))
}

/// "no such file" plus up to five look-alike names from the same directory.
fn missing(target: &Path, file: &str) -> String {
    let Some(dir) = target.parent() else {
        return format!("no such file: {file}");
    };
    if !dir.is_dir() {
        return format!("no such directory: {}", dir.display());
    }
    let wanted = target.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_lowercase();
    let mut similar: Vec<(usize, String)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .map(|name| (distance(&wanted, &name.to_lowercase()), name))
        .filter(|(score, name)| *score <= (name.len() / 3).max(2))
        .collect();
    similar.sort();
    let similar: Vec<String> = similar.into_iter().take(5).map(|(_, name)| name).collect();
    if similar.is_empty() {
        format!("no such file: {file}")
    } else {
        format!("no such file: {file} (similar here: {})", similar.join(", "))
    }
}

/// Lines per read when the caller gave no limit.
const PAGE: usize = 2000;

fn read(env: &Env, file: &str, opts: Option<&Value>) -> Done {
    let target = path(env, file)?;
    let meta = std::fs::metadata(&target).map_err(|_| missing(&target, file))?;
    if meta.is_dir() {
        return Err(format!("{file} is a directory; use ls('{file}')"));
    }
    let bytes = std::fs::read(&target).map_err(|err| err.to_string())?;
    if bytes.iter().take(8000).any(|byte| *byte == 0) {
        return Err(format!(
            "{file} is binary ({} bytes); inspect it with sh, e.g. sh('file {file}')",
            bytes.len()
        ));
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let total = text.lines().count();
    let offset = opts.and_then(|o| o["offset"].as_u64()).unwrap_or(1).max(1) as usize;
    let limit = opts.and_then(|o| o["limit"].as_u64()).map(|n| n as usize);
    if offset == 1 && limit.is_none() && total <= PAGE {
        return Ok((json!(text), format!("{total} lines")));
    }
    if offset > total.max(1) {
        return Err(format!("{file} has only {total} lines; offset {offset} is past the end"));
    }
    // The model asked for a range: give exactly that. No range on a long
    // file: first page plus a note on how to get the rest.
    let paged = limit.is_none();
    let limit = Some(limit.unwrap_or(PAGE));
    let lines: Vec<&str> = text
        .lines()
        .skip(offset - 1)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    let end = offset - 1 + lines.len();
    let mut page = lines.join("\n");
    if paged && end < total {
        page.push_str(&format!(
            "\n…[lines {offset}-{end} of {total}; read('{file}', {{offset: {}}}) for more]",
            end + 1
        ));
    }
    Ok((json!(page), format!("L{offset}-{end} of {total}")))
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
    let text = std::fs::read_to_string(&target).map_err(|_| missing(&target, file))?;
    if old.is_empty() {
        return Err("old text is empty; to create or replace a whole file use write".into());
    }
    if old == new {
        return Err("old and new text are identical; nothing to change".into());
    }
    let count = text.matches(old).count();
    let all = opts.and_then(|o| o["all"].as_bool()).unwrap_or(false);
    match count {
        0 => return Err(not_found(&text, old, file)),
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
    let timeout =
        Duration::from_millis(opts.and_then(|o| o["timeout"].as_u64()).unwrap_or(120_000));
    let start = Instant::now();
    let mut child = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Own process group: a timeout or a stopped run kills everything the
        // command started, not just bash.
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| err.to_string())?;
    let group = Group(child.id());
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err("no pipes".into());
    };
    let (out_buf, err_buf) = (Buffer::default(), Buffer::default());
    let out_task = tokio::spawn(drain(stdout, out_buf.clone()));
    let err_task = tokio::spawn(drain(stderr, err_buf.clone()));
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => status.map_err(|err| err.to_string())?,
        Err(_) => {
            return Err(format!(
                "timed out after {}s and was killed; pass {{timeout: ms}} for longer jobs, or run it in the background: sh('cmd > /tmp/log 2>&1 &')",
                timeout.as_secs()
            ));
        }
    };
    // `cmd &` leaves children holding the pipes open, so EOF may never come.
    // Once bash itself has exited, give the pipes a moment, then take what
    // arrived and stop reading.
    let grace = Duration::from_millis(200);
    let _ = tokio::time::timeout(grace, async {
        let _ = out_task.await;
        let _ = err_task.await;
    })
    .await;
    let (out, err) = (out_buf.take(), err_buf.take());
    std::mem::forget(group);
    let secs = start.elapsed().as_secs_f64();
    let code = status.code().unwrap_or(-1);
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

/// Bytes read so far from a pipe, readable while the reader still runs.
#[derive(Clone, Default)]
struct Buffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl Buffer {
    fn take(&self) -> String {
        let bytes = self
            .0
            .lock()
            .map(|mut bytes| std::mem::take(&mut *bytes))
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// Copy a pipe into `buffer` chunk by chunk until EOF.
async fn drain(mut pipe: impl tokio::io::AsyncRead + Unpin, buffer: Buffer) {
    use tokio::io::AsyncReadExt;
    let mut chunk = [0u8; 8192];
    while let Ok(read) = pipe.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        if let Ok(mut bytes) = buffer.0.lock() {
            bytes.extend_from_slice(&chunk[..read]);
        }
    }
}

/// Kills a command's whole process group when dropped: on timeout, on a
/// stopped run (the future is dropped), on error. Forgotten on normal exit so
/// intentional background jobs (`server &`) keep running.
struct Group(Option<u32>);

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // SAFETY-free: plain `kill -- -PGID`, no libc needed.
            let _ = std::process::Command::new("kill")
                .args(["-KILL", "--", &format!("-{pid}")])
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

/// Edit distance, for "did you mean" (names are short; O(n·m) is fine).
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (row[j + 1] + 1).min(row[j] + 1).min(prev + usize::from(ca != *cb));
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// Why `old` didn't match, and where to look.
fn not_found(text: &str, old: &str, file: &str) -> String {
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if squash(text).contains(&squash(old)) {
        return format!(
            "old text not found in {file} as written, but it matches when whitespace is ignored: copy the exact indentation from read('{file}')"
        );
    }
    let first = old.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
    match text.lines().position(|line| !first.is_empty() && line.contains(first)) {
        Some(at) => format!(
            "old text not found in {file}; its first line is at line {}, but what follows differs. read('{file}', {{offset: {}, limit: 30}}) and copy exactly",
            at + 1,
            at.saturating_sub(2) + 1
        ),
        None => format!("old text not found in {file}; read the file first, it may have changed"),
    }
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

    #[tokio::test]
    async fn background_jobs_dont_hang_and_timeouts_kill_the_group() {
        let dir = temp();
        let started = Instant::now();
        let (outcome, _) = script("return (await sh('sleep 30 & echo started')).out", &dir).await;
        assert_eq!(outcome.text, "started", "{}", outcome.text);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        let _ = std::process::Command::new("pkill")
            .args(["-f", "sleep 30"])
            .status();

        let marker = format!("smash-orphan-{}", std::process::id());
        let (timed, _) = script(
            &format!(
                "await sh('bash -c \"sleep 40; echo {marker}\" & sleep 40', {{timeout: 300}})"
            ),
            &dir,
        )
        .await;
        assert!(timed.text.contains("timed out"), "{}", timed.text);
        std::thread::sleep(Duration::from_millis(200));
        let left = std::process::Command::new("pgrep")
            .args(["-f", &marker])
            .output()
            .unwrap();
        assert!(
            left.stdout.is_empty(),
            "children of a timed-out command are killed"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn calls_are_async_by_default() {
        let dir = temp();
        // Never awaited: still runs, still finishes before the program ends.
        let (outcome, calls) = script("write('a.txt', 'x'); write('b.txt', 'y'); return 'started'", &dir).await;
        assert_eq!(outcome.text, "started");
        assert_eq!(calls.len(), 2, "unawaited calls finished");
        assert!(dir.join("b.txt").exists());
        // Returning calls without await resolves them, inside arrays/objects too.
        let (outcome, _) = script("return { a: read('a.txt'), both: [read('a.txt'), read('b.txt')] }", &dir).await;
        assert_eq!(outcome.text, r#"{"a":"x","both":["x","y"]}"#);
        // A failing call nobody awaited is reported, not lost.
        let (outcome, _) = script("read('nope.txt'); return 'done'", &dir).await;
        assert!(outcome.text.contains("not awaited and failed: read(\"nope.txt\")"), "{}", outcome.text);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn errors_point_at_the_line_and_list_what_ran() {
        let dir = temp();
        let (syntax, _) = script("const a = 1;\nconst b = (;\nreturn a", &dir).await;
        assert!(syntax.text.contains("at line 2") && syntax.text.contains("2 | const b = (;"), "{}", syntax.text);
        let (thrown, _) = script("await write('w.txt', 'hi');\nconst x = null;\nreturn x.length", &dir).await;
        assert!(thrown.text.contains("at line 3: TypeError"), "{}", thrown.text);
        assert!(thrown.text.contains("✓ write w.txt"), "footer lists finished calls: {}", thrown.text);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn tool_misuse_gets_useful_answers() {
        let dir = temp();
        std::fs::write(dir.join("config.toml"), "a = 1\n  b = 2\n").unwrap();
        std::fs::write(dir.join("blob"), [0u8, 1, 2, 3]).unwrap();
        let long: String = (1..=2500).map(|n| format!("{n}\n")).collect();
        std::fs::write(dir.join("long.txt"), long).unwrap();
        let err = |code: &'static str| {
            let dir = dir.clone();
            async move { script(code, &dir).await.0.text }
        };
        assert!(err("return await read('confg.toml')").await.contains("similar here: config.toml"));
        assert!(err("return await read('.')").await.contains("is a directory; use ls"));
        assert!(err("return await read('blob')").await.contains("is binary"));
        assert!(err("await edit('config.toml', 'a = 1\\nb = 2', 'x')").await.contains("whitespace is ignored"));
        assert!(err("await edit('config.toml', 'a = 1\\nzzz', 'x')").await.contains("first line is at line 1"));
        assert!(err("await edit('config.toml', 'a', 'a')").await.contains("identical"));
        let page = err("return await read('long.txt')").await;
        assert!(page.contains("lines 1-2000 of 2500") && page.contains("offset: 2001"), "{}", &page[page.len() - 120..]);
        // Object-style arguments and argv arrays.
        assert_eq!(err("return await read({path: 'config.toml', offset: 2, limit: 1})").await, "  b = 2");
        assert_eq!(err("return (await sh(['echo', 'a b'])).out").await, "a b");
        assert!(err("await sh('sleep 5', {timeout: 200})").await.contains("background"));
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
