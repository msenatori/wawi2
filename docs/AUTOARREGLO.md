# Autoarreglo

Cuando Wawi nota un problema, se arma un arreglo con Opus, lo prueba detrás de compuertas que no puede tocar y te lo **propone**. Nunca lo aplica sola. Aplicarlo es siempre decisión tuya.

El código está en `crates/wawi-autoarreglo`. El plugin está en `plugins/autoarreglo`.

## La idea en cinco frases

- Los problemas entran a un diario: los anota el programa de voz, los marcás vos o los encuentra la revisión de la noche.
- Un problema que se repite tiene un solo registro, con un contador.
- De noche, Wawi elige el problema que más te cuesta y le pide a Opus un arreglo, en una copia aparte del repo.
- El arreglo pasa por compuertas: archivos prohibidos, tamaño, dependencias, secretos, velocidad, las pruebas y una segunda opinión.
- Si pasa todo, te lo cuenta. Si decís que sí, queda un solo commit en tu rama.

## El dibujo

```
  programa de voz         vos                 revisión de la noche
  (incidentes)    (reportar_problema)        (registros del día)
        │                 │                           │
        └────────┬────────┴───────────────────────────┘
                 ▼
      <home>/cola/incidentes/  (tmp/ → new/ → cur/)
                 │
                 ▼
  ┌────────────────────────────────┐
  │ worker de autoarreglo          │
  │  · diario: un problema por     │
  │    tipo + huella, con contador │
  │  · elige el más útil           │
  └───────────────┬────────────────┘
                  │ de noche, con tope por día y presupuesto
                  ▼
  worktree aparte: <home>/autoarreglo/trabajo/<id>
  rama autoarreglo/<id>, desde HEAD
                  │
                  ▼
  Opus arregla (solo edita ahí y corre cargo)
                  │ el servicio hace el commit en la rama
                  ▼
  compuertas: prohibidos · tamaño · dependencias · secretos ·
              camino rápido · fmt · clippy · pruebas · velocidad ·
              segunda opinión (Opus, solo lectura)
                  │
         ┌────────┴────────┐
     falla una         pasan todas
         │                 │
  se anota y espera   <home>/autoarreglo/propuestas/<id>/
  una semana          + un aviso en cola/hechos
                           │
                           ▼
              Wawi: «Encontré por qué se me cortaba la voz
              y tengo un arreglo probado. ¿Te lo cuento?»
                           │ vos decís que sí
                           ▼
              un solo commit en tu rama (si tu árbol está limpio)
                           │
                           ▼
              vigía: si el primer arranque sale mal, te ofrece deshacerlo
```

`<home>` es `$WAWI2_HOME` o `~/.wawi2`.

## De dónde salen los problemas

### El programa de voz

El programa de voz deja un incidente cuando pasa algo de esto:

| Tipo | Cuándo |
| --- | --- |
| `turno_fallido` | el cerebro falló un turno, el turno llegó a su tope o se colgó y lo cortó el watchdog |
| `cerebro_caido` | el proceso de Claude Code se cerró en medio de un turno |
| `plugin_caido` | un plugin se cayó 3 veces en 10 minutos |
| `transcripcion_vacia` | 3 transcripciones seguidas vacías o sin entender, en 3 minutos (un toque corto no cuenta) |
| `turno_lento` | el primer audio tardó más del doble de la mediana de los últimos 20 turnos, y más de 1,5 s |
| `audio` | el reconocedor no contestó, la reproducción nunca avisó que terminó, o una frase no dio audio |

La velocidad es tu regla: un turno lento es un problema como cualquier otro.

Escribir un incidente nunca frena un turno. Las cuentas son puras y baratas, y los archivos los escribe un hilo aparte. El mismo incidente se escribe como mucho una vez por minuto. Si nadie los lee (el plugin no está instalado), se guardan hasta 500 y después no se escriben más. Nunca va lo que dijiste vos ni lo que dijo Wawi: solo el tipo, los tiempos y el motivo del error. Un motivo que parece un secreto no se guarda.

El archivo es así:

```json
{
  "id": "1763730604817-3",
  "version": 1,
  "tipo": "turno_lento",
  "clave": "cerebro",
  "resumen": "primer audio a los 4210 ms; la mediana es 1830 ms",
  "detalle": { "felt_ms": 4210, "mediana_ms": 1830 },
  "creado": "2026-11-21T10:50:04",
  "origen": "host"
}
```

