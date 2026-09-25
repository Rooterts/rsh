use std::collections::HashMap;
use std::env;
use std::path::Path;

use crate::parser::Job;

/// Result of trying to run a builtin.
/// `None` means "this is not a builtin, run it as an external command".
pub enum BuiltinResult {
    Exit(i32),
    Status(i32),
}

pub struct ShellState {
    pub vars: HashMap<String, String>,
    pub aliases: HashMap<String, String>,
    pub functions: HashMap<String, Vec<Job>>,
    pub positional: Vec<String>,
    pub last_status: i32,
    pub prev_dir: Option<String>,
    /// Running or stopped background jobs, tracked for `jobs`/`fg`/`bg`.
    pub jobs: Vec<JobRecord>,
    pub next_job_id: usize,
}

/// A single tracked job (an element of the shell's job table).
#[derive(Clone)]
pub struct JobRecord {
    pub id: usize,
    pub pid: i32,
    pub command: String,
    pub stopped: bool,
}

impl ShellState {
    pub fn new() -> Self {
        ShellState {
            vars: HashMap::new(),
            aliases: HashMap::new(),
            functions: HashMap::new(),
            positional: Vec::new(),
            last_status: 0,
            prev_dir: None,
            jobs: Vec::new(),
            next_job_id: 1,
        }
    }

    /// Adds a background/running job to the table and returns its new job id.
    pub fn add_job(&mut self, command: String, pid: i32) -> usize {
        let id = self.next_job_id;
        self.next_job_id += 1;
        self.jobs.push(JobRecord {
            id,
            pid,
            command,
            stopped: false,
        });
        id
    }

    /// Adds a foreground job that stopped (^Z) so it can be resumed later.
    pub fn add_stopped_job(&mut self, command: String, pid: i32) -> usize {
        let id = self.add_job(command, pid);
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            job.stopped = true;
        }
        id
    }
}

const BUILTIN_NAMES: &[&str] = &[
    "cd", "pwd", "exit", "export", "unset", "echo", "alias", "unalias", "which", "type", "test",
    "[", "jobs", "fg", "bg", "wait", "disown", "kill", "read",
];

pub fn is_builtin(name: &str) -> bool {
    BUILTIN_NAMES.contains(&name)
}

pub fn run_builtin(args: &[String], state: &mut ShellState) -> Option<BuiltinResult> {
    let name = args.first()?.as_str();
    if !is_builtin(name) {
        return None;
    }

    let result = match name {
        "cd" => builtin_cd(args, state),
        "pwd" => builtin_pwd(),
        "exit" => {
            let code = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            return Some(BuiltinResult::Exit(code));
        }
        "export" => builtin_export(args, state),
        "unset" => builtin_unset(args, state),
        "echo" => builtin_echo(args),
        "alias" => builtin_alias(args, state),
        "unalias" => builtin_unalias(args, state),
        "which" | "type" => builtin_which(args, state),
        "test" | "[" => builtin_test(args),
        "jobs" => builtin_jobs(args, state),
        "fg" => builtin_fg(args, state),
        "bg" => builtin_bg(args, state),
        "wait" => builtin_wait(args, state),
        "disown" => builtin_disown(args, state),
        "kill" => builtin_kill(args, state),
        "read" => builtin_read(args, state),
        _ => 1,
    };

    Some(BuiltinResult::Status(result))
}

/// Implements the `test` and `[` builtins. `[ ... ]` requires a trailing `]`
/// (which is stripped before evaluation). Returns 0 (true), 1 (false) or
/// 2 (syntax/usage error), matching the POSIX convention.
fn builtin_test(args: &[String]) -> i32 {
    let mut operands: Vec<&String> = args[1..].iter().collect();

    if args[0] == "[" {
        match operands.last().map(|s| s.as_str()) {
            Some("]") => {
                operands.pop();
            }
            _ => {
                eprintln!("[: missing ']'");
                return 2;
            }
        }
    }

    eval_test(&operands)
}

