use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

use crate::builtins::{BuiltinResult, ShellState, run_builtin};
use crate::expand::expand_args;
use crate::parser::{
    CaseArm, CompoundCommand, Connector, IfChain, Job, Pipeline, Redirect, SimpleCommand, Unit,
};
use crate::tokenizer::RedirectKind;
use crate::{parser, tokenizer};

static SUBST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Runs a list of jobs (already separated by `; && ||`) in order, honoring
/// the exit code to decide whether the next one should run. Returns
/// `Some(code)` if there was an `exit`, or `None` if it ended normally.
pub fn run_jobs(jobs: &[Job], state: &mut ShellState) -> Option<i32> {
    let mut prev_connector = Connector::Seq;

    for job in jobs {
        let should_run = match prev_connector {
            Connector::And => state.last_status == 0,
            Connector::Or => state.last_status != 0,
            Connector::Seq => true,
        };

        if should_run {
            match run_unit(&job.unit, state) {
                Ok(status) => state.last_status = status,
                Err(RunError::Exit(code)) => return Some(code),
            }
        }

        prev_connector = job.connector.clone();
    }

    None
}

fn run_unit(unit: &Unit, state: &mut ShellState) -> Result<i32, RunError> {
    match unit {
        Unit::Pipeline(p) => run_pipeline(p, state),
        Unit::Compound(c) => run_compound(c, state),
        Unit::Group(jobs) => run_jobs_inline(jobs, state),
    }
}

/// Runs a list of jobs "inward" inside a compound command (the body of an
/// if/for/while/case), converting a `exit` into a `RunError::Exit` so that it
/// keeps propagating outward correctly.
fn run_jobs_inline(jobs: &[Job], state: &mut ShellState) -> Result<i32, RunError> {
    match run_jobs(jobs, state) {
        Some(code) => Err(RunError::Exit(code)),
        None => Ok(state.last_status),
    }
}

fn run_compound(cmd: &CompoundCommand, state: &mut ShellState) -> Result<i32, RunError> {
    match cmd {
        CompoundCommand::If(chain) => run_if(chain, state),
        CompoundCommand::For { var, words, body } => run_for(var, words, body, state),
        CompoundCommand::While { cond, body, until } => run_while(cond, body, *until, state),
        CompoundCommand::Case { word, arms } => run_case(word, arms, state),
        CompoundCommand::FunctionDef { name, body } => {
            // Definitions only register the body; they never run it here.
            state.functions.insert(name.clone(), body.clone());
            Ok(0)
        }
    }
}

fn run_if(chain: &IfChain, state: &mut ShellState) -> Result<i32, RunError> {
    for (cond, body) in &chain.branches {
        let status = run_jobs_inline(cond, state)?;
        if status == 0 {
            return run_jobs_inline(body, state);
        }
    }
    if let Some(else_body) = &chain.else_body {
        return run_jobs_inline(else_body, state);
    }
    Ok(0)
}

fn run_for(
    var: &str,
    words: &[String],
    body: &[Job],
    state: &mut ShellState,
) -> Result<i32, RunError> {
    let items = expand_current(words, state);
    let mut last = 0;
    for item in items {
        state.vars.insert(var.to_string(), item);
        last = run_jobs_inline(body, state)?;
    }
    Ok(last)
}

fn run_while(
    cond: &[Job],
    body: &[Job],
    until: bool,
    state: &mut ShellState,
) -> Result<i32, RunError> {
    let mut last = 0;
    loop {
        let status = run_jobs_inline(cond, state)?;
        let keep_going = if until { status != 0 } else { status == 0 };
        if !keep_going {
            break;
        }
        last = run_jobs_inline(body, state)?;
    }
    Ok(last)
}

fn run_case(word: &str, arms: &[CaseArm], state: &mut ShellState) -> Result<i32, RunError> {
    let expanded = expand_current(std::slice::from_ref(&word.to_string()), state);
    let subject = expanded.into_iter().next().unwrap_or_default();

    for arm in arms {
        let expanded_patterns = expand_current(&arm.patterns, state);
        if expanded_patterns
            .iter()
            .any(|p| pattern_matches(p, &subject))
        {
            return run_jobs_inline(&arm.body, state);
        }
    }
    Ok(0)
}