`clave` es lo que hace que dos incidentes sean el mismo problema, junto con `tipo`. Cualquier proceso puede dejar uno: se escribe en `tmp/` y se renombra a `new/`.

### Vos

La herramienta `autoarreglo__reportar_problema(que_paso)` anota lo que le marcás: «eso está mal, reportalo», «no me entendiste», «tardaste un montón». Es `read`: solo escribe en la cola local y nunca toca código. Contesta «Anotado» o «Anotado, ya van 3 veces».

### La revisión de la noche

Una vez por día, después de las 4 (se cambia en `[revision]`), Wawi lee sus registros desde la revisión anterior:

1. **Barrido, sin modelo.** Junta las líneas con problemas de `logs/session.log` y `logs/plugins.log` por firma (sin hora, sin números, sin datos personales) y saca la latencia de los turnos de `logs/turns.log`. Nunca lee lo que se dijo.
2. **Juez, una llamada barata.** Si encontró algo, le pasa el resumen a un modelo barato (`claude-sonnet-5-5`, sin herramientas, salida estructurada). El modelo elige como mucho dos problemas. El código revisa que cada uno se refiera a un grupo de verdad y los anota como `revision`.

Si no hay nada raro, no llama a ningún modelo. Casi todas las noches no anota nada.

## El diario

Cada problema es un archivo: `<home>/autoarreglo/problemas/<tipo>-<huella>.json`. La huella sale del tipo y de la clave (en minúsculas, sin números). Un repetido suma uno al contador y guarda una muestra corta (hasta 5). Todo lo que se guarda pasa antes por el tapado de secretos: claves, tokens, mails, IPs de tu red y rutas de tu carpeta personal.

## Qué elige

- Puede intentar: lo que reportaste vos, el cerebro caído y lo de la revisión, siempre. Lo demás, cuando pasó al menos 2 veces.
- Nunca intenta: lo que descartaste, lo que ya tiene un arreglo esperando, ni lo que falló hace menos de una semana.
- Un arreglo aplicado cuenta como roto otra vez solo si el problema vuelve a pasar 2 veces más.
- Primero lo que más te cuesta: que se caiga, lo que reportaste, que tarde, que no te entienda, el audio, un turno fallido, un plugin, la revisión. Entre iguales, el que más pasó.

## El intento

1. **Antes de gastar.** Tiene que estar prendido, saber dónde está el repo, que el repo tenga al menos un commit, estar dentro de la ventana horaria (por defecto de 1 a 7), y no haber pasado el tope de intentos ni el presupuesto del día.
2. **La base tiene que estar verde.** En una copia limpia corre fmt, clippy y las pruebas. Si fallan, no gasta nada en Opus, te avisa («mis pruebas no pasan en el repo tal como está») y no vuelve a intentar hasta que haya un commit nuevo.
3. **El worktree.** `git worktree add` en `<home>/autoarreglo/trabajo/<id>`, fuera del repo, en una rama nueva `autoarreglo/<id>` desde HEAD.
4. **Opus.** Una corrida de `claude` sin cabeza, con `claude-opus-5-5`, encerrada en ese worktree. El brief es corto, está tapado y dice las reglas: qué no puede tocar, nada de dependencias nuevas, cuidar la velocidad, y primero una prueba que reproduzca el problema.
5. **El commit lo hace el servicio**, no el agente: lo que el agente dejó queda en la rama.
6. **Las compuertas**, una por una. La primera que falla corta todo.
7. **El final.** Si pasa, una propuesta y un aviso. Si no, el worktree y la rama se borran, y el motivo queda en el historial del problema y en el brief del próximo intento.

Todo corre con prioridad baja de CPU (`nice`), así una compilación de noche no frena un turno. Cargo corre sin red (`CARGO_NET_OFFLINE`) y compila en `<home>/autoarreglo/target`, aparte del tuyo.

### Los permisos de Opus

Lo que se verificó con `claude --help` (Claude Code 2.1):

