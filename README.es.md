# rsh — una shell en Rust (proyecto de Sistemas Operativos)

> Secondary Spanish documentation. See [README.md](./README.md) for the
> primary (English) documentation.

`rsh` es una shell pequeña tipo UNIX que implementa un subconjunto práctico del
comportamiento de una shell POSIX: comillas, variables, sustitución de
comandos, globs, redirecciones, pipelines, control de flujo, alias e historial
persistente. Es un proyecto de estudio, por lo que el código es intencionalmente
pequeño, legible y con comentarios.

## Compilar y correr

```bash
cargo build --release
./target/release/rsh
```

O directo:

```bash
cargo run
```

Correr los tests del tokenizer/expansor:

```bash
cargo test
```

## Estructura del proyecto

```
src/
  main.rs       -> loop principal (rustyline): prompt + autocompletado con Tab
  tokenizer.rs  -> texto crudo -> tokens (comillas, escapes, comentarios)
  parser.rs     -> tokens -> Jobs (pipelines unidos por && || ; , grupos, funciones)
  expand.rs     -> expansión de $VAR, ${VAR}, $?, $(...), $((...)), ~, globs, split por IFS
  builtins.rs   -> cd, pwd, exit, export, unset, echo, alias, unalias, which/type,
                   test/[, jobs, fg, bg — más el estado de la shell (vars, funciones,
                   params posicionales, tabla de jobs)
  executor.rs   -> corre pipelines, aplica redirects, respeta &&/||/;/&, job control
  jobctl.rs     -> grupos de procesos, control de la terminal y señales (^C/^Z), waitpid
```

## Features implementadas

- Prompt con el directorio actual (con `~` en vez del home) y un marcador `✗`
  si el último comando falló.
- Historial persistente en `~/.rsh_history`, navegable con las flechas (vía
  `rustyline`).
- **Comillas al estilo POSIX**: las comillas simples son 100% literales (ni
  `$VAR` ni `$(...)` ni globs se tocan adentro); las dobles expanden `$VAR` y
  `$(...)` pero NO hacen pathname expansion ni tilde expansion. Los escapes con
  `\` funcionan fuera de comillas.
- Comentarios con `#`.
- Pipelines: `cmd1 | cmd2 | cmd3`.
- Redirecciones: `>`, `>>`, `<`, `2>`.
- Conectores: `&&`, `||`, `;`.
- Background con `&` (no espera; imprime el PID).
- **Sustitución de comandos**: `$(cmd)` y backticks (reescritos a `$(...)`
  internamente). El comando corre con su stdout real redirigido a un archivo
  temporal, así que funciona con binarios externos y no solo con builtins.
  Simula un subshell: `export X=1` adentro no contamina al padre.
- Variables: `VAR=value` (asignación simple), `export VAR=value`, `$VAR`,
  `${VAR}`, `unset`.
- **Expansión de parámetros extendida**: `${VAR:-default}`, `${VAR:=default}`
  (además asigna), `${VAR:?mensaje}` (error si no está seteada), `${VAR:+alt}`,
  `${#VAR}` (longitud).
- Código de salida como `$?` (funciona dentro y fuera de comillas dobles).
- **Expansión aritmética**: `$(( 1 + 2 * 3 ))` con `+ - * / %`, paréntesis,
  signo unario y referencias a variables (`$n`, `n`, `$1`...). Es el bloque
  natural para loops tipo contador.
- **Field splitting**: el resultado sin comillas de `$VAR`, `$(cmd)`, `$@` y la
  aritmética se parte en varias palabras según el espacio (comportamiento IFS).
  El espacio que viene entre comillas se conserva dentro de una misma palabra.
- Expansión de `~` y de globs (`*.txt`, `?`, `[...]`).
- Alias: `alias ll='ls -la'`, `unalias`.
- `which` / `type` para saber si un comando es builtin o binario externo.
- **Funciones de shell y parámetros posicionales**: `name() { ... ; }` con `$1`,
  `$2`, ..., `$0`, `$#`, `$@`. Las funciones se pueden llamar recursivamente y
  también están disponibles dentro de `$(...)`.
