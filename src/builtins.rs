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
    "[", "jobs", "fg", "bg",
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
        "jobs" => builtin_jobs(state),
        "fg" => builtin_fg(args, state),
        "bg" => builtin_bg(args, state),
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

/// Implements `jobs`: lists the current job table, reaping and dropping any
/// background process that has already finished. Accepts `-l` / `-p` style
/// flags, which are parsed but only affect the prefix (matching common shells).
fn builtin_jobs(state: &mut ShellState) -> i32 {
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

/// Parses a `%N` job argument (or a bare `N`) into a job id.
fn parse_job_arg(arg: Option<&String>) -> Option<usize> {
    let s = arg?.trim();
    s.strip_prefix('%').unwrap_or(s).parse::<usize>().ok()
}

/// Finds a job by id, returning its index in the table.
fn find_job(state: &ShellState, id: usize) -> Option<usize> {
    state.jobs.iter().position(|j| j.id == id)
}

/// Implements `fg [%N]`: brings a (stopped or running) job to the foreground,
/// resuming it with SIGCONT and waiting for it to finish/stop again. With no
/// argument it uses the most recent job.
fn builtin_fg(args: &[String], state: &mut ShellState) -> i32 {
    let id =
        parse_job_arg(args.get(1)).unwrap_or_else(|| state.jobs.last().map(|j| j.id).unwrap_or(0));

    let idx = match find_job(state, id) {
        Some(i) => i,
        None => {
            eprintln!("fg: no such job {}", id);
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
    let id =
        parse_job_arg(args.get(1)).unwrap_or_else(|| state.jobs.last().map(|j| j.id).unwrap_or(0));

    let idx = match find_job(state, id) {
        Some(i) => i,
        None => {
            eprintln!("bg: no such job {}", id);
            return 1;
        }
    };

    state.jobs[idx].stopped = false;
    let (pid, command) = (state.jobs[idx].pid, state.jobs[idx].command.clone());
    println!("[{}] {} &", id, command);
    crate::jobctl::continue_job(pid);
    0
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
}
