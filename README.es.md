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
  main.rs       -> loop principal (rustyline: historial + edición de línea)
  tokenizer.rs  -> texto crudo -> tokens (comillas, escapes, comentarios)
  parser.rs     -> tokens -> Jobs (pipelines conectados por && || ;)
  expand.rs     -> expansión de $VAR, ${VAR}, $?, ~, y globs (*.txt)
  builtins.rs   -> cd, pwd, exit, export, unset, echo, alias, unalias, which/type
  executor.rs   -> ejecuta pipelines, aplica redirects, respeta &&/||/;/&
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
- Expansión de `~` y de globs (`*.txt`, `?`, `[...]`).
- Alias: `alias ll='ls -la'`, `unalias`.
- `which` / `type` para saber si un comando es builtin o binario externo.
- **Estructuras de control**: `if/then/elif/else/fi`,
  `for VAR in ...; do ... done`, `while ... do ... done`,
  `until ... do ... done`, `case ... in patrón) ... ;; esac` (los patrones de
  `case` soportan `*`, `?`, `[...]` igual que un glob).
- Ctrl+C cancela la línea actual sin cerrar la shell; Ctrl+D la cierra (igual
  que bash).

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
```

> Nota: `[ ... ]` todavía no es un builtin — normalmente es el binario externo
> `/usr/bin/[` o `/usr/bin/test`, así que en Linux suele funcionar tal cual.
> Si tu sistema no lo tiene, `if`/`while` van a fallar con "comando no
> encontrado" hasta que agreguemos `test`/`[` como builtin.

## Limitaciones conocidas (documentadas a propósito)

- **`test`/`[` no es builtin todavía** — depende de que exista como binario
  externo en el sistema (habitual en Linux/macOS). Es un buen próximo paso.
- **Sin field splitting real**: en bash, el resultado sin comillas de `$VAR`
  o `$(cmd)` se separa en varias palabras según `IFS`. Acá cada `$VAR` sigue
  siendo una sola palabra (aunque el `$(cmd)` de un pipeline sí puede generar
  varios argumentos vía glob). Es la brecha más grande que queda vs. POSIX.
- **Los comandos compuestos no pueden ir en un pipe** (ej. `if ...; fi | cat`)
  ni backgroundearse con `&` — son unidades aparte de las pipelines simples.
- **Sin funciones de shell** ni parámetros posicionales (`$1`, `$@`, `$#`).
- **Sin expansión aritmética** `$((1 + 2))` real (se necesita para loops
  tipo contador con `while`).
- **Los builtins dentro de un pipe** (ej. `export FOO=1 | cat`) no están
  soportados — solo corren "solos" o al final de `&&`/`;`. Meterlos en medio
  de un pipe real requeriría `fork()` manual en vez de
  `std::process::Command`.
- El **balanceo de paréntesis de `$(...)` es ingenuo**: no distingue paréntesis
  que aparecen dentro de comillas anidadas en el propio `$(...)`.
- Los jobs en background **no tienen tabla de jobs** (`jobs`, `fg`, `bg`) ni
  notificación al terminar.

## Próximos pasos sugeridos (roadmap)

1. `test` / `[` como builtin (o confirmar que el del sistema alcanza).
2. Expansión aritmética `$((...))` — hace mucho más útiles los `while`.
3. Funciones de shell y parámetros posicionales (`$1`, `$@`, `$#`, `$0`).
4. Field splitting real por `IFS`.
5. Tabla de jobs real (`jobs`, `fg %1`, `bg %1`).
6. Autocompletado de rutas/comandos con `rustyline::Helper`.
7. Manejo de señales más fino (que `Ctrl+Z` suspenda el proceso hijo).

## Licencia

[MIT](./LICENSE).