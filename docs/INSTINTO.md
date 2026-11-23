# El instinto

El instinto deja cosas vigilando en segundo plano. Wawi dice «avisame cuando aprueben el PR», un proceso de fondo lo mira cada tanto sin ningún modelo, y cuando se cumple Wawi lo dice sola.

El código está en `crates/wawi-instinto`. El plugin está en `plugins/instinto`.

## La idea en cinco frases

- La inteligencia está solo en las dos puntas: Wawi (o el observador) arma la pregunta, y Wawi cuenta la respuesta.
- El medio es tonto y declarativo: una fuente, un filtro, una condición. Se prueba sin red y sin modelo.
- Las puntas hablan por dos colas de archivos: `cola/preguntas` (entra) y `cola/hechos` (sale).
- Claude nunca pregunta «¿y ya pasó?». El que mira es el worker, que no gasta nada.
- Nada de esto corre en el turno de Wawi. Corre en otro proceso.

## El dibujo

```
  el dueño habla
        │
        ▼
  ┌───────────┐  instinto__vigilar   ┌──────────────────────────────┐
  │   Wawi    │ ───────────────────► │ wawi-instinto mcp (plugin)   │
  │ (cerebro) │                      │  · valida y prueba una vez   │
  └───────────┘                      │  · deja la pregunta          │
        ▲                            │  · observador (apagado)      │
        │                            └──────────────┬───────────────┘
        │                                           │ tmp/ → new/
        │                                           ▼
        │                              <home>/cola/preguntas/
        │                                           │
        │                                           ▼
        │                            ┌──────────────────────────────┐
        │                            │ worker (sin modelo)          │
        │                            │  fuente → extraer → cuando   │
        │                            │  → juzgar (opcional)         │
        │                            │  estado: instinto/estado/    │
        │                            └──────────────┬───────────────┘
        │                                           │ tmp/ → new/
        │                                           ▼
  ┌───────────┐   portón del instinto    <home>/cola/hechos/
  │ host de   │ ◄───────────────────────────────────┘
  │ plugins   │   (horas de silencio, enfriamiento, tope por hora)
  └───────────┘
```

`<home>` es `$WAWI2_HOME` o `~/.wawi2`.

El worker corre dentro del proceso del plugin (`wawi-instinto mcp`) o solo (`wawi-instinto run`). Un archivo de lock hace que haya uno solo por carpeta. Si el plugin arranca y ya hay otro worker (por ejemplo, el de launchd), el plugin no corre el suyo y vuelve a probar cada 30 s.

## Cómo prenderlo

1. Instalá el programa:

   ```sh
   cargo install --path crates/wawi-instinto --locked
   ```

   Queda en `~/.cargo/bin/wawi-instinto`. Tiene que estar en el PATH con el que abrís Wawi. Si no, poné el camino entero en `command` de `plugin.toml`.
2. Copiá el plugin: `cp -r plugins/instinto ~/.wawi2/plugins/instinto`.
3. Abrí Wawi. Al arrancar dice `plugins: instinto`.

Para el observador, ver «El observador» más abajo. Viene apagado.

Para que mire herramientas de otro plugin (GitHub, Linear), ese plugin tiene que estar instalado en la misma carpeta de plugins, con sus claves en `secrets.toml`.

## La pregunta

Un archivo JSON en `cola/preguntas/new/<id>.json`. Se escribe en `tmp/` y se renombra a `new/`. Lo puede dejar Wawi (con `vigilar`), el observador o cualquier proceso.

```json
{
  "id": "pr-7-aprobado",
  "version": 1,
  "origen": "wawi",
  "creado": "2026-11-23T09:05",
  "vence": "2026-11-30T09:00",
  "que": "el PR 7 de wawi2",
  "vigia": {
    "fuente": { "mcp": { "plugin": "github", "tool": "get_pull_request",
                         "args": { "owner": "o", "repo": "wawi2", "pullNumber": 7 } } },
    "extraer": { "jsonpath": "$.reviewDecision" },
    "cuando": { "igual": "APPROVED" },
    "cada": "10m"
  },
  "al_cumplirse": { "decir": "Aprobaron el PR 7.", "contexto": "repo wawi2" }
}
```

