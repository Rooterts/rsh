use crate::tokenizer::{RedirectKind, Token};

#[derive(Debug, Clone)]
pub struct Redirect {
    pub kind: RedirectKind,
    pub target: String,
}

#[derive(Debug, Clone)]
pub struct SimpleCommand {
    pub args: Vec<String>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub commands: Vec<SimpleCommand>,
    pub background: bool,
}

/// An executable "unit": either a pipeline of simple commands, or a compound
/// command (if/for/while/until/case). These cannot be mixed — a compound
/// command cannot be part of a pipe in this version (a simplification
/// documented in the README).
#[derive(Debug, Clone)]
pub enum Unit {
    Pipeline(Pipeline),
    Compound(CompoundCommand),
    Group(Vec<Job>), // `{ cmd1; cmd2; }` — runs its jobs in the current shell
}

#[derive(Debug, Clone)]
pub enum CompoundCommand {
    If(IfChain),
    For {
        var: String,
        words: Vec<String>,
        body: Vec<Job>,
    },
    While {
        cond: Vec<Job>,
        body: Vec<Job>,
        until: bool, // true = `until`, false = `while`
    },
    Case {
        word: String,
        arms: Vec<CaseArm>,
    },
    FunctionDef {
        name: String,
        body: Vec<Job>,
    },
}

#[derive(Debug, Clone)]
pub struct IfChain {
    /// Pairs (condition, body) for the `if` and each `elif`.
    pub branches: Vec<(Vec<Job>, Vec<Job>)>,
    pub else_body: Option<Vec<Job>>,
}

