#[derive(Debug, PartialEq, Clone)]
pub enum Token {
    Word(String),
    Pipe,                           // |
    Redirect(RedirectKind, String), // >, >>, <, 2>
    And,                            // &&
    Or,                             // ||
    Semicolon,                      // ;
    CaseEnd,                        // ;;
    RParen,                         // ) (closes a `case` pattern)
    Background,                     // &
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum RedirectKind {
    Out,    // >
    Append, // >>
    In,     // <
    ErrOut, // 2>
}

/// Non-printable control character used as an internal marker: MARK+c means
/// "c is literal, do not expand it nor treat it as a glob". This lets us
/// distinguish a `$` inside single quotes (never expanded) from a `$` outside
/// quotes (expanded), without needing a more complex data structure than a
/// String for each word.
pub const MARK: char = '\u{1}';

pub fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        if c == '#' {
            break; // comment: ignore the rest of the line
        }

        if c == '&' && chars.get(i + 1) == Some(&'&') {
            tokens.push(Token::And);
            i += 2;
            continue;
        }
        if c == '|' && chars.get(i + 1) == Some(&'|') {
            tokens.push(Token::Or);
            i += 2;
            continue;
        }
        if c == ';' && chars.get(i + 1) == Some(&';') {
            tokens.push(Token::CaseEnd);
            i += 2;
            continue;
        }
        if c == '>' && chars.get(i + 1) == Some(&'>') {
            i += 2;
            let target = read_word(&chars, &mut i)?;
            tokens.push(Token::Redirect(RedirectKind::Append, target));
            continue;
        }
        if c == '2' && chars.get(i + 1) == Some(&'>') {
            i += 2;
            let target = read_word(&chars, &mut i)?;
            tokens.push(Token::Redirect(RedirectKind::ErrOut, target));
            continue;
        }

        match c {
            '|' => {
                tokens.push(Token::Pipe);
                i += 1;
                continue;
            }
            ';' => {
                tokens.push(Token::Semicolon);
                i += 1;
                continue;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
                continue;
            }
            '&' => {
                tokens.push(Token::Background);
                i += 1;
                continue;
            }
            '>' => {
                i += 1;
                let target = read_word(&chars, &mut i)?;
                tokens.push(Token::Redirect(RedirectKind::Out, target));
                continue;
            }
            '<' => {
                i += 1;
                let target = read_word(&chars, &mut i)?;
                tokens.push(Token::Redirect(RedirectKind::In, target));
                continue;
            }
            _ => {}
        }

        let word = read_quoted_word(&chars, &mut i)?;
        tokens.push(Token::Word(word));
    }

    Ok(tokens)
}

fn read_word(chars: &[char], i: &mut usize) -> Result<String, String> {
    while *i < chars.len() && chars[*i].is_whitespace() {
        *i += 1;
    }
    read_quoted_word(chars, i)
}