```
claude -p --output-format json --model claude-opus-5-5
  --permission-mode acceptEdits --permission-prompts none
  --tools Read,Edit,Write,Grep,Glob,Bash
  --allowedTools "Bash(cargo *)" "Bash(git diff *)" "Bash(git diff)"
  --setting-sources= --strict-mcp-config --disable-slash-commands --no-chrome
  --no-session-persistence --max-budget-usd 4.00
  --json-schema <esquema> --append-system-prompt <reglas>
```

- Solo existen esas seis herramientas. Bash queda limitado a cargo y git diff.
- Leer y editar dentro del worktree no pide permiso. Cualquier otra cosa (un archivo de afuera, otro comando) pediría permiso, y con `--permission-prompts none` se niega sola.
- Sin tus ajustes, sin servidores MCP, sin sesión guardada.
- El brief va por stdin, nunca en la línea de comandos.
- Tiene un tope de plata (`--max-budget-usd`) y de tiempo (30 minutos). Al pasar el tiempo se mata el proceso con todo su grupo.

La segunda opinión usa la misma forma, con `--tools Read,Grep,Glob` y nada aprobado de antemano: solo puede leer.

Se probó una vez de verdad, en un repo de juguete con un error de suma: Opus encontró la causa, agregó la prueba, pasó todas las compuertas y la segunda opinión lo aprobó. Costó US$ 0,21 y tardó 36 segundos.

## Las compuertas

Todas tienen que pasar.