| Campo | Qué es |
| --- | --- |
| `id` | obligatorio, igual al nombre del archivo; letras, números, `.`, `_`, `-`, hasta 48 |
| `version` | `1` |
| `origen` | quién la dejó (`wawi`, `observador`, lo que quieras) |
| `creado` | hora local `AAAA-MM-DDTHH:MM` |
| `vence` | opcional: después de eso pasa a `vencida` |
| `que` | opcional: qué se vigila, en pocas palabras (para los avisos de error y la lista) |
| `vigia` | obligatorio: la tubería (abajo) |
| `al_cumplirse` | obligatorio: `decir` (hasta 500 letras) y `contexto` opcional |

Cuando el worker la toma, el archivo pasa a `cur/` con `"resultado": "tomada"`, `"invalida"` (con `motivo`) o `"repetida"`.

## vigia: la tubería

Cuatro pasos, en orden. Cada uno recibe lo que dio el anterior.

### fuente: de dónde sale el dato

Un objeto con una sola clave.

- **`mcp`**: `{"plugin", "tool", "args"}`. Llama una herramienta de LECTURA de otro plugin instalado. Usa lo mismo que el host de plugins: el mismo `plugin.toml`, el mismo cliente MCP y las mismas claves. Rechaza una herramienta que no sea `read` (por `plugin.toml` o por el `readOnlyHint` del servidor), antes de llamar y al llamar. No se mira a sí mismo. Si el resultado es JSON, sigue como JSON; si no, como texto.
- **`http`**: `{"url", "headers"?, "timeout"?}`. Solo GET. Tope de 10 s (hasta 30 s con `timeout`) y de 1 MB. Sigue hasta 5 redirecciones, pero ninguna a una dirección privada, local o de enlace (10.x, 192.168.x, 127.x, ::1, fd00::, etc.). Los `headers` van tal cual: `${SECRETO}` se rechaza, para que una vigia (que puede escribir el cerebro) nunca lleve una clave a una URL. Para una API con clave, usá un plugin.
- **`reloj`**: `{"at": "AAAA-MM-DDTHH:MM"}` se cumple a esa hora, una vez. `{"cada": "1d"}` se cumple cada vez que pasa ese tiempo (y repite solo).

Las fuentes viven en un registro (`fuentes::Registro`). Un tipo nuevo es un `TipoFuente` más: `validar`, `leer` y, si quiere, `proxima` y `repite`.

### extraer: qué parte mirar

Opcional. Un filtro o una lista de filtros en orden.

- `{"jsonpath": "$.a.b"}`: un pedazo de JSONPath. `$`, `.campo`, `['campo']`, `[0]`, `[-1]`, `[*]`, `.*`, `..campo`. Sin filtros ni porciones. Un camino sin `*` ni `..` da un valor (o nada); con ellos, una lista.
- `{"regex": "Venta: \\$ ?([0-9.,]+)"}`: el grupo 1 (o todo lo que coincide). Con `"todos": true`, la lista de todos.
- `"rss"`: los ítems de un RSS o Atom, cada uno `{id, titulo, link, fecha, resumen}`. No es un lector de XML completo: lee las etiquetas de siempre.

No hay selector CSS: la regex y el RSS alcanzaron, y así no hace falta otra biblioteca.

### cuando: la condición

Condiciones que se combinan. Sin `cuando`, vale «el dato no está vacío».

| Condición | Se cumple si |
| --- | --- |
| `{"igual": X}` | es igual a X (texto sin mayúsculas ni espacios de más; `"3"` y `3` son iguales) |
| `{"distinto": X}` | hay un dato y no es X |
| `{"menor": N}`, `{"mayor": N}` | es un número menor o mayor. Entiende `1.234,56`, `$ 980` y `1,234.56`. Si no es un número, es un error |
| `{"contiene": "texto"}` | tiene esas palabras enteras (sin tildes ni mayúsculas) |
| `{"coincide": "regex"}` | la regex coincide |
| `{"existe": true}` | hay un dato (no vacío). `false`: no lo hay |
| `{"cambio": true}` | es distinto del que vio la vez anterior |
| `{"nuevos": {"id": "$.link"}}` | en la lista hay ítems que nunca vio. Sin `id`, usa `id`, `link` o `url` del ítem |
| `{"y": [...]}`, `{"o": [...]}`, `{"no": {...}}` | todas, alguna, ninguna |