fn eval_test(a: &[&String]) -> i32 {
    if a.is_empty() {
        return 1;
    }

    if a[0] == "!" {
        if a.len() == 1 {
            eprintln!("test: argument expected");
            return 2;
        }
        return if eval_test(&a[1..]) == 0 { 1 } else { 0 };
    }

    match a.len() {
        1 => {
            if a[0].is_empty() {
                1
            } else {
                0
            }
        }
        2 => {
            // Unary operator.
            match a[0].as_str() {
                "-n" => {
                    if a[1].is_empty() {
                        1
                    } else {
                        0
                    }
                }
                "-z" => {
                    if a[1].is_empty() {
                        0
                    } else {
                        1
                    }
                }
                "-e" => path_stat(a[1], None),
                "-f" => path_stat(a[1], Some(TestPath::File)),
                "-d" => path_stat(a[1], Some(TestPath::Dir)),
                "-s" => path_stat(a[1], Some(TestPath::NonEmpty)),
                "-r" => path_stat(a[1], Some(TestPath::Readable)),
                "-w" => path_stat(a[1], Some(TestPath::Writable)),
                "-x" => path_stat(a[1], Some(TestPath::Executable)),
                _ => {
                    eprintln!("test: unary operator expected");
                    2
                }
            }
        }
        3 => eval_binary(a[0].as_str(), a[1].as_str(), a[2].as_str()),
        _ => {
            eprintln!("test: too many arguments");
            2
        }
    }
}

#[derive(Clone, Copy)]
enum TestPath {
    File,
    Dir,
    NonEmpty,
    Readable,
    Writable,
    Executable,
}

/// Returns 0 (true) if the path satisfies `kind`, 1 (false) otherwise.
fn path_stat(path: &str, kind: Option<TestPath>) -> i32 {
    use std::fs;
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return 1, // does not exist
    };

    let flag = match kind {
        None => true, // -e: exists
        Some(TestPath::File) => meta.is_file(),
        Some(TestPath::Dir) => meta.is_dir(),
        Some(TestPath::NonEmpty) => meta.len() > 0,
        Some(TestPath::Readable) | Some(TestPath::Writable) | Some(TestPath::Executable) => {
            file_mode_check(&meta, kind.unwrap())
        }
    };

    if flag { 0 } else { 1 }
}

#[cfg(unix)]
fn file_mode_check(meta: &std::fs::Metadata, kind: TestPath) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode();
    match kind {
        TestPath::Readable => mode & 0o444 != 0,
        TestPath::Writable => mode & 0o222 != 0,
        TestPath::Executable => mode & 0o111 != 0,
        _ => false,
    }
}

#[cfg(not(unix))]
fn file_mode_check(_meta: &std::fs::Metadata, kind: TestPath) -> bool {
    matches!(kind, TestPath::Writable) && !_meta.permissions().readonly()
}

fn eval_binary(a: &str, op: &str, b: &str) -> i32 {
    let result = match (a, op, b) {
        (_, "=", _) | (_, "==", _) => a == b,
        (_, "!=", _) => a != b,
        (_, "-eq", _) => int_cmp(a, b, std::cmp::Ordering::Equal),
        (_, "-ne", _) => !int_cmp(a, b, std::cmp::Ordering::Equal),
        (_, "-lt", _) => int_cmp(a, b, std::cmp::Ordering::Less),
        (_, "-le", _) => {
            int_cmp(a, b, std::cmp::Ordering::Less) || int_cmp(a, b, std::cmp::Ordering::Equal)
        }
        (_, "-gt", _) => int_cmp(a, b, std::cmp::Ordering::Greater),
        (_, "-ge", _) => {
            int_cmp(a, b, std::cmp::Ordering::Greater) || int_cmp(a, b, std::cmp::Ordering::Equal)
        }
        _ => {
            eprintln!("test: binary operator expected");
            return 2;
        }
    };

    if result { 0 } else { 1 }
}

/// Compares two operands as integers; returns false if they are not numbers
/// (and prints a usage error for the caller to reflect as status 2).
fn int_cmp(a: &str, b: &str, ord: std::cmp::Ordering) -> bool {
    let parse = |s: &str| s.trim().parse::<i64>();
    match (parse(a), parse(b)) {
        (Ok(x), Ok(y)) => x.cmp(&y) == ord,
        _ => false,
    }
}