| Compuerta | Qué mira |
| --- | --- |
| prohibidos | que no toque ningún camino prohibido (ver abajo), que no saque reglas de `.gitignore` y que no cambie los permisos de un `plugin.toml` |
| tamaño | como mucho 400 líneas y 12 archivos, y nada binario |
| dependencias | `Cargo.lock` no cambia, y un `Cargo.toml` solo en comentarios |
| secretos | en las líneas agregadas: claves, tokens, mails, rutas personales, tu usuario, IPs de tu red |
| camino rápido | nada bloqueante nuevo en el camino del turno: sleep, recv sin tope, procesos, wait, join, red, ni archivos en el núcleo puro |
| fmt | `cargo fmt --all --check` |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` |
| pruebas | `cargo test --workspace` entero |
| velocidad | las pruebas de velocidad por nombre: `a_turn_never_waits_for_a_respawn` y las de punta a punta del programa de voz. Cada una tiene que pasar y correr al menos una prueba: borrarla o renombrarla también falla |
| revisión | una segunda corrida de Opus, solo lectura, que tiene que aprobar con un veredicto estructurado. Ante la duda, rechaza |

## Lo que nunca puede tocar

- Su propio código: `crates/wawi-autoarreglo/` y `plugins/autoarreglo/`.
- El detector de incidentes: `crates/wawi-app/src/voice/incidents.rs`.
- La política y los permisos: `crates/wawi-core/src/policy.rs`, `crates/wawi-core/src/registry.rs`, `crates/wawi-plugins/src/manifest.rs` y `crates/wawi-plugins/src/tool.rs`.
- Los secretos: `crates/wawi-plugins/src/secrets.rs`, `crates/wawi-memory/src/secrets.rs`, cualquier `secrets.toml` o `.env`.
- `LICENSE`, `.git/` y `.github/`.
- En un `plugin.toml`, las líneas `kind`, `confirm`, `observa_charla`, `expose`, `enabled` y `[tools...]`.

Las reglas están en `gates.rs`, que es uno de los caminos prohibidos: no se puede reescribir sus propias barandas.

## La propuesta

Cuando pasa todo, queda `<home>/autoarreglo/propuestas/<id>/`:

- `propuesta.md`: qué andaba mal, por qué pasaba, qué cambió, las pruebas, las compuertas y lo que costó. En castellano.
- `cambios.diff`: el diff entero.
- `propuesta.json`: el estado.

La rama `autoarreglo/<id>` queda en el repo hasta que la apliques o la descartes. Wawi deja un aviso en `cola/hechos`, y el portón del instinto decide cuándo decirlo (nunca de noche, nunca encima tuyo). Algo como: «Encontré por qué se me cortaba la voz y tengo un arreglo probado. ¿Te lo cuento?».

## Las herramientas

| Herramienta | Qué hace | Tipo |
| --- | --- | --- |
| `autoarreglo__reportar_problema(que_paso)` | anota lo que salió mal | read |
| `autoarreglo__arreglos()` | lista los arreglos: id, estado y qué arreglan | read |
| `autoarreglo__contar_arreglo(id)` | un resumen corto | read |
| `autoarreglo__aplicar_arreglo(id)` | un solo commit en tu rama actual | act, pregunta |
| `autoarreglo__descartar_arreglo(id)` | lo tira y no vuelve a intentar ese problema | act, sin pregunta |
| `autoarreglo__revertir_arreglo(id)` | lo deshace con un commit nuevo | act, pregunta |
| `autoarreglo__compilar()` | compila a Wawi de nuevo, de fondo, y avisa al terminar | act, pregunta |

### Aplicar

- Solo si tu árbol está limpio: sin cambios sin commitear y sin archivos nuevos sin seguir. Si no, te dice por qué y no hace nada.
- Solo si estás en una rama (no con HEAD suelto) y si la rama del arreglo mezcla limpio con la tuya (lo prueba con `git merge-tree`, sin tocar tu árbol). Si tu rama avanzó y choca, te lo dice y no hace nada.
- Hace `git merge --squash` y un solo commit, con tu nombre. El mensaje empieza con `Autoarreglo:`, cuenta el cambio, la causa y las pruebas, y termina con el trailer `Autoarreglo: <id>`.
- Borra la rama `autoarreglo/<id>`. Nunca hace push, reset, rebase de tu rama ni force.
- Después te dice que hay que recompilarla y reiniciarla, y te ofrece `compilar`.

### El vigía

Después de aplicar, mira el primer arranque de Wawi (la primera línea `startup` de `logs/session.log` después del arreglo) durante 30 minutos. Ese arranque sale mal si:

- se cerró de golpe (el próximo `startup` llegó sin su `ended:` y `shutdown`);
- hubo 2 incidentes graves (cerebro caído, plugin caído, turno fallido);
- o volvió a pasar el problema que arreglaba.

Entonces te avisa: «El arreglo que apliqué para … parece que me hizo mal: … ¿Lo deshago?». No deshace nada sola. `revertir_arreglo` hace `git revert` de ese commit (solo de un commit con su trailer, que esté en tu rama y que no esté ya deshecho), con un commit nuevo y el trailer `Autoarreglo-Revierte: <sha>`. Nunca reset, nunca force. El problema vuelve a quedar abierto, con una semana de espera.

## El primer commit

Para arreglarse necesita una base: un repo con al menos un commit. Si el repo no tiene ninguno, no hace nada y te lo pide una vez por semana: «Para poder arreglarme necesito que el repo tenga un primer commit». Wawi nunca hace ese commit por vos.

## Lo que cuesta

Cada intento con Opus puede salir caro. Por eso hay topes, todos en `<home>/autoarreglo/config.toml`:

| Tope | Por defecto |
| --- | --- |
| intentos por día | 2 |
| presupuesto del día (todo junto: autor, revisor, revisión de la noche) | US$ 10 |
| tope del autor por intento (`--max-budget-usd`) | US$ 4 |
| tope de la segunda opinión | US$ 1 |
| tope de la revisión de la noche | US$ 0,30 |
| tiempo del autor | 30 minutos |

Antes de empezar un intento revisa que lo gastado hoy más el tope del intento entre en el presupuesto del día. El costo de cada llamada (el `total_cost_usd` del CLI) se suma aunque la llamada falle. Una base roja no gasta nada. La cuenta está en `<home>/autoarreglo/estado.json`, y `wawi-autoarreglo estado` la muestra.

## Cómo prenderlo y apagarlo

Viene **apagado**. Apagado igual anota los problemas (no cuesta nada) pero nunca llama a un modelo.

1. Instalá el programa:

   ```sh
   cargo install --path crates/wawi-autoarreglo --locked
   ```

2. Prendé el plugin: decile «Wawi, conectá el autoarreglo». Wawi lo instala sola.
3. Hacé el primer commit del repo, si todavía no lo tiene.
4. Escribí `~/.wawi2/autoarreglo/config.toml`:

   ```toml
   activo = true
   repo = "/Users/vos/Work/wawi2"
   ```

5. Abrí Wawi. El worker corre adentro del plugin. También se puede correr solo, por ejemplo desde launchd: `wawi-autoarreglo run`. Un archivo de lock hace que haya uno solo por carpeta.

Para apagarlo, `activo = false`. Lo lee de nuevo cada minuto: no hace falta reiniciar nada.

A mano:

- `wawi-autoarreglo estado`: los problemas, los arreglos y lo gastado hoy.
- `wawi-autoarreglo intentar [id]`: un intento ahora, fuera de la ventana horaria (respeta los topes).
- `wawi-autoarreglo revisar`: la revisión de la noche, ahora.

### Todo el archivo

```toml
activo = false
repo = ""                        # o $WAWI2_REPO
modelo = "claude-opus-5-5"       # el autor y la segunda opinión
max_intentos_dia = 2
presupuesto_dia_usd = 10.0
presupuesto_intento_usd = 4.0
presupuesto_revision_usd = 1.0
tiempo_max = "30m"
ventana = "01:00-07:00"          # "" = a cualquier hora
max_lineas = 400
max_archivos = 12
min_repeticiones = 2
reintento_dias = 7
nice = 10
probar_base = true
target = ""                      # por defecto <home>/autoarreglo/target
compilar = ["cargo", "build", "--release", "-p", "wawi-app"]