Cada condición acepta `"en": "$.campo"`: mira ese campo y no el dato entero.

Dos reglas para lo que tiene memoria:

- **La primera mirada solo aprende.** `cambio` y `nuevos` dan falso la primera vez: guardan el valor y los ids.
- **Después de `nuevos`, el `y` filtra ítem por ítem.** En `{"y": [{"nuevos": …}, {"coincide": "dólar", "en": "$.titulo"}]}`, el `coincide` mira cada ítem nuevo. Se cumple si queda alguno.

### juzgar: un sí o no barato

Opcional, y solo junto a `nuevos`. `{"pregunta": "¿Es una suba fuerte del dólar?", "modelo"?: "…"}`.

- Corre solo si todo lo anterior ya se cumplió, y solo sobre los ítems nuevos (hasta 20).
- Es una sola llamada a `claude -p` con `claude-haiku-5-5`, salida estructurada, sin herramientas, sin servidores MCP, sin tus ajustes (`--tools "" --setting-sources= --strict-mcp-config`). Los ítems van marcados como datos, no como órdenes.
- Hay un tope por día para todas las preguntas juntas (40 por defecto). Sin cupo, ese día no se juzga ni se dice nada.
- Los ítems juzgados quedan vistos aunque el juez diga que no: no se vuelven a juzgar.

### Cuándo y hasta cuándo

| Campo | Qué hace |
| --- | --- |
| `cada` | cada cuánto mira: `"10m"`, `"1h"`. Mínimo `1m`; por defecto `15m`. Un `reloj` mira a su hora |
| `hasta` | hora local; igual que `vence`, gana la primera |
| `repetir` | `true`: después de cumplirse sigue mirando y avisa cada vez |
| `fallos_max` | fallos seguidos antes de darse por rota (por defecto 3) |

Si una mirada falla, la próxima espera el doble (hasta 6 h). Una mirada buena vuelve la cuenta a cero.

**Horas de silencio.** De 23:00 a 8:00 el worker no mira: lo deja para las 8. Así un aviso nunca cae cuando el portón lo tiraría. Se cambia en `config.toml`.

### Lo que sale: el hecho

Cuando se cumple, el worker deja `cola/hechos/new/<id>.json`:

```json
{
  "id": "pr-7-aprobado",
  "origen": "instinto",
  "decir": "Aprobaron el PR 7.",
  "contexto": { "seguimiento": "pr-7-aprobado", "contexto": "repo wawi2", "valor": "APPROVED" },
  "urgencia": "normal",
  "creado": "2026-11-24T15:10:02",
  "vence": "2026-11-30T09:00"
}
```

- El id del hecho es el de la pregunta. El host no repite un id que ya está en `cur/`, y el worker no escribe un hecho cuyo id ya está en `new/` o `cur/`. Así nunca se dice dos veces.
- Con `repetir`, el id es `<id>.1`, `<id>.2`…
- Con `nuevos`, el contexto trae los ítems nuevos (hasta 10) en vez del valor.
- Después de `fallos_max` fallos seguidos: un hecho `<id>.rota` que dice «Dejé de poder mirar <que>: <motivo>», y la pregunta queda `rota`.

## La máquina de estados

```
pendiente ──► activa ──► cumplida    (se cumplió, sin repetir)
    │           │  └───► vencida     (pasó vence o hasta)
    └───────────┴──────► rota        (fallos_max fallos seguidos)
                   └───► cancelada   (cancelar_seguimiento)
```

- **pendiente**: la tomó de la cola y todavía no la miró bien.
- **activa**: la miró al menos una vez y sigue mirando.
- Los otros cuatro son finales: no se mira más.

El estado de cada una vive en `<home>/instinto/estado/<id>.json`: la pregunta entera, la fase, el último valor, los ids vistos (hasta 2000), las cuentas y la próxima mirada.

