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
  main.rs       -> main loop (rustyline: history + line editing)
  tokenizer.rs  -> raw text -> tokens (quotes, escapes, comments)
  parser.rs     -> tokens -> Jobs (pipelines joined by && || ;)
  expand.rs     -> expansion of $VAR, ${VAR}, $?, ~, and globs (*.txt)
  builtins.rs   -> cd, pwd, exit, export, unset, echo, alias, unalias, which/type
  executor.rs   -> runs pipelines, applies redirects, honors &&/||/;/&
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
- `~` and glob expansion (`*.txt`, `?`, `[...]`).
- Aliases: `alias ll='ls -la'`, `unalias`.
- `which` / `type` to tell whether a command is a builtin or an external binary.
- **Control structures**: `if/then/elif/else/fi`,
  `for VAR in ...; do ... done`, `while ... do ... done`,
  `until ... do ... done`, `case ... in pattern) ... ;; esac` (`case` patterns
  support `*`, `?`, `[...]` just like a glob).
- Ctrl+C cancels the current line without closing the shell; Ctrl+D closes it
  (like bash).

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
```

> Note: `[ ... ]` is not implemented as a builtin yet — it is usually the
> external `/usr/bin/[` or `/usr/bin/test`, so on Linux it generally works as
> an external binary. If your system lacks it, `if`/`while` will fail with
> "command not found" until we add `test`/`[` as a builtin.

## Known limitations (documented on purpose)

- **`test`/`[` is not a builtin yet** — relies on the system binary (normally
  present on Linux/macOS). A good next step.
- **No real field splitting**: in bash, the unquoted result of `$VAR` or
  `$(cmd)` is split into multiple words according to `IFS`. Here each `$VAR`
  stays a single word (a pipeline `$(cmd)` can still yield several arguments
  via glob). This is the largest remaining gap vs. real POSIX.
- **Compound commands cannot be piped** (e.g. `if ...; fi | cat`) nor run in
  the background with `&` — they are separate from simple pipelines.
- **No shell functions** nor positional parameters (`$1`, `$@`, `$#`).
- **No real arithmetic expansion** `$((1 + 2))` (needed for counter loops with
  `while`).
- **Builtins inside a pipe** (e.g. `export FOO=1 | cat`) are not supported —
  they only run standalone or at the end of `&&`/`;`. Putting them mid-pipe
  would require manual `fork()` instead of `std::process::Command`.
- The **`$(...)` parenthesis balance is naive**: it does not distinguish
  parentheses that appear inside quotes nested within the `$(...)`.
- Background jobs have **no job table** (`jobs`, `fg`, `bg`) nor
  completion notifications.

## Suggested next steps (a useful roadmap)

1. `test` / `[` as a builtin (or confirm the system one is enough).
2. Arithmetic expansion `$((...))` — makes `while` loops much more useful.
3. Shell functions and positional parameters (`$1`, `$@`, `$#`, `$0`).
4. Real field splitting by `IFS`.
5. A real job table (`jobs`, `fg %1`, `bg %1`).
6. Path/command completion with a `rustyline::Helper`.
7. Finer signal handling (make Ctrl+Z suspend the child process).

## License

[MIT](./LICENSE).