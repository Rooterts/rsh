mod builtins;
mod executor;
mod expand;
mod parser;
mod tokenizer;

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::env;

use builtins::ShellState;

fn main() {
    let mut state = ShellState::new();
    let mut rl = DefaultEditor::new().expect("failed to initialize the line editor");

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