/// Matches a `case` pattern (supports `*`, `?`, `[...]` like a glob) against
/// a value. Reuses the `glob` crate that we already use for pathname expansion.
fn pattern_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true; // the most common case; avoids the cost of compiling the pattern
    }
    glob::Pattern::new(pattern)
        .map(|p| p.matches(value))
        .unwrap_or(false)
}

enum RunError {
    Exit(i32),
}

/// True if `s` is a valid shell variable name (`[a-zA-Z_][a-zA-Z0-9_]*`).
fn is_valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Splits a list of expanded arguments into leading `NAME=value` variable
/// assignments and the actual command words. POSIX: a simple command may
/// start with variable assignments. Only assignment-like words that come
/// before any command word are treated as assignments.
fn split_assignments(args: &[String]) -> (Vec<(String, String)>, Vec<String>) {
    let mut assigns = Vec::new();
    let mut command = Vec::new();

    for arg in args {
        if command.is_empty()
            && let Some((key, value)) = arg.split_once('=')
            && is_valid_name(key)
        {
            assigns.push((key.to_string(), value.to_string()));
            continue;
        }
        command.push(arg.clone());
    }

    (assigns, command)
}

/// Applies `NAME=value` assignments to the shell variables only. Following
/// POSIX, a plain assignment does not export the variable to the process
/// environment; only `export` does. Child processes receive these values
/// explicitly via `command.env(...)` when a command is spawned.
fn apply_assignments(assigns: &[(String, String)], state: &mut ShellState) {
    for (key, value) in assigns {
        state.vars.insert(key.clone(), value.clone());
    }
}

fn run_pipeline(pipeline: &Pipeline, state: &mut ShellState) -> Result<i32, RunError> {
    if pipeline.commands.len() == 1 {
        let cmd = resolve_alias(&pipeline.commands[0], state);
        let expanded = expand_current(&cmd.args, state);

        // POSIX: the command may start with `NAME=value` assignments. They
        // are applied to the shell (and to the child's environment) rather
        // than being executed as a program name.
        let (assigns, words) = split_assignments(&expanded);
        apply_assignments(&assigns, state);

        if words.is_empty() {
            // Assignment-only command: there is nothing to run.
            return Ok(0);
        }

        if crate::builtins::is_builtin(&words[0]) {
            // TP simplification: builtins only support stdout redirection
            // (>, >>), not stdin. Putting them in the middle of a real pipe
            // would require manual fork() — see README. Builtins run in the
            // foreground; the `&` is simply ignored for them.
            return run_builtin_with_redirects(&words, &cmd.redirects, state);
        }

        // A shell function takes precedence over an external binary.
        if let Some(body) = state.functions.get(&words[0]).cloned() {
            let args = words[1..].to_vec();
            let saved = std::mem::take(&mut state.positional);
            state.positional = args;
            let result = run_jobs_inline(&body, state);
            state.positional = saved;
            return result;
        }

        let command = words.join(" ");
        return run_external_single(
            &words,
            &cmd.redirects,
            &assigns,
            pipeline.background,
            &command,
            state,
        );
    }

    run_external_pipeline(pipeline, state)
}

/// Expands the args of a command using the current state, wiring the command
/// substitution (`$(...)` / backticks) to the `capture_subshell` below.
fn expand_current(args: &[String], state: &mut ShellState) -> Vec<String> {
    let vars_snapshot = state.vars.clone();
    let aliases_snapshot = state.aliases.clone();
    let functions_snapshot = state.functions.clone();
    let positional_snapshot = state.positional.clone();
    let status_snapshot = state.last_status;
    let prev_dir_snapshot = state.prev_dir.clone();

    let mut subst = |cmd: &str| -> String {
        capture_subshell(
            cmd,
            &vars_snapshot,
            &aliases_snapshot,
            &functions_snapshot,
            &positional_snapshot,
            status_snapshot,
            &prev_dir_snapshot,
        )
    };

    expand_args(
        args,
        &mut state.vars,
        &state.positional,
        state.last_status,
        &mut subst,
    )
}