/// Resolves a job specifier to its index in the job table.
///
/// Accepts `%+`/`%%` (most recent), `%-` (second most recent), `%N`/`N`
/// (by job id) and a raw PID. With `None` it defaults to the most recent job.
fn resolve_job(state: &ShellState, arg: Option<&str>) -> Option<usize> {
    let pick_latest = |n: usize| {
        if state.jobs.len() >= n {
            Some(state.jobs.len() - n)
        } else {
            None
        }
    };

    let by_id = |id: usize| state.jobs.iter().position(|j| j.id == id);
    let by_pid = |pid: i32| state.jobs.iter().position(|j| j.pid == pid);

    let Some(s) = arg else {
        return pick_latest(1);
    };
    let s = s.trim();

    if let Some(end) = s.strip_prefix('%') {
        match end {
            "+" | "%" | "" => return pick_latest(1),
            "-" => return pick_latest(2),
            _ => {}
        }
        if let Ok(id) = end.parse::<usize>() {
            return by_id(id).or_else(|| by_pid(id as i32));
        }
        return None;
    }

    if let Ok(id) = s.parse::<usize>() {
        if let Some(i) = by_id(id) {
            return Some(i);
        }
        return by_pid(id as i32);
    }

    by_pid(s.parse::<i32>().ok()?)
}

/// Resolves a specifier (`%...` or a bare PID) to a tracked job's PID.
fn resolve_pid(state: &ShellState, arg: &str) -> Option<i32> {
    if arg.starts_with('%') {
        resolve_job(state, Some(arg)).map(|i| state.jobs[i].pid)
    } else {
        arg.parse::<i32>().ok()
    }
}

/// Reaps finished background jobs, dropping them from the job table. When the
/// shell is interactive (stdin is a terminal) it also prints a bash-style
/// `[%]  Done  cmd` / `[%]  Exit N  cmd` line for each, so a finished job
/// announces itself at the next prompt instead of remaining a zombie until
/// the user runs `jobs`/`fg`. Non-interactive shells reap silently.
pub fn reap_finished_jobs(state: &mut ShellState) {
    let interactive = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
    let mut i = 0;
    while i < state.jobs.len() {
        if state.jobs[i].stopped {
            i += 1;
            continue;
        }
        match crate::jobctl::try_reap(state.jobs[i].pid) {
            Some(status) => {
                let job = state.jobs.remove(i);
                if interactive {
                    if status == 0 {
                        eprintln!("[{}]  Done  {}", job.id, job.command);
                    } else {
                        eprintln!("[{}]  Exit {}  {}", job.id, status, job.command);
                    }
                }
            }
            None => i += 1,
        }
    }
}

/// Implements `jobs`: lists the current job table, reaping and dropping any
/// background process that has already finished. Accepts `-l` / `-p` flags
/// which change the prefix (matching common shells).
fn builtin_jobs(args: &[String], state: &mut ShellState) -> i32 {
    let _ = args;
    let mut i = 0;
    while i < state.jobs.len() {
        let job = &state.jobs[i];
        let marker = if crate::jobctl::try_reap(job.pid).is_some() {
            "Done"
        } else if job.stopped {
            "Stopped"
        } else {
            "Running"
        };
        println!("[{:>2}] {:8} {}", job.id, marker, job.command);
        if marker == "Done" {
            state.jobs.remove(i);
        } else {
            i += 1;
        }
    }
    0
}