#[derive(Debug, Clone)]
pub struct CaseArm {
    /// Patterns separated by `|` within the same arm (e.g. `a|b) ...`).
    pub patterns: Vec<String>,
    pub body: Vec<Job>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Connector {
    And, // &&: runs only if the previous one succeeded
    Or,  // ||: runs only if the previous one failed
    Seq, // ;  or end of list: always runs
}

#[derive(Debug, Clone)]
pub struct Job {
    pub unit: Unit,
    pub connector: Connector, // connector towards the NEXT job
}

pub fn parse(tokens: Vec<Token>) -> Result<Vec<Job>, String> {
    let (jobs, i) = parse_command_list(&tokens, 0, &[])?;
    if i != tokens.len() {
        return Err(format!(
            "unexpected token at position {}: {:?}",
            i,
            tokens.get(i)
        ));
    }
    Ok(jobs)
}

/// Parses jobs until the end of the tokens or until finding a reserved word
/// from `terminators` (e.g. "fi", "done") or a `;;`.
fn parse_command_list(
    tokens: &[Token],
    mut i: usize,
    terminators: &[&str],
) -> Result<(Vec<Job>, usize), String> {
    let mut jobs = Vec::new();

    while matches!(tokens.get(i), Some(Token::Semicolon)) {
        i += 1;
    }

    while i < tokens.len() && !is_terminator(tokens, i, terminators) {
        let (unit, next_i) = parse_unit(tokens, i)?;
        i = next_i;

        let connector = match tokens.get(i) {
            Some(Token::And) => {
                i += 1;
                Connector::And
            }
            Some(Token::Or) => {
                i += 1;
                Connector::Or
            }
            Some(Token::Semicolon) => {
                i += 1;
                Connector::Seq
            }
            _ => Connector::Seq,
        };

        jobs.push(Job { unit, connector });

        while matches!(tokens.get(i), Some(Token::Semicolon)) {
            i += 1;
        }
    }

    Ok((jobs, i))
}

fn is_terminator(tokens: &[Token], i: usize, terminators: &[&str]) -> bool {
    match tokens.get(i) {
        Some(Token::CaseEnd) => true,
        Some(Token::RBrace) => true, // `{ ...; }` group or function body closing brace
        Some(Token::Word(w)) => terminators.contains(&w.as_str()),
        _ => false,
    }
}

fn expect_word(tokens: &[Token], i: usize, word: &str) -> Result<usize, String> {
    match tokens.get(i) {
        Some(Token::Word(w)) if w == word => Ok(i + 1),
        other => Err(format!("expected '{}', found {:?}", word, other)),
    }
}

fn parse_unit(tokens: &[Token], i: usize) -> Result<(Unit, usize), String> {
    if let Some(Token::Word(w)) = tokens.get(i) {
        match w.as_str() {
            "if" => {
                let (cmd, next) = parse_if(tokens, i + 1)?;
                return Ok((Unit::Compound(CompoundCommand::If(cmd)), next));
            }
            "for" => {
                let (cmd, next) = parse_for(tokens, i + 1)?;
                return Ok((Unit::Compound(cmd), next));
            }
            "while" => {
                let (cmd, next) = parse_while(tokens, i + 1, false)?;
                return Ok((Unit::Compound(cmd), next));
            }
            "until" => {
                let (cmd, next) = parse_while(tokens, i + 1, true)?;
                return Ok((Unit::Compound(cmd), next));
            }
            "case" => {
                let (cmd, next) = parse_case(tokens, i + 1)?;
                return Ok((Unit::Compound(cmd), next));
            }
            _ => {
                // A `name ()` / `name()` sequence starts a function definition.
                if matches!(tokens.get(i + 1), Some(Token::LParen)) {
                    return parse_function(tokens, i);
                }
            }
        }
    }

    // A `{ ...; }` group command.
    if matches!(tokens.get(i), Some(Token::LBrace)) {
        let (jobs, next) = parse_group(tokens, i)?;
        return Ok((Unit::Group(jobs), next));
    }

    let (pipeline, next) = parse_pipeline(tokens, i)?;
    Ok((Unit::Pipeline(pipeline), next))
}

/// Parses `name () { ...; }` (or `name () command`). The body is stored for
/// later invocation by the executor; definitions do not run anything.
fn parse_function(tokens: &[Token], i: usize) -> Result<(Unit, usize), String> {
    let name = match tokens.get(i) {
        Some(Token::Word(w)) => w.clone(),
        _ => return Err("expected a function name".to_string()),
    };
    let mut i = i + 1;

    if !matches!(tokens.get(i), Some(Token::LParen)) {
        return Err(format!("expected '(' in function definition of '{}'", name));
    }
    i += 1;
    if !matches!(tokens.get(i), Some(Token::RParen)) {
        return Err(format!("expected ')' in function definition of '{}'", name));
    }
    i += 1;

    let body = if matches!(tokens.get(i), Some(Token::LBrace)) {
        // `name () { job; job; }`
        let (jobs, next) = parse_group(tokens, i)?;
        i = next;
        jobs
    } else {
        // `name () single_command`
        let (unit, next) = parse_unit(tokens, i)?;
        i = next;
        vec![Job {
            unit,
            connector: Connector::Seq,
        }]
    };

    Ok((
        Unit::Compound(CompoundCommand::FunctionDef { name, body }),
        i,
    ))
}

/// Parses `{ job; job; ... }` and returns the inner jobs plus the index just
/// after the closing `}`.
fn parse_group(tokens: &[Token], i: usize) -> Result<(Vec<Job>, usize), String> {
    if !matches!(tokens.get(i), Some(Token::LBrace)) {
        return Err("expected '{'".to_string());
    }
    let (jobs, next) = parse_command_list(tokens, i + 1, &[])?;
    if !matches!(tokens.get(next), Some(Token::RBrace)) {
        return Err("expected '}' to close the group".to_string());
    }
    Ok((jobs, next + 1))
}

fn parse_if(tokens: &[Token], mut i: usize) -> Result<(IfChain, usize), String> {
    let mut branches = Vec::new();

    loop {
        let (cond, next) = parse_command_list(tokens, i, &["then"])?;
        i = expect_word(tokens, next, "then")?;
        let (body, next) = parse_command_list(tokens, i, &["elif", "else", "fi"])?;
        i = next;
        branches.push((cond, body));

        match tokens.get(i) {
            Some(Token::Word(w)) if w == "elif" => {
                i += 1;
                continue;
            }
            _ => break,
        }
    }

    let else_body = match tokens.get(i) {
        Some(Token::Word(w)) if w == "else" => {
            let (body, next) = parse_command_list(tokens, i + 1, &["fi"])?;
            i = next;
            Some(body)
        }
        _ => None,
    };

    i = expect_word(tokens, i, "fi")?;

    Ok((
        IfChain {
            branches,
            else_body,
        },
        i,
    ))
}

fn parse_for(tokens: &[Token], mut i: usize) -> Result<(CompoundCommand, usize), String> {
    let var = match tokens.get(i) {
        Some(Token::Word(w)) => w.clone(),
        other => {
            return Err(format!(
                "expected a variable name after 'for', found {:?}",
                other
            ));
        }
    };
    i += 1;

    i = expect_word(tokens, i, "in")?;

    let mut words = Vec::new();
    while let Some(Token::Word(w)) = tokens.get(i) {
        if w == "do" {
            break;
        }
        words.push(w.clone());
        i += 1;
    }

    while matches!(tokens.get(i), Some(Token::Semicolon)) {
        i += 1;
    }

    i = expect_word(tokens, i, "do")?;
    let (body, next) = parse_command_list(tokens, i, &["done"])?;
    i = expect_word(tokens, next, "done")?;

    Ok((CompoundCommand::For { var, words, body }, i))
}

fn parse_while(
    tokens: &[Token],
    i: usize,
    until: bool,
) -> Result<(CompoundCommand, usize), String> {
    let (cond, next) = parse_command_list(tokens, i, &["do"])?;
    let i = expect_word(tokens, next, "do")?;
    let (body, next) = parse_command_list(tokens, i, &["done"])?;
    let i = expect_word(tokens, next, "done")?;

    Ok((CompoundCommand::While { cond, body, until }, i))
}

fn parse_case(tokens: &[Token], mut i: usize) -> Result<(CompoundCommand, usize), String> {
    let word = match tokens.get(i) {
        Some(Token::Word(w)) => w.clone(),
        other => return Err(format!("expected a word after 'case', found {:?}", other)),
    };
    i += 1;
    i = expect_word(tokens, i, "in")?;

    while matches!(tokens.get(i), Some(Token::Semicolon)) {
        i += 1;
    }

    let mut arms = Vec::new();

    while !matches!(tokens.get(i), Some(Token::Word(w)) if w == "esac") {
        if i >= tokens.len() {
            return Err("expected 'esac' to close the 'case'".to_string());
        }

        let mut patterns = Vec::new();
        loop {
            match tokens.get(i) {
                Some(Token::Word(w)) => {
                    patterns.push(w.clone());
                    i += 1;
                }
                other => return Err(format!("expected a pattern in 'case', found {:?}", other)),
            }
            match tokens.get(i) {
                Some(Token::Pipe) => {
                    i += 1;
                    continue;
                }
                Some(Token::RParen) => {
                    i += 1;
                    break;
                }
                other => return Err(format!("expected '|' or ')' in 'case', found {:?}", other)),
            }
        }

        let (body, next) = parse_command_list(tokens, i, &["esac"])?;
        i = next;

        if matches!(tokens.get(i), Some(Token::CaseEnd)) {
            i += 1;
        }

        arms.push(CaseArm { patterns, body });
    }

    i = expect_word(tokens, i, "esac")?;

    Ok((CompoundCommand::Case { word, arms }, i))
}

/// Parses a pipeline (commands joined by |) up to &&, ||, ; or the end.
fn parse_pipeline(tokens: &[Token], mut i: usize) -> Result<(Pipeline, usize), String> {
    let mut commands = Vec::new();
    let mut background = false;

    loop {
        let (cmd, next_i) = parse_simple_command(tokens, i)?;
        commands.push(cmd);
        i = next_i;

        match tokens.get(i) {
            Some(Token::Pipe) => {
                i += 1;
                continue;
            }
            Some(Token::Background) => {
                background = true;
                i += 1;
                break;
            }
            _ => break,
        }
    }

    if commands.is_empty() {
        return Err("expected a command".to_string());
    }

    Ok((
        Pipeline {
            commands,
            background,
        },
        i,
    ))
}

/// Parses a simple command: words and redirections, up to | && || ; & ) ;; or the end.
fn parse_simple_command(tokens: &[Token], mut i: usize) -> Result<(SimpleCommand, usize), String> {
    let mut args = Vec::new();
    let mut redirects = Vec::new();

    while let Some(tok) = tokens.get(i) {
        match tok {
            Token::Word(w) => {
                args.push(w.clone());
                i += 1;
            }
            Token::Redirect(kind, target) => {
                redirects.push(Redirect {
                    kind: *kind,
                    target: target.clone(),
                });
                i += 1;
            }
            Token::Pipe
            | Token::And
            | Token::Or
            | Token::Semicolon
            | Token::CaseEnd
            | Token::LParen
            | Token::RParen
            | Token::LBrace
            | Token::RBrace
            | Token::Background => break,
        }
    }

    if args.is_empty() && redirects.is_empty() {
        return Err("expected a command".to_string());
    }

    Ok((SimpleCommand { args, redirects }, i))
}