/// Runs the contents of a `$(...)` in a "sub-state" that starts as a copy of
/// the current one (so `export X=1` inside does not pollute the parent — a
/// simple simulation of a subshell having its own environment). Captures its
/// stdout by redirecting the real file descriptor 1 to a temporary file (so
/// it works with external processes too, not only builtins).
fn capture_subshell(
    input: &str,
    vars: &HashMap<String, String>,
    aliases: &HashMap<String, String>,
    functions: &HashMap<String, Vec<Job>>,
    positional: &[String],
    last_status: i32,
    prev_dir: &Option<String>,
) -> String {
    let tokens = match tokenizer::tokenize(input) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("rsh: $(...): {}", e);
            return String::new();
        }
    };
    let jobs = match parser::parse(tokens) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("rsh: $(...): {}", e);
            return String::new();
        }
    };

    let mut child_state = ShellState {
        vars: vars.clone(),
        aliases: aliases.clone(),
        functions: functions.clone(),
        positional: positional.to_vec(),
        last_status,
        prev_dir: prev_dir.clone(),
        jobs: Vec::new(),
        next_job_id: 1,
    };

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;

        let tmp_path = std::env::temp_dir().join(format!(
            "rsh_subst_{}_{}.tmp",
            std::process::id(),
            SUBST_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));

        let captured = (|| -> std::io::Result<String> {
            let file = File::create(&tmp_path)?;
            let target_fd = file.as_raw_fd();

            let saved = unsafe { libc_dup(1) };
            unsafe { libc_dup2(target_fd, 1) };

            run_jobs(&jobs, &mut child_state);

            unsafe { libc_dup2(saved, 1) };
            unsafe { libc_close(saved) };
            drop(file);

            let mut out = String::new();
            File::open(&tmp_path)?.read_to_string(&mut out)?;
            Ok(out)
        })();

        let _ = std::fs::remove_file(&tmp_path);

        match captured {
            // POSIX: the trailing newlines of the substitution are trimmed.
            Ok(s) => s.trim_end_matches('\n').to_string(),
            Err(e) => {
                eprintln!("rsh: $(...): {}", e);
                String::new()
            }
        }
    }

    #[cfg(not(unix))]
    {
        String::new()
    }
}

fn resolve_alias(cmd: &SimpleCommand, state: &ShellState) -> SimpleCommand {
    if let Some(first) = cmd.args.first()
        && let Some(expansion) = state.aliases.get(first)
    {
        let mut new_args: Vec<String> = expansion
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        new_args.extend(cmd.args[1..].iter().cloned());
        return SimpleCommand {
            args: new_args,
            redirects: cmd.redirects.clone(),
        };
    }
    cmd.clone()
}

fn run_builtin_with_redirects(
    args: &[String],
    redirects: &[Redirect],
    state: &mut ShellState,
) -> Result<i32, RunError> {
    let out_redirect = redirects
        .iter()
        .find(|r| matches!(r.kind, RedirectKind::Out | RedirectKind::Append));

    if let Some(r) = out_redirect {
        let file = match open_for_redirect(r) {
            Some(f) => f,
            None => return Ok(1),
        };
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let stdout_fd = std::io::stdout().as_raw_fd();
            let saved = unsafe { libc_dup(stdout_fd) };
            unsafe { libc_dup2(file.as_raw_fd(), stdout_fd) };

            let result = match run_builtin(args, state) {
                Some(BuiltinResult::Exit(code)) => {
                    unsafe { libc_dup2(saved, stdout_fd) };
                    unsafe { libc_close(saved) };
                    return Err(RunError::Exit(code));
                }
                Some(BuiltinResult::Status(code)) => code,
                None => 1,
            };

            unsafe { libc_dup2(saved, stdout_fd) };
            unsafe { libc_close(saved) };
            return Ok(result);
        }
        #[cfg(not(unix))]
        {
            let _ = file;
        }
    }

    match run_builtin(args, state) {
        Some(BuiltinResult::Exit(code)) => Err(RunError::Exit(code)),
        Some(BuiltinResult::Status(code)) => Ok(code),
        None => Ok(1),
    }
}

fn open_for_redirect(r: &Redirect) -> Option<File> {
    let result = match r.kind {
        RedirectKind::Out => File::create(&r.target),
        RedirectKind::Append => OpenOptions::new().create(true).append(true).open(&r.target),
        RedirectKind::In => File::open(&r.target),
        RedirectKind::ErrOut => File::create(&r.target),
    };
    result
        .map_err(|e| eprintln!("rsh: {}: {}", r.target, e))
        .ok()
}

