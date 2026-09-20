mod builtins;
mod executor;
mod expand;
mod jobctl;
mod parser;
mod tokenizer;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};
use std::env;
use std::fs;
use std::path::PathBuf;

use builtins::ShellState;

/// Reserved words and builtins offered by command-name completion.
const SHELL_WORDS: &[&str] = &[
    "cd", "pwd", "exit", "export", "unset", "echo", "alias", "unalias", "which", "type", "test",
    "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "until", "case",
    "esac",
];

/// rustyline helper that tab-completes file paths and command names.
#[derive(Default)]
struct RshCompleter;

impl Completer for RshCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> Result<(usize, Vec<Pair>), ReadlineError> {
        let start = word_start(line, pos);
        let prefix = &line[start..pos];
        let candidates = completion_candidates(prefix);
        let pairs: Vec<Pair> = candidates
            .into_iter()
            .map(|c| Pair {
                replacement: c.clone(),
                display: c,
            })
            .collect();
        Ok((start, pairs))
    }
}

impl Hinter for RshCompleter {
    type Hint = String;
}
impl Highlighter for RshCompleter {}
impl Validator for RshCompleter {}
impl Helper for RshCompleter {}

/// Byte index where the word currently under the cursor begins.
fn word_start(line: &str, pos: usize) -> usize {
    line[..pos]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace() || "|;&<>()".contains(*c))
        .map(|(i, _)| i + 1)
        .unwrap_or(0)
}

fn completion_candidates(prefix: &str) -> Vec<String> {
    if prefix.starts_with('/')
        || prefix.starts_with('.')
        || prefix.starts_with('~')
        || prefix.contains('/')
    {
        path_candidates(prefix)
    } else {
        command_candidates(prefix)
    }
}

fn command_candidates(prefix: &str) -> Vec<String> {
    let mut out: Vec<String> = SHELL_WORDS
        .iter()
        .filter(|c| c.starts_with(prefix))
        .map(|s| s.to_string())
        .collect();

    if let Ok(path) = env::var("PATH") {
        for dir in path.split(':') {
            if let Ok(rd) = fs::read_dir(dir) {
                for entry in rd.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(prefix) && is_executable(&entry) {
                        out.push(name);
                    }
                }
            }
        }
    }

    out
}

fn path_candidates(prefix: &str) -> Vec<String> {
    let (dir, base) = match prefix.rfind('/') {
        Some(i) => (&prefix[..=i], &prefix[i + 1..]),
        None => ("", prefix),
    };

    let bucket = match fs::read_dir(resolve_dir(dir)) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for entry in bucket.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(base) {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let suffix = if is_dir { "/" } else { "" };
            out.push(format!("{}{}{}", dir, name, suffix));
        }
    }
    out
}

/// The directory to list for a given (possibly `~`-prefixed) path prefix.
fn resolve_dir(dir: &str) -> PathBuf {
    if let Some(rest) = dir.strip_prefix("~/")
        && let Ok(home) = env::var("HOME")
    {
        return PathBuf::from(&home).join(rest);
    }
    if dir.is_empty() {
        return PathBuf::from(".");
    }
    PathBuf::from(dir)
}

#[cfg(unix)]
fn is_executable(entry: &fs::DirEntry) -> bool {
    use std::os::unix::fs::PermissionsExt;
    entry
        .metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_entry: &fs::DirEntry) -> bool {
    false
}

fn main() {
    let mut state = ShellState::new();
    // Ignore ^Z for the shell itself (it is not a job), so the signal only
    // reaches foreground children in their own process groups.
    #[cfg(unix)]
    crate::jobctl::shell_setup();

    let mut rl = Editor::<RshCompleter, DefaultHistory>::new()
        .expect("failed to initialize the line editor");
    rl.set_helper(Some(RshCompleter));

    let history_path = history_file_path();
    let _ = rl.load_history(&history_path);

    loop {
        let prompt = build_prompt(&state);

        match rl.readline(&prompt) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let _ = rl.add_history_entry(trimmed);

                match tokenizer::tokenize(trimmed) {
                    Ok(tokens) => match parser::parse(tokens) {
                        Ok(jobs) => {
                            if let Some(code) = executor::run_jobs(&jobs, &mut state) {
                                let _ = rl.save_history(&history_path);
                                std::process::exit(code);
                            }
                        }
                        Err(e) => eprintln!("rsh: syntax error: {}", e),
                    },
                    Err(e) => eprintln!("rsh: lexical error: {}", e),
                }
            }
            // Ctrl+C: like bash, cancel the current line instead of exiting.
            Err(ReadlineError::Interrupted) => continue,
            // Ctrl+D: exits the shell, as in bash.
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("rsh: error reading input: {}", e);
                break;
            }
        }
    }

    let _ = rl.save_history(&history_path);
}

fn build_prompt(state: &ShellState) -> String {
    let cwd = env::current_dir()
        .map(|p| shorten_home(&p.to_string_lossy()))
        .unwrap_or_else(|_| "?".to_string());

    let marker = if state.last_status == 0 { "$" } else { "✗" };
    format!("rsh:{} {} ", cwd, marker)
}

/// Replaces the user's home directory with ~ in the prompt, like bash.
fn shorten_home(path: &str) -> String {
    if let Ok(home) = env::var("HOME")
        && let Some(rest) = path.strip_prefix(&home)
    {
        if rest.is_empty() {
            return "~".to_string();
        }
        if rest.starts_with('/') {
            return format!("~{}", rest);
        }
    }
    path.to_string()
}

fn history_file_path() -> String {
    env::var("HOME")
        .map(|h| format!("{}/.rsh_history", h))
        .unwrap_or_else(|_| ".rsh_history".to_string())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_word_start() {
        assert_eq!(word_start("echo hola", 9), 5);
        assert_eq!(word_start("echo hola", 5), 5);
        assert_eq!(word_start("cd /us", 6), 3);
        assert_eq!(word_start("ls | gre", 8), 5);
        assert_eq!(word_start("single", 6), 0);
    }

    #[test]
    fn test_command_candidates() {
        let c = command_candidates("ec");
        assert!(c.iter().any(|x| x == "echo"));
        let c2 = command_candidates("whi");
        assert!(c2.iter().any(|x| x == "which"));
    }

    #[test]
    fn test_path_candidates_slash() {
        let c = path_candidates("/usr/");
        assert!(c.iter().any(|p| p == "/usr/bin/"));
    }

    #[test]
    fn test_path_candidates_relative() {
        let c = path_candidates("./C");
        assert!(c.iter().any(|p| p.starts_with("./Cargo")));
    }
}
