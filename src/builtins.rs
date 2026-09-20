use std::collections::HashMap;
use std::env;
use std::path::Path;

/// Result of trying to run a builtin.
/// `None` means "this is not a builtin, run it as an external command".
pub enum BuiltinResult {
    Exit(i32),
    Status(i32),
}

pub struct ShellState {
    pub vars: HashMap<String, String>,
    pub aliases: HashMap<String, String>,
    pub last_status: i32,
    pub prev_dir: Option<String>,
}

impl ShellState {
    pub fn new() -> Self {
        ShellState {
            vars: HashMap::new(),
            aliases: HashMap::new(),
            last_status: 0,
            prev_dir: None,
        }
    }
}

const BUILTIN_NAMES: &[&str] = &[
    "cd", "pwd", "exit", "export", "unset", "echo", "alias", "unalias", "which", "type",
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
        "which" | "type" => builtin_which(args),
        _ => 1,
    };

    Some(BuiltinResult::Status(result))
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

fn builtin_which(args: &[String]) -> i32 {
    let Some(cmd) = args.get(1) else {
        return 1;
    };

    if is_builtin(cmd) {
        println!("{}: shell builtin", cmd);
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