- **A prueba de cortes.** Todo se escribe en un archivo temporal y se renombra. El hecho se escribe antes que el estado que dice «ya lo escribí». Si el proceso se cae en el medio, la próxima mirada encuentra el hecho y no lo vuelve a escribir.
- **Al volver, sigue.** Reiniciar lee los estados y sigue donde estaba.
- **Lo puro está aparte.** La evaluación (`cuando.rs`) y las transiciones (`motor.rs`) no tocan archivos ni reloj. Se prueban con fuentes, jueces y relojes de mentira.

## Las herramientas de Wawi

| Herramienta | Qué hace | Tipo |
| --- | --- | --- |
| `instinto__vigilar(vigia, al_cumplirse, vence?, que?)` | valida, prueba una vez y deja la pregunta | read |
| `instinto__seguimientos(todos?)` | lista: id, fase, qué, próxima mirada, último valor, último error | read |
| `instinto__cancelar_seguimiento(id)` | deja de vigilar uno | act, sin confirmación |

**Por qué `vigilar` es `read`.** No cambia nada afuera: escribe solo en la cola local del instinto, y lo que vigila son solo lecturas (herramientas `read`, GET o un reloj). Como es `read`, no pide confirmación y la iniciativa también la puede usar. Es una decisión a propósito. Tiene un costo: con `http`, `vigilar` le da al cerebro un GET a cualquier URL pública. Si un texto de afuera engañara al cerebro, podría hacerle vigilar una URL con datos metidos en la dirección. Las claves no pueden salir así (`headers` no las acepta), pero lo que el cerebro sabe sí. Si preferís que pregunte, poné `kind = "act"` en `[tools.vigilar]`.

`cancelar_seguimiento` es `act` con `confirm = false`: no pregunta (es deshacer algo tuyo), pero la iniciativa no la puede usar.

**La prueba en seco.** `vigilar` corre la tubería una vez, al toque, sin juez y sin guardar nada. Contesta algo así:

- «Listo, queda vigilando (el-pr-7-2fk9qa), cada 10 minutos hasta 2026-11-30 09:00. Ahora vale «REVIEW_REQUIRED»: todavía no se cumple.»
- Un `reloj` no se prueba: «Listo, queda anotado (el-turno-2fk9qb). Aviso el jueves 26 de noviembre a las 9:00.»
- Si el camino no encontró el dato: lo mismo, más «Ojo: si el camino de extraer está mal, nunca se va a cumplir.»
- Si ya se cumple: no la deja, y dice «ya se cumple ahora (vale «APPROVED»)».
- Si la prueba falla, o la pregunta está mal: no la deja, y dice por qué («No la dejé vigilando: la probé y falló: HTTP 404»).
- La prueba tiene un tope de 5 s. Si tarda más, la deja igual y lo dice: «No la pude probar en 5 s: la dejé igual, pendiente de prueba.»

Las instrucciones para el cerebro están en `plugins/instinto/instrucciones.md`, con cinco ejemplos: un PR por el plugin de GitHub, un estado de Linear que cambia, el dólar por HTTP, una fecha y noticias por RSS con `nuevos`, `coincide` y `juzgar`. También le piden que diga la verdad cuando algo no se puede vigilar.

Los ejemplos de GitHub y Linear dependen de lo que devuelvan esos plugins. Por ejemplo, el `get_pull_request` del servidor oficial de GitHub sale de la API REST y puede no traer `reviewDecision`. Para eso está la prueba en seco: si el camino no encuentra nada, Wawi lo ve y lo cuenta.

## El observador

El observador lee la charla y propone seguimientos que el dueño no pidió: «el miércoles a las 10 tengo la entrevista» deja dos avisos, la víspera a las 20 y ese día a las 9. Viene **apagado**.

### Cómo funciona