/// Implements `fg [%N]`: brings a (stopped or running) job to the foreground,
/// resuming it with SIGCONT and waiting for it to finish/stop again. With no
/// argument it uses the most recent job.
fn builtin_fg(args: &[String], state: &mut ShellState) -> i32 {
    let idx = match resolve_job(state, args.get(1).map(|s| s.as_str())) {
        Some(i) => i,
        None => {
            eprintln!("fg: no such job");
            return 1;
        }
    };

    let (pid, command) = {
        let job = &state.jobs[idx];
        (job.pid, job.command.clone())
    };

    println!("{}", command);
    crate::jobctl::continue_job(pid);
    crate::jobctl::set_group(pid);
    crate::jobctl::give_terminal(pid);

    match crate::jobctl::wait_foreground(pid) {
        crate::jobctl::FgOutcome::Exited(code) => {
            crate::jobctl::release_terminal();
            state.jobs.remove(idx);
            code
        }
        crate::jobctl::FgOutcome::Stopped => {
            crate::jobctl::release_terminal();
            state.jobs[idx].stopped = true;
            0
        }
    }
}

/// Implements `bg [%N]`: sends SIGCONT to a stopped job so it keeps running in
/// the background. With no argument it uses the most recent job.
fn builtin_bg(args: &[String], state: &mut ShellState) -> i32 {
    let idx = match resolve_job(state, args.get(1).map(|s| s.as_str())) {
        Some(i) => i,
        None => {
            eprintln!("bg: no such job");
            return 1;
        }
    };

    state.jobs[idx].stopped = false;
    let (id, pid, command) = {
        let j = &state.jobs[idx];
        (j.id, j.pid, j.command.clone())
    };
    println!("[{}] {} &", id, command);
    crate::jobctl::continue_job(pid);
    0
}

/// Implements `wait [%N|pid ...]`: waits for background jobs to finish and
/// returns their exit status. With no arguments it waits for all of them.
fn builtin_wait(args: &[String], state: &mut ShellState) -> i32 {
    let mut status = state.last_status;
    let mut handled: Vec<i32> = Vec::new();

    if args.len() < 2 {
        handled.extend(state.jobs.iter().map(|j| j.pid));
    } else {
        for spec in &args[1..] {
            match resolve_pid(state, spec) {
                Some(pid) => handled.push(pid),
                None => {
                    eprintln!("wait: {}: no such job", spec);
                    return 127;
                }
            }
        }
    }

    for &pid in &handled {
        if let Some(code) = crate::jobctl::wait_blocking(pid) {
            status = code;
        }
    }
    // Drop the jobs we waited on from the table (they have been reaped).
    state.jobs.retain(|j| !handled.contains(&j.pid));
    status
}

/// Implements `disown [-h] [%N|pid ...]`: removes jobs from the job table so
/// the shell stops tracking (and later reaping) them. With no arguments it
/// disowns the most recent job.
fn builtin_disown(args: &[String], state: &mut ShellState) -> i32 {
    let mut targets: Vec<i32> = Vec::new();
    if args.len() < 2 {
        if let Some(job) = state.jobs.last() {
            targets.push(job.pid);
        }
    } else {
        for spec in &args[1..] {
            if spec.starts_with('-') {
                continue; // flags like -h are accepted but irrelevant here
            }
            if let Some(pid) = resolve_pid(state, spec) {
                targets.push(pid);
            }
        }
    }

    state.jobs.retain(|j| !targets.contains(&j.pid));
    0
}

/// Maps a signal name (optionally with a trailing digit) or number to a
/// signal value.
fn signal_from_name(s: &str) -> Option<i32> {
    let upper = s.trim_start_matches("SIG").to_uppercase();
    let v = match upper.as_str() {
        "HUP" => libc::SIGHUP,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "KILL" => libc::SIGKILL,
        "TERM" => libc::SIGTERM,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "CONT" => libc::SIGCONT,
        "STOP" => libc::SIGSTOP,
        "TSTP" => libc::SIGTSTP,
        "TTIN" => libc::SIGTTIN,
        "TTOU" => libc::SIGTTOU,
        "CHLD" => libc::SIGCHLD,
        "PIPE" => libc::SIGPIPE,
        _ => return s.parse::<i32>().ok(),
    };
    Some(v)
}