fn run_external_single(
    args: &[String],
    redirects: &[Redirect],
    envs: &[(String, String)],
    background: bool,
    command: &str,
    state: &mut ShellState,
) -> Result<i32, RunError> {
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..]);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    apply_redirects(&mut cmd, redirects, Stdio::inherit(), Stdio::inherit());

    // Give every spawned child its own process group and the right signal
    // dispositions (all async-signal-safe, so it is safe in pre_exec).
    #[cfg(unix)]
    {
        if background {
            cmd.stdin(Stdio::null());
            unsafe {
                cmd.pre_exec(|| {
                    crate::jobctl::background_child_setup();
                    Ok(())
                });
            }
        } else {
            unsafe {
                cmd.pre_exec(|| {
                    crate::jobctl::foreground_child_setup();
                    Ok(())
                });
            }
        }
    }

    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id() as i32;

            if background {
                crate::jobctl::set_group(pid);
                let id = state.add_job(command.to_string(), pid);
                println!("[{}] {}", id, pid);
                return Ok(0);
            }

            // Foreground: give the child the terminal and block until it
            // finishes, or is stopped with ^Z (in which case it becomes a job).
            crate::jobctl::set_group(pid);
            crate::jobctl::give_terminal(pid);
            match crate::jobctl::wait_foreground(pid) {
                crate::jobctl::FgOutcome::Exited(code) => {
                    crate::jobctl::release_terminal();
                    Ok(code)
                }
                crate::jobctl::FgOutcome::Stopped => {
                    crate::jobctl::release_terminal();
                    state.add_stopped_job(command.to_string(), pid);
                    Ok(0)
                }
            }
        }
        Err(e) => {
            eprintln!("rsh: {}: {}", args[0], e);
            Ok(127)
        }
    }
}

fn apply_redirects(
    cmd: &mut Command,
    redirects: &[Redirect],
    default_in: Stdio,
    default_out: Stdio,
) {
    let mut stdin_set = false;
    let mut stdout_set = false;

    for r in redirects {
        match r.kind {
            RedirectKind::In => {
                if let Ok(f) = File::open(&r.target) {
                    cmd.stdin(Stdio::from(f));
                    stdin_set = true;
                } else {
                    eprintln!("rsh: {}: no such file", r.target);
                }
            }
            RedirectKind::Out => {
                if let Ok(f) = File::create(&r.target) {
                    cmd.stdout(Stdio::from(f));
                    stdout_set = true;
                }
            }
            RedirectKind::Append => {
                if let Ok(f) = OpenOptions::new().create(true).append(true).open(&r.target) {
                    cmd.stdout(Stdio::from(f));
                    stdout_set = true;
                }
            }
            RedirectKind::ErrOut => {
                if let Ok(f) = File::create(&r.target) {
                    cmd.stderr(Stdio::from(f));
                }
            }
        }
    }

    if !stdin_set {
        cmd.stdin(default_in);
    }
    if !stdout_set {
        cmd.stdout(default_out);
    }
}

fn run_external_pipeline(pipeline: &Pipeline, state: &mut ShellState) -> Result<i32, RunError> {
    let n = pipeline.commands.len();
    let background = pipeline.background;

    // A human-readable label for the job table (only needed for background).
    let label = pipeline
        .commands
        .iter()
        .flat_map(|c| c.args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" | ");

    // Pids of every forked/spawned stage, waited in order at the end.
    let mut child_pids: Vec<i32> = Vec::with_capacity(n);
    // Read end of the previous stage's pipe, feeding this stage's stdin.
    let mut prev_in: Option<OwnedFd> = None;

    for (idx, cmd) in pipeline.commands.iter().enumerate() {
        let resolved = resolve_alias(cmd, state);
        let expanded = expand_current(&resolved.args, state);

        // Handle leading `NAME=value` assignments within a pipeline stage.
        let (assigns, words) = split_assignments(&expanded);
        apply_assignments(&assigns, state);
        if words.is_empty() {
            continue;
        }

        let is_last = idx == n - 1;

        // A builtin or function stage runs in a forked subshell that writes to
        // the pipe, so it can sit anywhere in a pipeline (`echo x | wc`, etc.).
        if crate::builtins::is_builtin(&words[0]) || state.functions.contains_key(&words[0]) {
            let (pid, next_in) =
                match fork_stage(&words, &resolved.redirects, state, prev_in.take(), is_last) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("rsh: {}: {}", words[0], e);
                        return Ok(127);
                    }
                };
            prev_in = next_in;
            child_pids.push(pid);
            continue;
        }

        // External executable, spawned via exec (fds handled by std::Command).
        let mut command = Command::new(&words[0]);
        command.args(&words[1..]);
        for (key, value) in &assigns {
            command.env(key, value);
        }

        let stdin = match prev_in.take() {
            Some(fd) => Stdio::from(fd),
            None => Stdio::inherit(),
        };
        let stdout = if is_last {
            Stdio::inherit()
        } else {
            Stdio::piped()
        };

        apply_redirects(&mut command, &resolved.redirects, stdin, stdout);

        if background {
            command.stdin(Stdio::null());
            #[cfg(unix)]
            unsafe {
                command.pre_exec(|| {
                    crate::jobctl::background_child_setup();
                    Ok(())
                });
            }
        }

        match command.spawn() {
            Ok(mut child) => {
                if !is_last && let Some(out) = child.stdout.take() {
                    prev_in = Some(unsafe { OwnedFd::from_raw_fd(out.into_raw_fd()) });
                }
                child_pids.push(child.id() as i32);
            }
            Err(e) => {
                eprintln!("rsh: {}: {}", words[0], e);
                return Ok(127);
            }
        }
    }

    if background {
        if let Some(&pid) = child_pids.last() {
            crate::jobctl::set_group(pid);
            let id = state.add_job(label, pid);
            println!("[{}] {}", id, pid);
        }
        return Ok(0);
    }

    let mut last_status = 0;
    for pid in child_pids {
        last_status = crate::jobctl::wait_blocking(pid).unwrap_or(1);
    }
    Ok(last_status)
}

