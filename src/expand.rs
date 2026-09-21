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
    positional: &[String],
    last_status: i32,
    run_subst: &mut dyn FnMut(&str) -> String,
) -> Vec<String> {
    let mut expanded = Vec::new();
    // POSIX: a word of the form NAME=value that appears before the command word
    // is an assignment. Tilde, parameter, command and arithmetic expansion are
    // applied to it, but NOT field splitting nor pathname (glob) expansion, so
    // its value is kept as one word even if it contains whitespace.
    let mut seen_command_word = false;

    for arg in args {
        let after_subst = expand_command_subst(arg, run_subst);
        let after_vars = expand_vars(&after_subst, shell_vars, positional, last_status);
        let after_tilde = expand_tilde(&after_vars);

        if !seen_command_word && is_assignment_word(&after_tilde) {
            expanded.push(strip_marks(&after_tilde));
            continue;
        }
        seen_command_word = true;

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
            // Field splitting: the result of an unquoted expansion ($VAR,
            // $(...), $@, arithmetic) may contain unmarked whitespace, which
            // separates it into several fields per IFS. Whitespace that came
            // from quotes is marked and therefore preserved inside one field.
            expanded.extend(split_fields(&after_tilde));
        }
    }

    expanded
}

/// Whether an expanded word is an assignment (`NAME=value`) with a valid name.
fn is_assignment_word(s: &str) -> bool {
    match s.split_once('=') {
        Some((name, _)) => {
            let mut chars = name.chars();
            match chars.next() {
                Some(c) if c.is_alphabetic() || c == '_' => {}
                _ => return false,
            }
            chars.all(|c| c.is_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Splits a (still MARK-ed) word into fields on runs of unmarked whitespace.
/// Each returned field has its literalness marks removed. Unmarked whitespace
/// at the start/end is dropped (IFS trimming); empty fields are skipped.
fn split_fields(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == MARK {
            // A marked char is literal/protected: carry it (and its target)
            // verbatim into the current field, so it is not treated as a split.
            cur.push(chars[i]);
            if i + 1 < chars.len() {
                cur.push(chars[i + 1]);
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if chars[i].is_whitespace() {
            if !cur.is_empty() {
                fields.push(strip_marks(&cur));
                cur.clear();
            }
            i += 1;
            continue;
        }
        cur.push(chars[i]);
        i += 1;
    }

    if !cur.is_empty() {
        fields.push(strip_marks(&cur));
    }

    fields
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

        if chars[i] == '$' && chars.get(i + 1) == Some(&'(') && chars.get(i + 2) == Some(&'(') {
            // $(( ... )) is arithmetic expansion, handled by expand_vars as a
            // parameter expansion. Leave it untouched here (just copy the `$`
            // through so expand_vars sees the `$((` opener).
            out.push(chars[i]);
            i += 1;
            continue;
        }

        if chars[i] == '$' && chars.get(i + 1) == Some(&'(') {
            // `$(( ... ))` is handled as arithmetic by expand_vars (see below);
            // here we only treat the command-substitution case. The matching
            // `)` is found with the same quote/escape-aware logic the
            // tokenizer uses, so a paren inside quotes does not cut it short.
            match crate::tokenizer::find_command_subst_end(&chars, i + 2) {
                Some(j) => {
                    let inner: String = chars[i + 2..j].iter().collect();
                    let output = run(&inner);
                    out.push_str(&mark_all_special(&output));
                    i = j + 1;
                }
                None => {
                    // Unclosed: leave the text as-is to avoid panicking.
                    out.push(chars[i]);
                    i += 1;
                }
            }
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
fn expand_vars(
    input: &str,
    shell_vars: &mut HashMap<String, String>,
    positional: &[String],
    last_status: i32,
) -> String {
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
            // Arithmetic expansion `$(( expr ))`: find the balanced group and
            // replace it with the integer result.
            if chars.get(i + 1) == Some(&'(') && chars.get(i + 2) == Some(&'(') {
                let mut depth = 0i32;
                let mut j = i;
                while j < chars.len() {
                    match chars[j] {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                if j < chars.len() {
                    // Inner expression is wrapped in an extra pair of parens
                    // (chars[i+2..j] == "( expr )"); the evaluator handles them.
                    // MARK characters are stripped: inside double quotes the
                    // tokenizer marks `* ? [` as literal to avoid globbing, but
                    // inside arithmetic they are always operators / wildcards.
                    let inner: String = chars[i + 2..j].iter().filter(|&&c| c != MARK).collect();
                    let value = eval_arithmetic(&inner, shell_vars, positional);
                    out.push_str(&value.to_string());
                    i = j + 1;
                    continue;
                }
            }

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

            // Positional parameters and the "special" single-char params.
            if chars[i + 1] == '@' {
                out.push_str(&positional.join(" "));
                i += 2;
                continue;
            }
            if chars[i + 1] == '#' {
                out.push_str(&positional.len().to_string());
                i += 2;
                continue;
            }
            if chars[i + 1].is_ascii_digit() {
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                let num: usize = chars[i + 1..j]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0);
                let value = if num == 0 {
                    "rsh".to_string() // $0 is the shell name
                } else {
                    positional
                        .get(num.saturating_sub(1))
                        .cloned()
                        .unwrap_or_default()
                };
                out.push_str(&value);
                i = j;
                continue;
            }

            if chars[i + 1] == '{'
                && let Some(end_rel) = chars[i + 2..].iter().position(|&c| c == '}')
            {
                let inner: String = chars[i + 2..i + 2 + end_rel].iter().collect();
                out.push_str(&expand_braced_param(&inner, shell_vars, positional));
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

/// Handles ${NAME}, ${#NAME}, ${NAME:-def}, ${NAME:=def}, ${NAME:?err},
/// ${NAME:+alt}, and the positional forms ${N}, ${@}, ${#}.
fn expand_braced_param(
    inner: &str,
    shell_vars: &mut HashMap<String, String>,
    positional: &[String],
) -> String {
    // ${#NAME} — length of a variable; ${#} — number of positional params.
    if let Some(name) = inner.strip_prefix('#') {
        if name.is_empty() {
            return positional.len().to_string();
        }
        let val = lookup_var(name, shell_vars);
        return val.chars().count().to_string();
    }

    // ${@}, ${0}, ${N}
    if !inner.is_empty() {
        if inner == "@" {
            return positional.join(" ");
        }
        if let Ok(num) = inner.parse::<usize>() {
            let value = if num == 0 {
                "rsh".to_string()
            } else {
                positional
                    .get(num.saturating_sub(1))
                    .cloned()
                    .unwrap_or_default()
            };
            return value;
        }
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

/// Evaluates an integer arithmetic expression with support for `+ - * / %`,
/// parentheses, unary minus, bare variable names, and `$name`/`$N` references
/// (whose numeric value is looked up from the shell variables, the positional
/// parameters, and the environment). On any parse or evaluation problem it
/// returns 0, matching a lenient `expr`-like behavior.
fn eval_arithmetic<'a>(
    input: &str,
    shell_vars: &'a HashMap<String, String>,
    positional: &'a [String],
) -> i64 {
    struct Arith<'a> {
        chars: Vec<char>,
        pos: usize,
        vars: &'a HashMap<String, String>,
        positional: &'a [String],
    }

    impl<'a> Arith<'a> {
        fn new(s: &str, vars: &'a HashMap<String, String>, positional: &'a [String]) -> Self {
            Self {
                chars: s.chars().collect(),
                pos: 0,
                vars,
                positional,
            }
        }
        fn skip_ws(&mut self) {
            while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
                self.pos += 1;
            }
        }
        fn peek(&self) -> Option<char> {
            self.chars.get(self.pos).copied()
        }
        fn raw_value(&self, name: &str) -> i64 {
            if let Ok(num) = name.parse::<usize>() {
                let v = if num == 0 {
                    "rsh".to_string()
                } else {
                    self.positional
                        .get(num.saturating_sub(1))
                        .cloned()
                        .unwrap_or_default()
                };
                return v.trim().parse::<i64>().unwrap_or(0);
            }
            let raw = self
                .vars
                .get(name)
                .cloned()
                .unwrap_or_else(|| env::var(name).unwrap_or_default());
            raw.trim().parse::<i64>().unwrap_or(0)
        }
        fn expr(&mut self) -> i64 {
            let mut v = self.term();
            loop {
                self.skip_ws();
                match self.peek() {
                    Some('+') => {
                        self.pos += 1;
                        v += self.term();
                    }
                    Some('-') => {
                        self.pos += 1;
                        v -= self.term();
                    }
                    _ => break,
                }
            }
            v
        }
        fn term(&mut self) -> i64 {
            let mut v = self.factor();
            loop {
                self.skip_ws();
                match self.peek() {
                    Some('*') => {
                        self.pos += 1;
                        v *= self.factor();
                    }
                    Some('/') => {
                        self.pos += 1;
                        let d = self.factor();
                        v = if d != 0 { v / d } else { 0 };
                    }
                    Some('%') => {
                        self.pos += 1;
                        let d = self.factor();
                        v = if d != 0 { v % d } else { 0 };
                    }
                    _ => break,
                }
            }
            v
        }
        fn factor(&mut self) -> i64 {
            self.skip_ws();
            match self.peek() {
                Some('-') => {
                    self.pos += 1;
                    -self.factor()
                }
                Some('+') => {
                    self.pos += 1;
                    self.factor()
                }
                Some('(') => {
                    self.pos += 1;
                    let v = self.expr();
                    self.skip_ws();
                    if self.peek() == Some(')') {
                        self.pos += 1;
                    }
                    v
                }
                Some('$') => {
                    // A `$name` / `$N` reference inside the arithmetic.
                    self.pos += 1;
                    if self.peek() == Some('{') {
                        self.pos += 1;
                    }
                    let start = self.pos;
                    while self.pos < self.chars.len()
                        && (self.chars[self.pos].is_alphanumeric() || self.chars[self.pos] == '_')
                    {
                        self.pos += 1;
                    }
                    let name: String = self.chars[start..self.pos].iter().collect();
                    if self.peek() == Some('}') {
                        self.pos += 1;
                    }
                    self.raw_value(&name)
                }
                Some(c) if c.is_ascii_digit() => {
                    let start = self.pos;
                    while self.pos < self.chars.len() && self.chars[self.pos].is_ascii_digit() {
                        self.pos += 1;
                    }
                    let s: String = self.chars[start..self.pos].iter().collect();
                    s.parse::<i64>().unwrap_or(0)
                }
                Some(c) if c.is_alphabetic() || c == '_' => {
                    let start = self.pos;
                    while self.pos < self.chars.len()
                        && (self.chars[self.pos].is_alphanumeric() || self.chars[self.pos] == '_')
                    {
                        self.pos += 1;
                    }
                    let name: String = self.chars[start..self.pos].iter().collect();
                    self.raw_value(&name)
                }
                _ => 0,
            }
        }
    }

    let mut a = Arith::new(input, shell_vars, positional);
    a.skip_ws();
    a.expr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_var() {
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "bar".to_string());
        assert_eq!(expand_vars("hola $FOO!", &mut vars, &[], 0), "hola bar!");
    }

    #[test]
    fn test_expand_status() {
        let mut vars = HashMap::new();
        assert_eq!(
            expand_vars("exit code: $?", &mut vars, &[], 42),
            "exit code: 42"
        );
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
        assert_eq!(expand_vars(&raw, &mut vars, &[], 7), "status=7");
    }

    #[test]
    fn test_default_value() {
        let mut vars = HashMap::new();
        assert_eq!(expand_vars("${FOO:-bar}", &mut vars, &[], 0), "bar");
    }

    #[test]
    fn test_assign_default() {
        let mut vars = HashMap::new();
        expand_vars("${FOO:=bar}", &mut vars, &[], 0);
        assert_eq!(vars.get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn test_length() {
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "hola".to_string());
        assert_eq!(expand_vars("${#FOO}", &mut vars, &[], 0), "4");
    }

    #[test]
    fn test_single_quote_protects_dollar() {
        // Simulates what the tokenizer produces for 'echo "$FOO"' vs 'echo '\''$FOO'\'''
        let marked = format!("{}$FOO", MARK); // marked $ = literal
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "bar".to_string());
        let result = strip_marks(&expand_vars(&marked, &mut vars, &[], 0));
        assert_eq!(result, "$FOO");
    }

    #[test]
    fn test_arithmetic_basic() {
        let vars = HashMap::new();
        assert_eq!(eval_arithmetic("(1+2)", &vars, &[]), 3);
        assert_eq!(eval_arithmetic("(10-4)", &vars, &[]), 6);
        assert_eq!(eval_arithmetic("(6*7)", &vars, &[]), 42);
        assert_eq!(eval_arithmetic("(10/3)", &vars, &[]), 3);
        assert_eq!(eval_arithmetic("(10%3)", &vars, &[]), 1);
    }

    #[test]
    fn test_arithmetic_precedence_and_parens() {
        let vars = HashMap::new();
        assert_eq!(eval_arithmetic("((2+3)*4)", &vars, &[]), 20);
        assert_eq!(eval_arithmetic("(((1+1)*(2+2)))", &vars, &[]), 8);
        assert_eq!(eval_arithmetic("(-5+2)", &vars, &[]), -3);
    }

    #[test]
    fn test_arithmetic_variables() {
        let mut vars = HashMap::new();
        vars.insert("n".to_string(), "2".to_string());
        assert_eq!(eval_arithmetic("(n*5)", &vars, &[]), 10);
    }

    #[test]
    fn test_arithmetic_expansion_inside_vars() {
        // `$((1+2))` expands to the integer result via expand_vars.
        let mut vars = HashMap::new();
        assert_eq!(expand_vars("r=$((1+2))", &mut vars, &[], 0), "r=3");
    }

    #[test]
    fn test_positional_params() {
        let mut vars = HashMap::new();
        let pos = ["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(expand_vars("$1 $2 $3", &mut vars, &pos, 0), "a b c");
        assert_eq!(expand_vars("$#", &mut vars, &pos, 0), "3");
        assert_eq!(expand_vars("$@", &mut vars, &pos, 0), "a b c");
        assert_eq!(expand_vars("$0", &mut vars, &pos, 0), "rsh");
        assert_eq!(expand_vars("${2}", &mut vars, &pos, 0), "b");
    }

    #[test]
    fn test_arithmetic_positional() {
        let positionals = ["3".to_string(), "4".to_string()];
        let mut vars = HashMap::new();
        // Inside $(( ... )), `$1`/`$2` resolve from positional parameters.
        assert_eq!(
            expand_vars("$(( $1 + $2 ))", &mut vars, &positionals, 0),
            "7"
        );
    }

    #[test]
    fn test_field_splitting_unmarked() {
        assert_eq!(split_fields("a b c"), vec!["a", "b", "c"]);
        assert_eq!(split_fields("  a  b  "), vec!["a", "b"]); // trimmed
    }

    #[test]
    fn test_field_splitting_protected() {
        // A MARK-ed space comes from quotes and must stay inside the field.
        let protected = format!("a{} b", MARK);
        assert_eq!(split_fields(&protected), vec!["a b"]);
    }

    #[test]
    fn test_field_splitting_dollar_at() {
        // Unquoted `$@` expands to one field per positional parameter.
        let mut vars = HashMap::new();
        let pos = ["a".to_string(), "b".to_string(), "c".to_string()];
        let mut subst = |_: &str| String::new();
        let fields = expand_args(&["$@".to_string()], &mut vars, &pos, 0, &mut subst);
        assert_eq!(fields, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_command_subst_quoted_paren_inner() {
        // A `)` inside double quotes is part of the substitution, not its end.
        let mut captured: Option<String> = None;
        let out = expand_command_subst("$(echo \"hi) there\")", &mut |inner: &str| -> String {
            captured = Some(inner.to_string());
            "OK".to_string()
        });
        assert_eq!(captured.as_deref(), Some("echo \"hi) there\""));
        assert_eq!(out, "OK");
    }

    #[test]
    fn test_assignment_value_not_field_split() {
        // An assignment's RHS is not field-split, even with whitespace.
        let mut vars = HashMap::new();
        let mut subst = |_: &str| -> String { "a b c".to_string() };
        let out = expand_args(&["v=$(x)".to_string()], &mut vars, &[], 0, &mut subst);
        assert_eq!(out, vec!["v=a b c"]);
    }

    #[test]
    fn test_command_arg_is_field_split() {
        // A normal argument IS field-split after an unquoted expansion.
        let mut vars = HashMap::new();
        let mut subst = |_: &str| -> String { "b c".to_string() };
        let out = expand_args(&["a $(x)".to_string()], &mut vars, &[], 0, &mut subst);
        assert_eq!(out, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_assignment_glob_not_expanded() {
        // Pathname expansion is not performed on an assignment's value.
        let mut vars = HashMap::new();
        let mut subst = |_: &str| -> String { String::new() };
        let out = expand_args(&["g=*err*".to_string()], &mut vars, &[], 0, &mut subst);
        assert_eq!(out, vec!["g=*err*"]);
    }

    #[test]
    fn test_is_assignment_word() {
        assert!(is_assignment_word("VAR=1"));
        assert!(is_assignment_word("_x=hello"));
        assert!(is_assignment_word("A="));
        assert!(!is_assignment_word("not assignment"));
        assert!(!is_assignment_word("1BAD=x"));
        assert!(!is_assignment_word("noequals"));
        assert!(!is_assignment_word("=value"));
    }
}
