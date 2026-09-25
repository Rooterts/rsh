# rsh — a shell written in Rust (Operating Systems course project)

`rsh` is a small UNIX-like shell that implements a practical subset of POSIX
shell behavior: quoting, variables, command substitution, globbing,
redirections, pipelines, control flow, aliases and a persistent command
history. It is a teaching project, so the code is intentionally small, readable
and heavily commented.

> Documentation is also available in [Spanish](./README.es.md).

## Build and run

```bash
cargo build --release
./target/release/rsh
```

Or directly with:

```bash
cargo run
```

Run the tokenizer/expander test suite:

```bash
cargo test
```

## Project structure

```
src/
  main.rs       -> main loop (rustyline): prompt + tab-completer helper
  tokenizer.rs  -> raw text -> tokens (quotes, escapes, comments)
  parser.rs     -> tokens -> Jobs (pipelines joined by && || ;, groups, functions)
  expand.rs     -> expansion of $VAR, ${VAR}, $?, $(...), $((...)), ~, globs, IFS split
  builtins.rs   -> cd, pwd, exit, export, unset, echo, alias, unalias, which/type,
                   test/[, jobs, fg, bg — plus the shell state (vars, functions,
                   positional params, job table)
  executor.rs   -> runs pipelines, applies redirects, honors &&/||/;/&, job control
  jobctl.rs     -> process groups, terminal control and signals (^C/^Z), waitpid
```

## Implemented features

- Prompt showing the current directory (with `~` instead of the home path) and
  a `✗` marker when the last command failed.
- Persistent history in `~/.rsh_history`, navigable with the arrow keys (via
  `rustyline`).
- **POSIX-style quotes**: single quotes are 100% literal (neither `$VAR` nor
  `$(...)` nor globs are touched inside); double quotes expand `$VAR` and
  `$(...)` but do NOT perform pathname or tilde expansion. Backslash escapes
  work outside quotes.
- Comments with `#`.
- Pipelines: `cmd1 | cmd2 | cmd3`.
- Redirections: `>`, `>>`, `<`, `2>`.
- Connectors: `&&`, `||`, `;`.
- Background with `&` (does not wait; prints the PID).
- **Command substitution**: `$(cmd)` and backticks (rewritten to `$(...)`
  internally). The command runs with its real stdout redirected to a
  temporary file, so it works with external binaries and not only builtins.
  It simulates a subshell: `export X=1` inside does not leak into the parent.
- Variables: `VAR=value` (plain assignment), `export VAR=value`, `$VAR`,
  `${VAR}`, `unset`.
- **Extended parameter expansion**: `${VAR:-default}`, `${VAR:=default}`
  (also assigns), `${VAR:?message}` (error if unset), `${VAR:+alt}`,
  `${#VAR}` (length).
- Exit code accessible as `$?` (works both inside and outside double quotes).
- **Arithmetic expansion**: `$(( 1 + 2 * 3 ))` with `+ - * / %`, parentheses,
  unary signs and variable references (`$n`, `n`, `$1`...). It is the natural
  building block for counter loops.
- **Field splitting**: the unquoted result of `$VAR`, `$(cmd)`, `$@` and
  arithmetic is split into separate words on whitespace (IFS behavior).
  Quoted whitespace is preserved inside a single word.
- `~` and glob expansion (`*.txt`, `?`, `[...]`).
- Aliases: `alias ll='ls -la'`, `unalias`.
- `which` / `type` to tell whether a command is a builtin or an external binary.
- **Shell functions and positional parameters**: `name() { ... ; }` with `$1`,
  `$2`, ..., `$0`, `$#`, `$@`. Functions are callable recursively and are also
  available inside `$(...)`.
- **`test` / `[` as a builtin**: unary (`-z`, `-n`, `-f`, `-d`, `-r`, `-w`, `-x`),
  binary (`=`, `!=`, `-eq`, `-ne`, `-lt`, `-le`, `-gt`, `-ge`) and `!` negation.
- **Control structures**: `if/then/elif/else/fi`,
  `for VAR in ...; do ... done`, `while ... do ... done`,
  `until ... do ... done`, `case ... in pattern) ... ;; esac` (`case` patterns
  support `*`, `?`, `[...]` just like a glob). `{ ...; }` command groups.
- **Job control**: background jobs with `&` are tracked in a job table;
  `jobs` lists them, `fg %N` brings one to the foreground and `bg %N`
  resumes a stopped job in the background. `wait` blocks until jobs finish,
  `disown` removes them from the table and `kill [-s sig]` sends signals.
  Job specifiers `%+`/`%%` (most recent), `%-` (second most recent), `%N`
  and a bare PID are accepted by `fg`/`bg`/`wait`/`disown`/`kill`.
  Finished background jobs are reaped automatically before every prompt and,
  in interactive sessions, announced with a `[N]  Done  cmd` line.
- **`read` builtin**: `read [-r] name1 name2 ...` parses a line from stdin
  into variables (the last one receives the rest), so
  `while read x; do ...; done` works.
- **Builtins and functions everywhere in a pipe**: stages that resolve to a
  builtin or shell function run in a forked subshell connected to the pipe
  (`echo hi | wc -c`, `printf 'a\nb\n' | grep a`, `export X=1 | cat`), so they
  no longer need an external binary of the same name.
- **Tab completion** for command names and file paths (`rustyline::Helper`).
- **Finer signal handling**: every foreground child runs in its own process
  group and receives the terminal, so Ctrl+C interrupts only the command and
  Ctrl+Z suspends it (it becomes a job you can `fg`/`bg`). The shell itself
  survives both.
- Ctrl+C at the prompt cancels the current line without closing the shell;
  Ctrl+D closes it (like bash).

Working examples:

```sh
for f in *.rs; do
  echo "file: $f"
done

if [ -z "$FOO" ]; then
  echo "FOO is not set"
fi

GREET=hello
echo "greeting=$GREET"        # variables assigned without `export`

false
echo "exit code was $?"       # $? expands inside double quotes too

v=$(echo sub) ; echo "v=$v"   # command substitution into a variable

fact() { if [ "$1" -le 1 ]; then echo 1;
         else r=$(fact $(($1 - 1))); echo $(($1 * r)); fi }
echo "fact 5 = $(fact 5)"     # shell functions + recursion + arithmetic

sleep 5 &
jobs            # [ 1] Running  sleep 5
fg %1           # bring it back to the foreground
```

## Known limitations (documented on purpose)

- **Compound commands cannot be piped** (e.g. `if ...; fi | cat`) nor run in
  the background with `&` — they are separate from simple pipelines.
- **Pipe lines have no per-stage job control**: a single command (foreground)
  is the only construct with its own process group and ^C/^Z handling. A
  foreground pipeline, and its intermediate stages, run in the shell's group.

## Suggested next steps (a useful roadmap)

1. Multi-stage pipeline job control (each stage in its own process group).
2. Compound commands (`if`/`for`/`while`/functions) piped or backgrounded.
3. Here-documents (`<<`).

## License

[MIT](./LICENSE).