/// Forks a subshell that runs a builtin or function stage of a pipeline with
/// the correct pipe fds. Returns the child pid and the read end of this
/// stage's stdout pipe (if not the last stage).
#[cfg(unix)]
fn fork_stage(
    words: &[String],
    redirects: &[Redirect],
    state: &mut ShellState,
    stdin_read: Option<OwnedFd>,
    is_last: bool,
) -> std::io::Result<(i32, Option<OwnedFd>)> {
    let (next_read, out_write): (Option<OwnedFd>, Option<OwnedFd>) = if is_last {
        (None, None)
    } else {
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        (
            Some(unsafe { OwnedFd::from_raw_fd(fds[0]) }),
            Some(unsafe { OwnedFd::from_raw_fd(fds[1]) }),
        )
    };

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(std::io::Error::last_os_error());
    }

    if pid == 0 {
        // Child: a subshell running the builtin/function with the pipe fds.
        stage_child_setup(&stdin_read, &out_write, redirects);
        let code = run_stage_in_child(words, state);
        unsafe {
            libc::_exit(code);
        }
    }

    // Parent: the child owns (a copy of) these ends; drop ours.
    drop(out_write);
    drop(stdin_read);
    Ok((pid, next_read))
}

/// Child-side fd setup for a forked pipeline stage: applies redirects, then
/// wires the incoming pipe read end and outgoing write end, closing the latter
/// so a downstream reader sees EOF when the stage exits.
#[cfg(unix)]
fn stage_child_setup(
    stdin_read: &Option<OwnedFd>,
    stdout_write: &Option<OwnedFd>,
    redirects: &[Redirect],
) {
    let mut in_set = false;
    let mut out_set = false;
    for r in redirects {
        match r.kind {
            RedirectKind::In => {
                if let Ok(f) = std::fs::File::open(&r.target) {
                    let fd = f.as_raw_fd();
                    unsafe {
                        libc::dup2(fd, libc::STDIN_FILENO);
                    }
                    in_set = true;
                }
            }
            RedirectKind::Out | RedirectKind::Append => {
                let f = if r.kind == RedirectKind::Out {
                    std::fs::File::create(&r.target)
                } else {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&r.target)
                };
                if let Ok(f) = f {
                    let fd = f.as_raw_fd();
                    unsafe {
                        libc::dup2(fd, libc::STDOUT_FILENO);
                    }
                    out_set = true;
                }
            }
            RedirectKind::ErrOut => {
                if let Ok(f) = std::fs::File::create(&r.target) {
                    let fd = f.as_raw_fd();
                    unsafe {
                        libc::dup2(fd, libc::STDERR_FILENO);
                    }
                }
            }
        }
    }
    unsafe {
        if !in_set && let Some(fd) = stdin_read {
            libc::dup2(fd.as_raw_fd(), libc::STDIN_FILENO);
        }
        if !out_set && let Some(fd) = stdout_write {
            libc::dup2(fd.as_raw_fd(), libc::STDOUT_FILENO);
        }
        // Close inherited pipe ends so other stages observe EOF promptly.
        if let Some(fd) = stdin_read {
            libc::close(fd.as_raw_fd());
        }
        if let Some(fd) = stdout_write {
            libc::close(fd.as_raw_fd());
        }
    }
}