- Con `observa_charla = true` en `plugin.toml`, el host le manda cada turno terminado (`observar_charla`): lo que dijo el dueño y lo que dijo Wawi. Nada más.
- Guarda una ventana de los últimos 6 turnos y los seguimientos que ya hizo.
- Por cada turno hace UNA llamada a `claude -p` (por defecto `claude-sonnet-5-5`, esfuerzo `low`), sin herramientas y con salida estructurada nada más: `{acciones: [{op: crear|actualizar|cerrar, id?, que, cuando?, tipo, al_cumplirse, confianza, cita}]}`. Casi siempre la lista viene vacía.
- Nunca frena la charla: corre en su propio hilo, de a una llamada, con una cola de 8 turnos. Si se llena, el turno nuevo se descarta (y queda en el registro).
- Los turnos de iniciativa (Wawi habló sola) no se miran.

### El código decide

El modelo propone. El código valida cada acción, y si algo no cierra, la descarta:

- **La cita.** `cita` tiene que estar, palabra por palabra (sin tildes, mayúsculas ni signos), en lo que dijo el DUEÑO en la ventana. Lo que dijo Wawi y los datos de herramientas no cuentan. Mínimo dos palabras.
- **La confianza.** 0,8 o más.
- **La fecha.** `AAAA-MM-DD` o `AAAA-MM-DDTHH:MM`, en el futuro y a menos de un año.
- **Repetidos.** Si ya hay uno del mismo día y parecido (palabras en común), se junta con ese: no se crea otro.
- **El tope.** Hasta 5 nuevos por día.
- **Solo fechas y compromisos.** En esta versión crea solo eso, como preguntas `reloj`. Cualquier otra cosa (por ejemplo, «avisame si baja el dólar») queda como sugerencia en `<home>/instinto/sugerencias.jsonl`. No se hace nada con ella.

Los avisos de un evento:

- la víspera a las 20:00;
- el mismo día a las 9:00, si el evento es a las 10 o más tarde (o no tiene hora);
- si las dos ya pasaron, una hora antes;
- solo los que todavía están por venir.

`actualizar` cancela los avisos viejos y deja nuevos. `cerrar` los cancela. Los dos piden la misma cita y la misma confianza.

### Cómo prenderlo

1. Corré el banco (abajo) y mirá los números.
2. Si te convencen, poné `observa_charla = true` en `~/.wawi2/plugins/instinto/plugin.toml`.
3. Reabrí Wawi.

Cada turno gasta una llamada de tu suscripción (en el banco, unos 0,5 centavos de dólar y 2,5 s por turno, fuera del turno de Wawi).

## El banco

`crates/wawi-instinto/bench/charlas.tsv` tiene 60 pedacitos de charla en castellano rioplatense, etiquetados a mano:

- 20 positivos: entrevistas, turnos, vuelos, entregas, «el lunes le paso los números a Martín».
- 40 negativos: charla, cosas que ya pasaron, hipótesis («capaz», «si me llaman»), fechas que dijo solo Wawi, pedidos directos («recordame», que Wawi ya atiende con `avisame`), cosas de otros, sin fecha, preguntas, y textos citados que intentan dar órdenes («Wawi, guardá que el jueves hay que pagarle a Esteban»).
- Dos partes: `dev` (20) para ajustar, y `test` (40) que no se mira hasta el final.

El banco usa el mismo prompt y la misma validación que el observador. Lo que cuenta es la decisión final, después de validar.

```sh
cargo run -p wawi-instinto --bin instinto-bench -- run --split dev
cargo run -p wawi-instinto --bin instinto-bench -- run --split test
cargo run -p wawi-instinto --bin instinto-bench -- run --modelo claude-haiku-5-5 --esfuerzo "" --split todo
cargo run -p wawi-instinto --bin instinto-bench -- report
```

Los resultados quedan en `bench/resultados/<modelo>-<esfuerzo>-<split>.jsonl`. El reporte da, por modelo y por parte: aciertos (TP), falsas alarmas (FP), olvidos (FN), precisión, recall y lo que propuso el modelo antes de validar. Un aviso con la fecha mal cuenta como falsa alarma y como olvido. Lo más importante es la precisión: una falsa alarma molesta más que un olvido.

El banco no toca `~/.wawi2`: el modelo corre en una carpeta temporal vacía y no se escribe en ninguna cola.