/// Reads a word respecting single/double quotes, backslash escapes, and
/// command substitution `$(...)` / backticks. The result may contain MARK
/// characters indicating "this is literal, do not expand it" — interpreted
/// later by expand.rs. Stops before an unquoted ')' or ';' so `case` can
/// recognize the end of a pattern.
pub fn read_quoted_word(chars: &[char], i: &mut usize) -> Result<String, String> {
    let mut word = String::new();

    while *i < chars.len() {
        let c = chars[*i];

        if c.is_whitespace() || "|&;<>#)".contains(c) {
            break;
        }

        match c {
            '\'' => {
                // Single quotes: EVERYTHING is literal, even $ and globs.
                *i += 1;
                while *i < chars.len() && chars[*i] != '\'' {
                    push_literal(&mut word, chars[*i]);
                    *i += 1;
                }
                if *i >= chars.len() {
                    return Err("unclosed single quote".to_string());
                }
                *i += 1;
            }
            '"' => {
                // Double quotes: $ DOES expand (parameters / commands),
                // but * ? [ ~ must not be interpreted as glob nor tilde.
                *i += 1;
                while *i < chars.len() && chars[*i] != '"' {
                    if chars[*i] == '\\'
                        && *i + 1 < chars.len()
                        && (chars[*i + 1] == '"' || chars[*i + 1] == '\\' || chars[*i + 1] == '$')
                    {
                        push_literal(&mut word, chars[*i + 1]);
                        *i += 2;
                    } else {
                        push_double_quoted(&mut word, chars[*i]);
                        *i += 1;
                    }
                }
                if *i >= chars.len() {
                    return Err("unclosed double quote".to_string());
                }
                *i += 1;
            }
            '`' => {
                // Classic backtick: rewritten as $(...) so the expander only
                // has to understand one syntax.
                *i += 1;
                let mut inner = String::new();
                while *i < chars.len() && chars[*i] != '`' {
                    if chars[*i] == '\\' && *i + 1 < chars.len() {
                        inner.push(chars[*i + 1]);
                        *i += 2;
                    } else {
                        inner.push(chars[*i]);
                        *i += 1;
                    }
                }
                if *i >= chars.len() {
                    return Err("unclosed backtick (`)".to_string());
                }
                *i += 1;
                word.push('$');
                word.push('(');
                word.push_str(&inner);
                word.push(')');
            }
            '$' if chars.get(*i + 1) == Some(&'(') => {
                // Unquoted $(...): read the whole block honoring nested
                // parentheses, so inner spaces do not cut the word short
                // (its content is re-tokenized recursively when executed).
                let start = *i;
                *i += 2;
                let mut depth = 1;
                while *i < chars.len() && depth > 0 {
                    match chars[*i] {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    *i += 1;
                    if depth == 0 {
                        break;
                    }
                }
                if depth != 0 {
                    return Err("unclosed $(...)".to_string());
                }
                let chunk: String = chars[start..*i].iter().collect();
                word.push_str(&chunk);
            }
            '\\' => {
                // Escape outside quotes: the following char is literal.
                *i += 1;
                if *i < chars.len() {
                    push_literal(&mut word, chars[*i]);
                    *i += 1;
                }
            }
            _ => {
                word.push(c);
                *i += 1;
            }
        }
    }

    Ok(word)
}

/// Marks a character so it is NEVER expanded (neither $VAR, nor glob, nor ~).
/// Used for single quotes and for `\c` escapes outside quotes.
fn push_literal(word: &mut String, c: char) {
    if matches!(c, '$' | '*' | '?' | '[' | '~') {
        word.push(MARK);
    }
    word.push(c);
}

/// Marks * ? [ ~ but leaves $ unmarked: double quotes allow parameter and
/// command expansion but NOT pathname/tilde expansion.
fn push_double_quoted(word: &mut String, c: char) {
    if matches!(c, '*' | '?' | '[' | '~') {
        word.push(MARK);
    }
    word.push(c);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple() {
        let tokens = tokenize("ls -la").unwrap();
        assert_eq!(
            tokens,
            vec![Token::Word("ls".into()), Token::Word("-la".into())]
        );
    }

    #[test]
    fn test_quotes() {
        let tokens = tokenize(r#"echo "hola mundo" 'chau mundo'"#).unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into()),
                Token::Word("hola mundo".into()),
                Token::Word("chau mundo".into()),
            ]
        );
    }

    #[test]
    fn test_single_quotes_protect_dollar() {
        let tokens = tokenize("echo '$FOO'").unwrap();
        if let Token::Word(w) = &tokens[1] {
            assert!(w.contains(MARK), "expected a MARK before the literal $");
        } else {
            panic!("expected a Word");
        }
    }

    #[test]
    fn test_pipe() {
        let tokens = tokenize("ls | grep foo").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Word("ls".into()),
                Token::Pipe,
                Token::Word("grep".into()),
                Token::Word("foo".into()),
            ]
        );
    }

    #[test]
    fn test_redirect() {
        let tokens = tokenize("echo hola > out.txt").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into()),
                Token::Word("hola".into()),
                Token::Redirect(RedirectKind::Out, "out.txt".into()),
            ]
        );
    }

    #[test]
    fn test_and_or_seq_background() {
        let tokens = tokenize("make && ./run || echo fail ; sleep 5 &").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Word("make".into()),
                Token::And,
                Token::Word("./run".into()),
                Token::Or,
                Token::Word("echo".into()),
                Token::Word("fail".into()),
                Token::Semicolon,
                Token::Word("sleep".into()),
                Token::Word("5".into()),
                Token::Background,
            ]
        );
    }

    #[test]
    fn test_command_subst_with_spaces() {
        let tokens = tokenize("echo $(ls -la /tmp)").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into()),
                Token::Word("$(ls -la /tmp)".into())
            ]
        );
    }

    #[test]
    fn test_backtick_rewritten_as_dollar_paren() {
        let tokens = tokenize("echo `whoami`").unwrap();
        assert_eq!(
            tokens,
            vec![Token::Word("echo".into()), Token::Word("$(whoami)".into())]
        );
    }

    #[test]
    fn test_case_tokens() {
        let tokens = tokenize("case $x in a|b) ;; *) ;; esac").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Word("case".into()),
                Token::Word("$x".into()),
                Token::Word("in".into()),
                Token::Word("a".into()),
                Token::Pipe,
                Token::Word("b".into()),
                Token::RParen,
                Token::CaseEnd,
                Token::Word("*".into()),
                Token::RParen,
                Token::CaseEnd,
                Token::Word("esac".into()),
            ]
        );
    }
}