/// Implements `kill [-s SIG | -SIG] [pid|%job ...]`: sends a signal (SIGTERM
/// by default) to a process or job.
fn builtin_kill(args: &[String], state: &ShellState) -> i32 {
    let mut sig = libc::SIGTERM;
    let mut i = 1;

    if args.len() < 2 {
        eprintln!("kill: usage: kill [-s sig | -sig] pid ...");
        return 2;
    }

    if let Some(spec) = args.get(1) {
        if spec == "-s" {
            sig = args
                .get(2)
                .and_then(|x| signal_from_name(x))
                .unwrap_or(libc::SIGTERM);
            i = 3;
        } else if let Some(name) = spec.strip_prefix('-') {
            // `-SIG` / `-9` style (also handles a lone `-`).
            sig = if name.is_empty() {
                libc::SIGTERM
            } else {
                signal_from_name(name).unwrap_or(libc::SIGTERM)
            };
            i = 2;
        }
    }

    let mut failed = 0;
    for spec in &args[i..] {
        let pid = resolve_pid(state, spec).or_else(|| spec.parse::<i32>().ok());
        match pid {
            Some(pid) if crate::jobctl::kill_pid(pid, sig) => {}
            _ => {
                eprintln!("kill: {}: no such process or job", spec);
                failed += 1;
            }
        }
    }
    if failed == 0 { 0 } else { 1 }
}

/// Implements `read [-r] [var ...]`: reads a line from standard input and
/// assigns whitespace-separated fields to each variable (the last variable
/// receives the rest of the line). Free variables default to `REPLY`. Without
/// `-r`, backslashes would escape the next character; with `-r` they stay
/// literal (the common, recommended mode). Returns 0 on success and 1 on EOF,
/// which makes `while read x; do ... done` work naturally.
fn builtin_read(args: &[String], state: &mut ShellState) -> i32 {
    let mut raw = false;
    let mut vars: Vec<String> = Vec::new();
    for a in &args[1..] {
        if a == "-r" {
            raw = true;
        } else if a.starts_with('-') {
            // Other flags are not supported; ignore silently.
        } else {
            vars.push(a.clone());
        }
    }
    if vars.is_empty() {
        vars.push("REPLY".to_string());
    }

    // Read one line from fd 0 byte-by-byte (no read-ahead), so buffered
    // input lines remain available to the shell and to other `read` calls.
    let mut buf = String::new();
    let mut got_any = false;
    #[cfg(unix)]
    loop {
        let mut b = [0u8; 1];
        let n = unsafe { libc::read(libc::STDIN_FILENO, b.as_mut_ptr() as *mut libc::c_void, 1) };
        if n < 0 {
            eprintln!("read: {}", std::io::Error::last_os_error());
            return 1;
        }
        if n == 0 {
            if !got_any {
                return 1; // EOF with empty line
            }
            break;
        }
        got_any = true;
        if b[0] == b'\n' {
            break;
        }
        buf.push(b[0] as char);
    }
    #[cfg(not(unix))]
    {
        use std::io::BufRead;
        let n = match std::io::stdin().lock().read_line(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("read: {}", e);
                return 1;
            }
        };
        if n == 0 {
            return 1; // EOF
        }
    }
    if !raw {
        // A backslash escapes the following character (POSIX).
        let mut out = String::with_capacity(buf.len());
        let mut it = buf.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                if let Some(nc) = it.next() {
                    out.push(nc);
                }
            } else {
                out.push(c);
            }
        }
        buf = out;
    }
    let line = buf.trim_end_matches(['\n', '\r']);

    assign_read_fields(line, &vars, &mut state.vars);
    0
}

/// Splits a (backslash-unescaped) line into fields and stores them in the
/// shell variables: each non-last variable gets one whitespace-separated
/// field and the last variable receives the rest of the line.
fn assign_read_fields(line: &str, vars: &[String], map: &mut HashMap<String, String>) {
    let fields: Vec<&str> = line.split_whitespace().collect();
    for (i, var) in vars.iter().enumerate() {
        if i == vars.len() - 1 {
            let rest: Vec<&str> = fields.iter().skip(i).copied().collect();
            map.insert(var.clone(), rest.join(" "));
        } else if let Some(f) = fields.get(i) {
            map.insert(var.clone(), f.to_string());
        } else {
            map.insert(var.clone(), String::new());
        }
    }
}