[revision]
activa = true
hora = "04:00"
modelo = "claude-sonnet-5-5"
presupuesto_usd = 0.3

[pruebas]
fmt = ["cargo", "fmt", "--all", "--check"]
clippy = ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]
test = ["cargo", "test", "--workspace"]
velocidad = [
  ["cargo", "test", "-p", "wawi-brain-claude", "--test", "fake_cli", "--", "--exact", "a_turn_never_waits_for_a_respawn"],
  ["cargo", "test", "-p", "wawi-app", "--", "--exact", "voice::tests::a_turn_flows_from_the_key_to_the_last_sound"],
  ["cargo", "test", "-p", "wawi-app", "--", "--exact", "voice::tests::pressing_the_key_while_she_speaks_cuts_audio_and_the_brain"],
]
tiempo_max = "25m"
```

Un archivo con errores se ignora entero y queda apagado; el motivo va a `logs/autoarreglo.log`.

## Dónde queda cada cosa

```
<home>/
  cola/incidentes/{tmp,new,cur}   los incidentes
  autoarreglo/
    config.toml                   tus ajustes
    estado.json                   lo gastado, la revisión, el intento en curso
    problemas/<id>.json           el diario
    trabajo/<id>/                 el worktree de un intento (se borra al terminar)
    intentos/<id>/                el brief, el diff y el resultado de cada intento
    propuestas/<id>/              propuesta.md, cambios.diff, propuesta.json
    target/                       lo que compila cargo para los intentos
  logs/autoarreglo.log            una línea por cosa que pasó, sin secretos
```

## Qué se dejó de Wawi 1

Se quedó: el diario con huella y contador, los briefs tapados, los caminos prohibidos, las compuertas, una semana de espera después de un fallo, y el vigía con revert (nunca reset).

Se sacó: aplicar sola los cambios «internos» (ahora todo se propone), alternar entre Codex y Claude, la terminal de Orca, el ejecutor remoto por ssh, las ideas propias de la noche, mirar si hay batería, reuniones o teclado (ahora hay una ventana horaria y prioridad baja), y leer los informes de cuelgue de macOS. El vigía ya no reinicia a Wawi: te ofrece deshacer.

## Límites conocidos

- La mediana de latencia vive en memoria: arranca de cero en cada sesión y necesita 5 turnos.
- El vigía toma como «primer arranque» el primero después de aplicar, aunque todavía no hayas recompilado. Si recompilás con `compilar`, conviene reiniciar recién después.
- Un intento que corre adentro del plugin se corta si cerrás Wawi: se mata el proceso de Claude y al volver se limpia el worktree, sin contarlo como fallo. Para intentos largos de noche conviene `wawi-autoarreglo run` aparte.
- `Bash(cargo *)` deja correr cualquier subcomando de cargo, y las pruebas corren código. El worktree está aparte, sin red para cargo, y nada de eso llega a tu rama sin tu sí.
- Las pruebas de velocidad que vienen por defecto son las de Wawi 2. Si cambian de nombre, hay que cambiarlas en `[pruebas]`.
