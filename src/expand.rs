use std::collections::HashMap;
use std::env;

use crate::tokenizer::MARK;

/// Expands a list of "raw" arguments (as produced by the parser) to their
/// final form, following the POSIX order: command substitution and parameter
/// expansion first, then tilde, then pathname expansion (glob), and finally
/// "quote removal" (stripping the literalness marks).
///
/// `run_subst` is a callback that executes the contents of a `$(...)` and
/// returns what it printed to stdout — injected here by the caller
/// (executor.rs) because expand.rs does not know how to run commands.
pub fn expand_args(
    args: &[String],
    shell_vars: &mut HashMap<String, String>,
    last_status: i32,
    run_subst: &mut dyn FnMut(&str) -> String,
) -> Vec<String> {
    let mut expanded = Vec::new();

    for arg in args {
        let after_subst = expand_command_subst(arg, run_subst);
        let after_vars = expand_vars(&after_subst, shell_vars, last_status);
        let after_tilde = expand_tilde(&after_vars);

        if has_unmarked_glob_char(&after_tilde) {
            let pattern = to_glob_pattern(&after_tilde);
            match glob::glob(&pattern) {
                Ok(paths) => {
                    let matches: Vec<String> = paths
                        .filter_map(|p| p.ok())
                        .map(|p| p.to_string_lossy().to_string())
                        .collect();
                    if matches.is_empty() {
                        // No matches: like bash, the pattern is left literal.
                        expanded.push(strip_marks(&after_tilde));
                    } else {
                        expanded.extend(matches);
                    }
                }
                Err(_) => expanded.push(strip_marks(&after_tilde)),
            }
        } else {
            expanded.push(strip_marks(&after_tilde));
        }
    }

    expanded
}