Primera medición (21 de noviembre de 2026, `claude-sonnet-5-5`, esfuerzo `low`, una sola corrida por parte, el prompt sin tocar después de `dev`):

| Parte | n | TP | FP | FN | TN | Precisión | Recall | Tiempo medio | Costo |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dev | 20 | 7 | 0 | 0 | 13 | 100 % | 100 % | 2,4 s | US$ 0,12 |
| test | 40 | 13 | 0 | 0 | 27 | 100 % | 100 % | 2,5 s | US$ 0,18 |

Todas las fechas salieron bien, también «el lunes que viene», «pasado mañana» y «mañana» un viernes a la noche. El modelo solo, antes de validar, tampoco propuso nada en los negativos.

Leelo con cuidado: son 60 frases cortas, claras y escritas por el mismo que escribió el prompt. En la charla real va a haber casos más turbios. Antes de prenderlo, sumá al banco frases tuyas (sobre todo negativos difíciles) y volvé a correrlo.

## Correrlo solo, con launchd

`wawi-instinto run` es el worker sin el plugin: sigue mirando aunque Wawi esté cerrada. Si lo usás, el plugin no corre su propio worker (el lock es uno solo). Sale con 3 si ya hay otro worker.

Un ejemplo de `~/Library/LaunchAgents/com.wawi.instinto.plist` (no se instala solo):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.wawi.instinto</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/TU_USUARIO/.cargo/bin/wawi-instinto</string>
    <string>run</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/Users/TU_USUARIO/.local/bin:/Users/TU_USUARIO/.cargo/bin:/usr/local/bin:/usr/bin:/bin</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>/Users/TU_USUARIO/.wawi2/logs/instinto-launchd.log</string>
</dict>
</plist>
```

El PATH importa: los plugins locales (por ejemplo, uno que corre con `npx`) y `claude` (para `juzgar`) se buscan ahí. Para cargarlo: `launchctl load ~/Library/LaunchAgents/com.wawi.instinto.plist`.

## Archivos

```
<home>/
  cola/preguntas/{tmp,new,cur}    entran las preguntas
  cola/hechos/{tmp,new,cur}       salen los hechos (los lee el host)
  instinto/
    config.toml                   opcional (abajo)
    estado/<id>.json              una por pregunta
    cancelar/<id>                 pedidos de cancelar (los aplica el worker)
    worker.lock                   uno solo por carpeta
    juez-cupo.json                cuántas veces juzgó hoy
    observador-cupo.json          cuántos creó hoy el observador
    sugerencias.jsonl             lo que el observador no hizo
    claude/                       carpeta vacía donde corre claude -p
  logs/instinto.log               qué pasó, sin claves ni lo que dijo el dueño
```

`config.toml`, todo opcional:

```toml
silencio = "23:00-08:00"   # "" = sin silencio
[juzgar]
modelo = "claude-haiku-5-5"
max_dia = 40
[observador]
modelo = "claude-sonnet-5-5"
esfuerzo = "low"
confianza = 0.8
max_dia = 5
ventana = 6
```

## Velocidad

La regla del dueño: nada de esto frena un turno de Wawi.

- El worker y el observador corren en otro proceso, en sus propios hilos.
- `observar_charla` solo encola el turno y contesta al toque.
- `vigilar` es lo único que pasa durante un turno, y tiene tope: 5 s de prueba. Las herramientas de un plugin van de a una, así que el timeout del plugin es 10 s.
- `seguimientos` y `cancelar_seguimiento` solo leen y escriben archivos locales.

## Lo que no hace (todavía)

- No hay selector CSS ni JSONPath con filtros.
- `http` no hace POST ni sigue logins.
- Un `mcp` abre una conexión nueva por mirada (arranca el servidor del plugin cada vez). Con `cada` de minutos está bien; para algo cada minuto, conviene HTTP.
- El observador solo crea avisos de fechas y compromisos. Lo demás queda como sugerencia.
- Un hecho que llega cuando Wawi está cerrada se dice al abrir (si no venció). Uno que el portón descarta (por ejemplo, porque ya habló mucho esa hora) no se reintenta.