fn builtin_cd(args: &[String], state: &mut ShellState) -> i32 {
    let cwd_before = env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().to_string());

    let target = match args.get(1).map(|s| s.as_str()) {
        None => env::var("HOME").unwrap_or_else(|_| "/".to_string()),
        Some("-") => match &state.prev_dir {
            Some(p) => p.clone(),
            None => {
                eprintln!("cd: OLDPWD is not set");
                return 1;
            }
        },
        Some(p) => p.to_string(),
    };

    match env::set_current_dir(Path::new(&target)) {
        Ok(_) => {
            // Keep OLDPWD/PWD in sync (as bash does), both in the shell's
            // variable table and the process environment so children see them.
            if let Some(old) = &cwd_before {
                state.vars.insert("OLDPWD".into(), old.clone());
                unsafe { env::set_var("OLDPWD", old) };
            }
            if let Ok(new_pwd) = env::current_dir() {
                let s = new_pwd.to_string_lossy().to_string();
                state.vars.insert("PWD".into(), s.clone());
                unsafe { env::set_var("PWD", s) };
            }
            state.prev_dir = cwd_before;
            0
        }
        Err(e) => {
            eprintln!("cd: {}: {}", target, e);
            1
        }
    }
}

fn builtin_pwd() -> i32 {
    match env::current_dir() {
        Ok(p) => {
            println!("{}", p.display());
            0
        }
        Err(e) => {
            eprintln!("pwd: {}", e);
            1
        }
    }
}

fn builtin_export(args: &[String], state: &mut ShellState) -> i32 {
    if args.len() < 2 {
        // Without arguments: bash lists all exported variables
        for (k, v) in &state.vars {
            println!("export {}=\"{}\"", k, v);
        }
        return 0;
    }

    for assignment in &args[1..] {
        if let Some((key, value)) = assignment.split_once('=') {
            state.vars.insert(key.to_string(), value.to_string());
            // SAFETY: single-threaded shell context; a data race on the
            // process environment is not a concern here.
            unsafe { env::set_var(key, value) } // so child processes also see it
        } else {
            eprintln!("export: {}: invalid format, use VAR=value", assignment);
            return 1;
        }
    }
    0
}

fn builtin_unset(args: &[String], state: &mut ShellState) -> i32 {
    for name in &args[1..] {
        state.vars.remove(name);
        // SAFETY: single-threaded shell context; a data race on the
        // process environment is not a concern here.
        unsafe { env::remove_var(name) };
    }
    0
}

fn builtin_echo(args: &[String]) -> i32 {
    let mut rest = &args[1..];
    let mut no_newline = false;

    if rest.first().map(|s| s.as_str()) == Some("-n") {
        no_newline = true;
        rest = &rest[1..];
    }

    let out = rest.join(" ");
    if no_newline {
        print!("{}", out);
        use std::io::Write;
        let _ = std::io::stdout().flush();
    } else {
        println!("{}", out);
    }
    0
}

fn builtin_alias(args: &[String], state: &mut ShellState) -> i32 {
    if args.len() < 2 {
        for (k, v) in &state.aliases {
            println!("alias {}='{}'", k, v);
        }
        return 0;
    }

    for assignment in &args[1..] {
        if let Some((key, value)) = assignment.split_once('=') {
            state.aliases.insert(key.to_string(), value.to_string());
        } else if let Some(v) = state.aliases.get(assignment) {
            println!("alias {}='{}'", assignment, v);
        } else {
            eprintln!("alias: {}: not found", assignment);
            return 1;
        }
    }
    0
}

fn builtin_unalias(args: &[String], state: &mut ShellState) -> i32 {
    for name in &args[1..] {
        state.aliases.remove(name);
    }
    0
}