/// Runs a builtin or shell function inside a forked pipeline stage, returning
/// its exit code.
#[cfg(unix)]
fn run_stage_in_child(words: &[String], state: &mut ShellState) -> i32 {
    use std::io::Write;
    let code = if crate::builtins::is_builtin(&words[0]) {
        match run_builtin(words, state) {
            Some(BuiltinResult::Status(c)) | Some(BuiltinResult::Exit(c)) => c,
            None => 127,
        }
    } else if let Some(body) = state.functions.get(&words[0]).cloned() {
        let args = words[1..].to_vec();
        let saved = std::mem::take(&mut state.positional);
        state.positional = args;
        let r = run_jobs_inline(&body, state);
        state.positional = saved;
        r.unwrap_or(0)
    } else {
        127
    };
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    code
}
// --- small libc wrappers for dup/dup2/close, used to redirect builtin stdout
// and to capture $(...) without depending on an extra crate. ---
#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "dup"]
    fn c_dup(fd: i32) -> i32;
    #[link_name = "dup2"]
    fn c_dup2(oldfd: i32, newfd: i32) -> i32;
    #[link_name = "close"]
    fn c_close(fd: i32) -> i32;
}
#[cfg(unix)]
unsafe fn libc_dup(fd: i32) -> i32 {
    unsafe { c_dup(fd) }
}
#[cfg(unix)]
unsafe fn libc_dup2(oldfd: i32, newfd: i32) -> i32 {
    unsafe { c_dup2(oldfd, newfd) }
}
#[cfg(unix)]
unsafe fn libc_close(fd: i32) -> i32 {
    unsafe { c_close(fd) }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Connector;

    fn simple(unit: Unit) -> Job {
        Job {
            unit,
            connector: Connector::Seq,
        }
    }

    fn pipeline_command(args: &[&str]) -> Pipeline {
        Pipeline {
            commands: vec![SimpleCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
                redirects: vec![],
            }],
            background: false,
        }
    }

    #[test]
    fn test_split_assignments() {
        let args = vec![
            "FOO=1".to_string(),
            "BAR=hello".to_string(),
            "echo".to_string(),
            "FOO=not_an_assignment_anymore".to_string(),
        ];
        let (assigns, words) = split_assignments(&args);
        assert_eq!(assigns.len(), 2);
        assert_eq!(assigns[0], ("FOO".to_string(), "1".to_string()));
        assert_eq!(assigns[1], ("BAR".to_string(), "hello".to_string()));
        assert_eq!(
            words,
            vec![
                "echo".to_string(),
                "FOO=not_an_assignment_anymore".to_string()
            ]
        );
    }

    #[test]
    fn test_split_assignments_invalid_name_not_split() {
        // `1FOO=...` is not a valid name, so it stays a command word.
        let args = vec!["1FOO=x".to_string()];
        let (assigns, words) = split_assignments(&args);
        assert!(assigns.is_empty());
        assert_eq!(words, vec!["1FOO=x".to_string()]);
    }

    #[test]
    fn test_assignment_only_command_sets_var() {
        let jobs = vec![simple(Unit::Pipeline(pipeline_command(&["FOO=hello"])))];
        let mut state = ShellState::new();
        assert_eq!(run_jobs(&jobs, &mut state), None);
        assert_eq!(state.vars.get("FOO"), Some(&"hello".to_string()));
    }

    #[test]
    fn test_assignment_without_command_subst() {
        // Two bare assignments in a row must both be set.
        // (Uses literal values: command substitution redirects the real
        // process fd 1, which becomes racy under the parallel test runner.)
        let jobs = vec![simple(Unit::Pipeline(pipeline_command(&[
            "G=A", "H=world",
        ])))];
        let mut state = ShellState::new();
        assert_eq!(run_jobs(&jobs, &mut state), None);
        assert_eq!(state.vars.get("G"), Some(&"A".to_string()));
        assert_eq!(state.vars.get("H"), Some(&"world".to_string()));
    }
}