- **`test` / `[` como builtin**: unarios (`-z`, `-n`, `-f`, `-d`, `-r`, `-w`,
  `-x`), binarios (`=`, `!=`, `-eq`, `-ne`, `-lt`, `-le`, `-gt`, `-ge`) y
  negación con `!`.
- **Estructuras de control**: `if/then/elif/else/fi`,
  `for VAR in ...; do ... done`, `while ... do ... done`,
  `until ... do ... done`, `case ... in patrón) ... ;; esac` (los patrones de
  `case` soportan `*`, `?`, `[...]` igual que un glob). Grupos con `{ ...; }`.
- **Job control**: los jobs en background con `&` se rastrean en una tabla;
  `jobs` los lista, `fg %N` trae uno a primer plano y `bg %N` reanuda uno
  detenido en background. `wait` bloquea hasta que terminan, `disown` los quita
  de la tabla y `kill [-s sig]` envía señales. Los especificadores de job
  `%+`/`%%` (más reciente), `%-` (segundo más reciente), `%N` y un PID a secas
  funcionan en `fg`/`bg`/`wait`/`disown`/`kill`. Los jobs terminados se
  recolectan automáticamente antes de cada prompt y, en sesiones interactivas,
  se anuncian con `[N]  Done  cmd`.
- **Builtin `read`**: `read [-r] nombre1 nombre2 ...` lee una línea del stdin y
  la reparte en variables (la última se queda con el resto), así que funciona
  `while read x; do ...; done`.
- **Builtins y funciones en cualquier punto de un pipe**: una etapa que sea
  builtin o función corre en un subshell forkeado conectado al pipe
  (`echo hi | wc -c`, `printf 'a\nb\n' | grep a`, `export X=1 | cat`), así ya
  no hace falta que exista un binario externo del mismo nombre.
- **Job control de toda la pipeline**: todas las etapas corren en un mismo
  grupo de procesos que posee la terminal mientras corre, así ^C interrumpe y
  ^Z detiene la pipeline *entera*; pasa a ser un único job para `fg`/`bg`.
- **Autocompletado con Tab** de nombres de comandos y rutas de archivos
  (`rustyline::Helper`).
- **Manejo de señales fino**: cada comando en primer plano corre en su propio
  grupo de procesos y recibe la terminal, así Ctrl+C interrumpe solo el comando
  y Ctrl+Z lo suspende (pasa a ser un job que puedes `fg`/`bg`). La shell
  sobrevive a ambos.
- Ctrl+C en el prompt cancela la línea sin cerrar la shell; Ctrl+D la cierra
  (igual que bash).

Ejemplos que ya funcionan:

```sh
for f in *.rs; do
  echo "file: $f"
done

if [ -z "$FOO" ]; then
  echo "FOO no está seteada"
fi

GREET=hola
echo "saludo=$GREET"          # variables asignadas sin `export`

false
echo "el código fue $?"       # $? se expande también en comillas dobles

v=$(echo sub) ; echo "v=$v"   # sustitución de comandos en una variable

fact() { if [ "$1" -le 1 ]; then echo 1;
         else r=$(fact $(($1 - 1))); echo $(($1 * r)); fi }
echo "fact 5 = $(fact 5)"     # funciones + recursión + aritmética

sleep 5 &
jobs            # [ 1] Running  sleep 5
fg %1           # lo vuelve a primer plano
```

## Limitaciones conocidas (documentadas a propósito)

- **Los comandos compuestos no pueden ir en un pipe** (ej. `if ...; fi | cat`)
  ni backgroundearse con `&` — son unidades aparte de las pipelines simples.

## Próximos pasos sugeridos (roadmap)

1. Comandos compuestos (`if`/`for`/`while`/funciones) en pipe o background.
2. Here-documents (`<<`).

## Licencia

[MIT](./LICENSE).