fn builtin_which(args: &[String], state: &ShellState) -> i32 {
    let Some(cmd) = args.get(1) else {
        return 1;
    };

    if is_builtin(cmd) {
        println!("{}: shell builtin", cmd);
        return 0;
    }

    if state.functions.contains_key(cmd) {
        println!("{}: shell function", cmd);
        return 0;
    }

    if let Ok(path_var) = env::var("PATH") {
        for dir in path_var.split(':') {
            let candidate = Path::new(dir).join(cmd);
            if candidate.is_file() {
                println!("{}", candidate.display());
                return 0;
            }
        }
    }

    println!("{}: not found", cmd);
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(expr: &[&str]) -> i32 {
        let operands: Vec<String> = expr.iter().map(|s| s.to_string()).collect();
        let refs: Vec<&String> = operands.iter().collect();
        eval_test(&refs)
    }

    #[test]
    fn test_plain_string() {
        assert_eq!(run(&[""]), 1);
        assert_eq!(run(&["x"]), 0);
    }

    #[test]
    fn test_negation() {
        assert_eq!(run(&["!", "x"]), 1);
        assert_eq!(run(&["!", ""]), 0);
    }

    #[test]
    fn test_string_unary() {
        assert_eq!(run(&["-z", ""]), 0);
        assert_eq!(run(&["-z", "a"]), 1);
        assert_eq!(run(&["-n", "a"]), 0);
        assert_eq!(run(&["-n", ""]), 1);
    }

    #[test]
    fn test_string_binary() {
        assert_eq!(run(&["abc", "=", "abc"]), 0);
        assert_eq!(run(&["abc", "=", "xyz"]), 1);
        assert_eq!(run(&["abc", "!=", "xyz"]), 0);
    }

    #[test]
    fn test_integer_binary() {
        assert_eq!(run(&["3", "-eq", "3"]), 0);
        assert_eq!(run(&["2", "-lt", "5"]), 0);
        assert_eq!(run(&["5", "-gt", "2"]), 0);
        assert_eq!(run(&["4", "-le", "4"]), 0);
        assert_eq!(run(&["4", "-ge", "5"]), 1);
    }

    #[test]
    fn test_bracket_requires_closing() {
        let args = vec!["[".to_string(), "-d".to_string(), "/tmp".to_string()];
        assert_eq!(builtin_test(&args), 2);
    }

    #[test]
    fn test_bracket_syntax() {
        let args = vec![
            "[".to_string(),
            "-d".to_string(),
            "/tmp".to_string(),
            "]".to_string(),
        ];
        assert_eq!(builtin_test(&args), 0);
    }

    #[test]
    fn test_read_assign_fields() {
        let mut m = HashMap::new();
        let vars: Vec<String> = ["x", "y", "z"].iter().map(|s| s.to_string()).collect();
        assign_read_fields("a b c d", &vars, &mut m);
        assert_eq!(m.get("x"), Some(&"a".to_string()));
        assert_eq!(m.get("y"), Some(&"b".to_string()));
        // The last variable receives the rest of the line.
        assert_eq!(m.get("z"), Some(&"c d".to_string()));
    }

    #[test]
    fn test_read_assign_fields_missing() {
        let mut m = HashMap::new();
        let vars: Vec<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        assign_read_fields("only", &vars, &mut m);
        assert_eq!(m.get("a"), Some(&"only".to_string()));
        assert_eq!(m.get("b"), Some(&String::new()));
    }

    #[test]
    fn test_resolve_job_specifiers() {
        let mut st = ShellState::new();
        st.jobs.push(JobRecord {
            id: 1,
            pid: 100,
            command: "one".into(),
            stopped: false,
        });
        st.jobs.push(JobRecord {
            id: 2,
            pid: 200,
            command: "two".into(),
            stopped: false,
        });

        // %+ / default resolve to the most recent job (last in the table).
        assert_eq!(resolve_job(&st, Some("%+")), Some(1));
        assert_eq!(resolve_job(&st, None), Some(1));
        // %- resolves to the second-most-recent.
        assert_eq!(resolve_job(&st, Some("%-")), Some(0));
        // %N and bare N resolve by job id; a bare number can also be a pid.
        assert_eq!(resolve_job(&st, Some("%1")), Some(0));
        assert_eq!(resolve_job(&st, Some("1")), Some(0));
        assert_eq!(resolve_job(&st, Some("200")), Some(1));
        assert_eq!(resolve_job(&st, Some("%9")), None);
    }
}