/// Finds `$(...)` that are NOT marked (i.e. outside single quotes) and
/// replaces them with the captured command output. The captured result is
/// marked entirely as literal, so it is not re-expanded or globbed by
/// accident (avoids surprises if a file happens to be named "notas*.txt").
fn expand_command_subst(input: &str, run: &mut dyn FnMut(&str) -> String) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == MARK {
            out.push(chars[i]);
            if i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        if chars[i] == '$' && chars.get(i + 1) == Some(&'(') {
            let mut depth = 1;
            let mut j = i + 2;
            while j < chars.len() && depth > 0 {
                match chars[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    break;
                }
                j += 1;
            }
            let inner: String = chars[i + 2..j].iter().collect();
            let output = run(&inner);
            out.push_str(&mark_all_special(&output));
            i = j + 1;
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

/// Marks $ * ? [ ~ so that the text becomes fully literal — used on the
/// output of a command in `$(...)`.
fn mark_all_special(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if matches!(c, '$' | '*' | '?' | '[' | '~') {
            out.push(MARK);
        }
        out.push(c);
    }
    out
}

/// Expands `$VAR`, `${VAR...}` and `$?` — respects MARK (does not touch
/// anything marked literal). Inside double quotes the `?` of `$?` arrives
/// marked (the tokenizer marks `?` to prevent pathname expansion), so we
/// also accept `$` + MARK + `?` and treat it as the status expansion.
fn expand_vars(input: &str, shell_vars: &mut HashMap<String, String>, last_status: i32) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == MARK {
            out.push(chars[i]);
            if i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        if chars[i] == '$' && i + 1 < chars.len() {
            if chars[i + 1] == '?' {
                out.push_str(&last_status.to_string());
                i += 2;
                continue;
            }

            // `$?` written as `$?` inside double quotes: the `?` is marked,
            // so the tokenizer produced `$` + MARK + `?`.
            if chars[i + 1] == MARK && chars.get(i + 2) == Some(&'?') {
                out.push_str(&last_status.to_string());
                i += 3;
                continue;
            }

            if chars[i + 1] == '{'
                && let Some(end_rel) = chars[i + 2..].iter().position(|&c| c == '}')
            {
                let inner: String = chars[i + 2..i + 2 + end_rel].iter().collect();
                out.push_str(&expand_braced_param(&inner, shell_vars));
                i = i + 2 + end_rel + 1;
                continue;
            }

            if chars[i + 1].is_alphabetic() || chars[i + 1] == '_' {
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let name: String = chars[i + 1..j].iter().collect();
                out.push_str(&lookup_var(&name, shell_vars));
                i = j;
                continue;
            }
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

/// Handles ${NAME}, ${#NAME}, ${NAME:-def}, ${NAME:=def}, ${NAME:?err}, ${NAME:+alt}.
fn expand_braced_param(inner: &str, shell_vars: &mut HashMap<String, String>) -> String {
    if let Some(name) = inner.strip_prefix('#') {
        let val = lookup_var(name, shell_vars);
        return val.chars().count().to_string();
    }

    for op in [":-", ":=", ":?", ":+"] {
        if let Some(pos) = inner.find(op) {
            let name = &inner[..pos];
            let word = &inner[pos + op.len()..];
            let current = lookup_var(name, shell_vars);
            let unset_or_empty = current.is_empty();

            return match op {
                ":-" => {
                    if unset_or_empty {
                        word.to_string()
                    } else {
                        current
                    }
                }
                ":=" => {
                    if unset_or_empty {
                        shell_vars.insert(name.to_string(), word.to_string());
                        // SAFETY: single-threaded shell context; a data race
                        // on the process environment is not a concern here.
                        unsafe { env::set_var(name, word) };
                        word.to_string()
                    } else {
                        current
                    }
                }
                ":?" => {
                    if unset_or_empty {
                        let msg = if word.is_empty() {
                            "parameter null or not set"
                        } else {
                            word
                        };
                        eprintln!("rsh: {}: {}", name, msg);
                        String::new()
                    } else {
                        current
                    }
                }
                ":+" => {
                    if unset_or_empty {
                        String::new()
                    } else {
                        word.to_string()
                    }
                }
                _ => unreachable!(),
            };
        }
    }

    lookup_var(inner, shell_vars)
}

fn lookup_var(name: &str, shell_vars: &HashMap<String, String>) -> String {
    if let Some(v) = shell_vars.get(name) {
        return v.clone();
    }
    env::var(name).unwrap_or_default()
}

/// Expands `~` to the home directory, only at the start of the argument and
/// only if it is not marked (i.e. did not come from quotes).
fn expand_tilde(input: &str) -> String {
    if let Some(rest) = input.strip_prefix('~')
        && (rest.is_empty() || rest.starts_with('/'))
        && let Ok(home) = env::var("HOME")
    {
        return format!("{}{}", home, rest);
    }
    input.to_string()
}

/// True if there is any `* ? [` WITHOUT a mark (i.e. a real pathname
/// expansion candidate).
fn has_unmarked_glob_char(s: &str) -> bool {
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == MARK {
            chars.next();
            continue;
        }
        if matches!(c, '*' | '?' | '[') {
            return true;
        }
    }
    false
}

/// Converts the string (with marks) into a pattern that the `glob` crate
/// understands, escaping marked characters as literals (e.g. MARK+'*' -> "[*]").
fn to_glob_pattern(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == MARK {
            if let Some(next) = chars.next() {
                match next {
                    '*' => out.push_str("[*]"),
                    '?' => out.push_str("[?]"),
                    '[' => out.push_str("[[]"),
                    other => out.push(other),
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Strips the literalness marks that survived (final quote removal).
fn strip_marks(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == MARK {
            if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_var() {
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "bar".to_string());
        assert_eq!(expand_vars("hola $FOO!", &mut vars, 0), "hola bar!");
    }

    #[test]
    fn test_expand_status() {
        let mut vars = HashMap::new();
        assert_eq!(expand_vars("exit code: $?", &mut vars, 42), "exit code: 42");
    }

    #[test]
    fn test_expand_status_inside_double_quotes() {
        // Inside double quotes the tokenizer marks the `?` as literal, so
        // the expansion sees `$` + MARK + `?`. It must still become the status.
        let mut vars = HashMap::new();
        // Build `"status=$"` + MARK + `?` by hand (as the tokenizer would emit).
        let mut raw = String::from("status=$");
        raw.push(MARK);
        raw.push('?');
        assert_eq!(expand_vars(&raw, &mut vars, 7), "status=7");
    }

    #[test]
    fn test_default_value() {
        let mut vars = HashMap::new();
        assert_eq!(expand_vars("${FOO:-bar}", &mut vars, 0), "bar");
    }

    #[test]
    fn test_assign_default() {
        let mut vars = HashMap::new();
        expand_vars("${FOO:=bar}", &mut vars, 0);
        assert_eq!(vars.get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn test_length() {
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "hola".to_string());
        assert_eq!(expand_vars("${#FOO}", &mut vars, 0), "4");
    }

    #[test]
    fn test_single_quote_protects_dollar() {
        // Simulates what the tokenizer produces for 'echo "$FOO"' vs 'echo '\''$FOO'\'''
        let marked = format!("{}$FOO", MARK); // marked $ = literal
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "bar".to_string());
        let result = strip_marks(&expand_vars(&marked, &mut vars, 0));
        assert_eq!(result, "$FOO");
    }
}
