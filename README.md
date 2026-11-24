# Wawi 2

[![CI](https://github.com/msenatori/wawi2/actions/workflows/ci.yml/badge.svg)](https://github.com/msenatori/wawi2/actions/workflows/ci.yml)

Wawi es un asistente de voz. Esta es la reescritura desde cero en Rust: primero para Mac, y en Rust para que después puedan venir otros sistemas.

Esta primera parte es el núcleo del diálogo, un banco de pruebas, el cerebro real, un reconocedor de voz local, la voz de Wawi (TTS local) y la capa de audio en vivo para Mac (micrófono con la tecla y reproducción, con cancelación de eco). Con `--voice` todo eso está enchufado: mantenés la ⌘ derecha, hablás y Wawi te contesta en voz alta (ver «Cómo hablar con Wawi 2»).

## Instalación

### Qué necesitás

- Una Mac con Apple Silicon (M1 o más nueva). Se probó en macOS 26; pide macOS 13 o más nuevo.
- [Rust](https://rustup.rs) para compilarla (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`).
- [Claude Code](https://claude.com/claude-code), el programa `claude`, con tu cuenta de Claude (Pro o Max). Wawi lo usa como cerebro, con tu suscripción. No hace falta clave de API.
- Unos 2 GB libres: 1 GB para los modelos de voz y el resto para compilar.

### 1. Bajá el código y armá la app

```sh
git clone https://github.com/msenatori/wawi2.git
cd wawi2
scripts/bundle-mac.sh
```

Compila y deja `dist/Wawi.app`. Movela a Aplicaciones si querés (`WAWI_APP_OUT=/Applications scripts/bundle-mac.sh` la arma directamente ahí).

Si preferís no armar la app, también anda desde la terminal: `cargo run --release -p wawi-app -- --voice`.

### 2. Prepará todo con `setup`

```sh
dist/Wawi.app/Contents/MacOS/wawi2 setup
# o, desde el código:  cargo run --release -p wawi-app -- setup
```

`setup` hace esto, en orden:

1. Crea la carpeta `~/.wawi2`.
2. Baja los dos modelos de voz (unos 1000 MB, una sola vez): Parakeet para entender y Kokoro para hablar. Muestra el avance y revisa el tamaño y el SHA-256 de cada archivo, de una versión fija.
3. Busca `claude` y se fija si iniciaste sesión. Si falta algo, te dice cómo arreglarlo.
4. Revisa los permisos de macOS. Si falta uno, te abre el panel justo de Ajustes del Sistema y te dice qué app habilitar.

Termina con «Listo. Abrí Wawi y mantené ⌘ derecha para hablarle.». Si algo falta, te dice qué. Lo podés correr las veces que quieras: lo que ya está, solo lo revisa. Con `--sin-abrir` solo informa: no pide permisos ni abre Ajustes.

### 3. Los permisos

macOS da los permisos a la app que abre Wawi:

- Si abrís **Wawi.app**, los permisos son de Wawi. La primera vez te los pide: aceptá el micrófono y habilitá Wawi en Monitoreo de entrada. Si falta uno, Wawi te muestra un aviso, te abre el panel y se cierra. Habilitala y volvé a abrirla.
- Si la corrés **desde la terminal**, los permisos son de la terminal (Terminal, iTerm, tu editor).

Los permisos son tres, en Ajustes del Sistema > Privacidad y seguridad:

- **Micrófono**: para oírte. Solo escucha mientras mantenés la ⌘ derecha.
- **Monitoreo de entrada**: para saber cuándo apretás la ⌘ derecha. No lee otras teclas.
- **Grabación de pantalla**: solo si prendiste el plugin curiosidad (está apagado de entrada).

La app está firmada ad-hoc (sin certificado de Apple). Si macOS te vuelve a pedir los permisos después de recompilar, sacá Wawi de la lista y agregala de nuevo.

### 4. La primera charla

Abrí Wawi.app (doble clic). No aparece en el Dock: aparece la perrita en una esquina y su carita en la barra de menú. Mantené la ⌘ derecha, decile «Hola, ¿qué hora es?» y soltala. Te contesta en voz alta.

Para cerrarla: clic en su carita de la barra de menú > «Salir». Lo que pasó queda en `~/.wawi2/logs` (menú > «Abrir registros»).

### Si no encuentra `claude`

Una app abierta desde el Finder no recibe el PATH de tu terminal. Por eso Wawi busca `claude` y los comandos de los plugins también en `~/.local/bin`, `~/.claude/local`, `~/.cargo/bin`, `~/.bun/bin`, `~/.volta/bin`, `~/.npm-global/bin`, `/opt/homebrew/bin` y `/usr/local/bin`. Si lo tenés en otro lado (por ejemplo con nvm), `setup` anota su carpeta en `~/.wawi2/path`. Ese archivo lo podés editar: una carpeta por línea. `WAWI2_CLAUDE_BIN` elige el programa exacto cuando corrés desde la terminal.

### Lo que falta para distribuirla

Para pasarla a otras personas sin compilar hace falta firmarla con un certificado Developer ID de Apple, con el runtime endurecido, y notarizarla. Eso no está hecho. Tampoco está «Abrir al iniciar sesión» en el menú (se puede hacer con `SMAppService`); por ahora, agregala a mano en Ajustes del Sistema > General > Ítems de inicio.

## Qué funciona

- El dueño del diálogo: una máquina de estados pura, en `crates/wawi-core`. Maneja turnos cancelables, interrupciones, unión de frases cortadas, confirmaciones, watchdog y pedidos de palabra de sistemas de fondo.
- El registro de herramientas y el router, los dos modificables en caliente.
- Permisos aplicados en código. Cada turno sabe si lo pidió el dueño o si es iniciativa de Wawi. La iniciativa solo puede usar herramientas de lectura; si pide otra cosa, el núcleo la rechaza antes de ejecutarla. Los argumentos de cada herramienta tienen que ser un objeto JSON válido.
- Sugerencias: la iniciativa no actúa, propone. El dueño acepta una sugerencia (pasa a ser un pedido suyo) o la descarta, y lo descartado no vuelve más.
- Alma y memoria en archivos de texto, en `crates/wawi-memory`: `SOUL.md`, `MEMORY.md` y `memory/<tema>.md`, en `~/.wawi2` (o en `$WAWI2_HOME`).
- Un tope total por turno y un tope para la cola de avisos.
- Un banco de escenarios y una prueba con miles de eventos al azar, en `crates/wawi-core/tests`.
- Un simulador por consola, `wawi2`, para sentir el flujo.
- El reconocimiento de voz real, en `crates/wawi-stt-parakeet`: Parakeet TDT 0.6B v3 de NVIDIA, local, en la CPU. En el simulador se prueba con un WAV (`/hear`), y tiene su banco de medición contra el Whisper de Wawi 1.
- La voz, en `crates/wawi-tts-kokoro`: Kokoro-82M, local, en la CPU, con la misma voz de Wawi 1 y hablando en rioplatense. Se escucha con `tts-say`.
- El audio en vivo para Mac, en `crates/wawi-audio-mac`: un solo motor de audio con cancelación de eco, la tecla ⌘ derecha para hablar (mantener = hablar, como en Wawi 1) y la reproducción que avisa cuándo terminó de sonar cada frase. Se prueba a mano con `ptt-test`.
- Las primeras herramientas de verdad, en `crates/wawi-tools`: recordar y olvidar datos, y temporizadores y recordatorios que Wawi dice sola cuando llegan (ver «Herramientas»). Además, el cerebro sabe la fecha y la hora local en cada turno.
- Plugins por MCP, en `crates/wawi-plugins`: todo lo que va más allá de lo básico es un plugin, una carpeta en `~/.wawi2/plugins/<nombre>/` con su `plugin.toml`. Sus herramientas pasan por el servidor `wawi` y por los permisos de siempre (ver «Plugins»).
- El instinto: un plugin puede hacer que Wawi hable sola (un watch que mira sin gastar, una rutina a su hora, un aviso empujado, un archivo en `~/.wawi2/cola/hechos`). Un portón en el núcleo decide si se dice y cuándo.
- El servicio instinto, en `crates/wawi-instinto` y `plugins/instinto`: «avisame cuando aprueben el PR» o «si el dólar baja de mil» queda vigilando en otro proceso, sin modelo, y Wawi lo dice cuando pasa. Trae un observador de la charla, apagado, y un banco para decidir si prenderlo. La guía está en `docs/INSTINTO.md`.
- La personalidad: un alma en cinco partes con una línea de ancla que viaja en cada turno, el plugin carácter (aprende cómo le gusta al dueño que le hablen, en `trato.md`; apagado) y el plugin curiosidad (mira la pantalla de vez en cuando y opina). La guía está en `docs/PERSONALIDAD.md`.
- El cerebro real, en `crates/wawi-brain-claude`: Claude Code en modo headless (`claude -p`), con la suscripción del dueño. Sin clave de API. Un solo proceso vive entre turnos, el texto llega en vivo frase por frase, las herramientas de Wawi le llegan como servidor MCP dentro del mismo proceso y cancelar interrumpe de verdad.

## Qué no funciona

- No hay detección de voz (se habla con la tecla). La interfaz es solo el cuerpo de Wawi (la mascota y el ícono de la barra de menú, ver «El cuerpo de Wawi»): no hay panel ni historial. El simulador sigue sin audio; la voz real está solo en `--voice`.
- Hay pocas herramientas a propósito: las de «Herramientas» y nada más. Las de Wawi 1 no se portaron.
- En el simulador son de mentira `clima` y `borrar` (las de verdad sí están), la transcripción, y el cerebro también, salvo con `--brain claude`. La hora es la real al arrancar y avanza con el reloj simulado (`/tick`). La transcripción es real solo para los archivos de `/hear`, con `--stt parakeet`. La reproducción es una línea de texto.
- El esquema JSON de cada herramienta se guarda, pero no se valida: solo se revisa que los argumentos sean un objeto JSON.
- La iniciativa la disparan los plugins solo en `--voice`; en el simulador se prueba a mano. Las sugerencias descartadas se olvidan al cerrar.
- Los plugins solo corren en `--voice`, no en el simulador. Todavía no hay ventana de Ajustes: un plugin se conecta por voz («Wawi, conectá Linear»).
- El ordenamiento de la memoria con un modelo no existe. Solo está el validador que va a decidir si se acepta lo que proponga.
- La voz entera (`--voice`) se probó de punta a punta con un WAV y en silencio (`--input-wav`, `--mute`), con el cerebro, Parakeet y Kokoro reales. Con el micrófono y la tecla de verdad todavía no la probó nadie a mano.

## Cómo hablar con Wawi 2

```sh
cargo run --release -p wawi-app -- --voice
```

Al arrancar revisa y va contando, en este orden: los permisos de **Micrófono** y de **Monitoreo de entrada** (los dos para la app desde la que lo corrés, igual que `ptt-test`), que estén los modelos de Parakeet y Kokoro (si falta alguno, te dice que corras `wawi2 setup` y sale: no baja nada solo; esta revisión solo mira tamaños, no recalcula hashes), carga los dos modelos, abre la memoria y los avisos en `~/.wawi2` (o `$WAWI2_HOME`) y dice qué herramientas tiene y cuántos avisos quedan pendientes, arranca Claude Code para que el primer turno no espere, arranca el audio con cancelación de eco y la tecla. Cuando aparece «Listo: mantené ⌘ derecha y hablá», ya está.

Mantené la ⌘ derecha, hablá y soltala. En la terminal ves cada turno así:

```
vos:  «¿Qué hora es?»
wawi: «Son las 16:23.»
  tiempos: transcripción 125 ms · primera frase 125 ms · primer audio 545 ms · total sentido 545 ms
```

Los tiempos se cuentan desde que soltás la tecla: hasta tener el texto, hasta la primera frase de Wawi y hasta que empieza a sonar. «Total sentido» es ese último, el silencio que esperás vos. Si apretás la tecla mientras habla, la corta en el acto y cancela lo que estaba pensando. Enter o Ctrl+C cierran todo: el audio, Claude Code y los modelos (un segundo Ctrl+C sale sin esperar). En Wawi.app se cierra con «Salir» en el menú.

- **Cerebro:** `claude-sonnet-5-5` con esfuerzo `low`, lo más rápido que sigue sonando bien: empieza a hablar en alrededor de 1 s en vez de 4 s con Opus. La medición y los ejemplos están en `docs/LATENCIA.md`. `WAWI2_CLAUDE_MODEL` y `WAWI2_CLAUDE_EFFORT` lo cambian (por ejemplo `WAWI2_CLAUDE_MODEL=claude-opus-5-5`). Al arrancar dice cuál usa.
- **Voz:** la de Wawi 1 (`jf_alpha`, velocidad 1,05, tres semitonos arriba) con acento castellano. Cada frase termina con 0,3 s de silencio, para que los auriculares Bluetooth no se coman el final.
- **Idioma:** si lo que oyó Parakeet probablemente no es castellano, no se lo pasa al cerebro: Wawi dice «No te entendí, ¿me lo repetís?» y el turno queda en el registro como «no entendido».
- **Registros:** `~/.wawi2/logs/turns.log` guarda una línea por turno con fecha, los tiempos, lo que oyó y lo que dijo (si algo parece una clave, no se guarda). `~/.wawi2/logs/session.log` es para depurar: qué pasó y cuándo, sin textos ni audio. Abierta como Wawi.app, lo que en la terminal se ve en pantalla va a `~/.wawi2/logs/wawi-app.log` (el de la vez anterior queda en `wawi-app.log.1`).
- **Salidas:** 0 bien, 1 error, 2 argumentos mal, 3 sin permiso de micrófono, 4 faltan modelos, 5 sin permiso de teclado.

Para probarlo sin micrófono y sin que suene nada:

```sh
say -v Paulina "¿Qué hora es?" -o hora.aiff
afconvert -f WAVE -d LEI16@16000 -c 1 hora.aiff hora.wav
cargo run --release -p wawi-app -- --voice --mute --input-wav hora.wav
```

`--input-wav` le pasa el archivo como si lo hubieras dicho (sin micrófono ni tecla) y sale después de ese turno. `--mute` reproduce en silencio pero tardando lo mismo que el audio de verdad, así los tiempos y el fin de cada frase son reales. Nunca toca el volumen. Cada turno con el cerebro gasta de tu suscripción (con Sonnet 5.5, nueve turnos cortos de prueba sumaron unos dos centavos de dólar).

Primera medición (M4 Pro, 20 de noviembre de 2026, con WAV y `--mute`, el modelo por defecto, que fue `claude-opus-5-5`):

| | ¿Qué hora es? (ruta de la hora) | Contame en una frase qué es un arcoíris (cerebro) |
| --- | --- | --- |
| Transcripción | 125 ms | 196 ms |
| Primera frase | 125 ms | 4417 ms |
| Primer audio (total sentido) | 545 ms | 5419 ms |

Con el cerebro, casi todo es el modelo: 3,5 s hasta la primera palabra y 0,7 s más hasta terminar la frase (el núcleo habla por frases enteras). Después Kokoro tardó 1 s porque la primera frase era larga y su primera coma quedaba lejos.

## El cuerpo de Wawi

Con `--voice` aparece Wawi en pantalla, como en Wawi 1: la perrita sentada en una esquina, flotando arriba de las ventanas, en todos los escritorios, sin ícono en el Dock. Lo que dice la terminal sigue igual; `--no-ui` la saca y deja solo la terminal.

```sh
cargo run --release -p wawi-app -- --ui-demo              # el cuerpo solo: recorre todas sus caras, sin audio ni cerebro
cargo run --release -p wawi-app -- --ui-demo=speaking     # quieta en una (idle, listening, transcribing, thinking, speaking, interrupted, error, muted, sleeping)
cargo run --release -p wawi-app -- --voice --no-ui        # sin cuerpo, solo la terminal
```

- **Qué cara pone.** Tranquila: respira, parpadea, mira para los costados, se rasca, se estira; a los 10 s sin nada se acuesta a dormir. Mientras mantenés la tecla: orejas paradas y un puntito verde (te está grabando). Al soltar: cabeza ladeada y tres puntitos (está transcribiendo). Pensando: mueve la cabeza de un lado al otro. Hablando: mueve la boca al ritmo y se balancea. Si la cortás, da un respingo; si algo falla, se encoge de hombros. Con el micrófono apagado lleva el ícono de la oreja tachada y a veces baja las orejas: sigue con su vida, solo no escucha.
- **El globito.** Arriba de su cabeza aparece lo que oyó (entre «», más clarito) y después lo que dice, frase por frase. Unos segundos después de terminar se desvanece; un clic lo cierra.
- **Moverla.** Arrastrala adonde quieras: se acuerda del lugar en `~/.wawi2/mascot.json` (y de si la ocultaste). Si quedó en un monitor que ya no está, vuelve a la esquina de abajo a la derecha. Un clic la hace saltar (o la despierta).
- **Su cuerpo.** Wawi siente la gravedad, como en Wawi 1: si la soltás en el aire, cae al piso (arriba del Dock) o arriba de una ventana; si la ventana donde está sentada se mueve o se cierra, se cae. Cada 8 a 25 minutos se levanta y camina a otro lugar; a veces trepa a una ventana o baja en paracaídas. Mientras charlan se queda quieta. «Quedate quieta» en el menú lo apaga. Cómo darle cuerpo a otra mascota: [docs/MASCOTAS.md](docs/MASCOTAS.md).
- **El menú.** El mismo en la barra de menú (su carita y un signo: ● tranquila, ◉ escuchando, ◍ pensando, ◎ hablando, ◌ sin escuchar) y con clic derecho sobre ella: «Dejar de escuchar» / «Escuchar» (con el micrófono apagado la ⌘ derecha no hace nada), «Ocultar a Wawi» / «Mostrar a Wawi» (con el nombre de la mascota activa), «Quedate quieta» / «Podés moverte» (la gravedad y los paseos), «Mascota» (las mascotas instaladas), «Abrir registros» (abre `~/.wawi2/logs` en el Finder) y «Salir», que cierra igual que Enter o Ctrl+C: corta el audio y cierra Claude Code.
- **Consumo.** Solo se redibuja cuando algo cambia: tranquila o dormida usa alrededor de 1 % de un núcleo (M4 Pro, medido con `--ui-demo`); hablando o pensando, unos 3 %.
- **Los dibujos.** Son los de Wawi 1, copiados de `jarvis/wawi/assets` a `assets/mascotas/wawi` (los cuadros en `cuadros/`, descritos por `mascota.toml`). Wawi la dibujó la sobrina del dueño; los dibujos tienen su propia licencia, CC BY-NC 4.0 (`assets/mascotas/wawi/LICENSE-art.md`), no la del código. Sin los dibujos el programa anda igual, solo con el ícono de la barra de menú. `$WAWI2_ASSETS` apunta a otra carpeta con `mascotas/` adentro. En Wawi.app los dibujos van en `Contents/Resources/mascotas`.
- **Otras mascotas.** Los dibujos y las animaciones no están en el código: una mascota es una carpeta con PNG y un `mascota.toml`. Wawi es la que viene incluida; las tuyas van en `~/.wawi2/mascotas/<id>/` y se eligen en el menú, en «Mascota», sin reiniciar. `mascota nueva kitty --nombre Kitty` crea una con dibujos de ejemplo, `mascota validar CARPETA` la revisa y `mascota ver CARPETA` la muestra (también `wawi2 --ui-demo --mascota CARPETA`). Una mascota rota no rompe nada: Wawi avisa y vuelve a Wawi. Cómo hacer una, paso a paso: [docs/MASCOTAS.md](docs/MASCOTAS.md).
- **Sin frenar la voz.** Los dibujos se decodifican de a poco, en un hilo aparte: antes del primer cuadro solo `idle`, y nunca en el hilo de la voz. Hay un tope de memoria para los dibujos decodificados.

## Cómo correrlo

```sh
cargo test --workspace
cargo run -p wawi-app
```

En el simulador:

| Entrada | Qué simula |
| --- | --- |
| cualquier texto | lo que dijo el dueño, como transcripción final |
| `/talk` | el dueño empieza a hablar (interrumpe) |
| `/stop` | la tecla de cancelar |
| `/done` | terminó la reproducción |
| `/brain` | el cerebro falso avanza un paso |
| `/tick MS` | avanza el reloj simulado |
| `/hear ARCHIVO.wav` | el dueño dijo lo que está en el archivo; lo reconoce Parakeet (solo con `--stt parakeet`) |
| `/initiative TEXTO` | Wawi piensa por su cuenta (turno de iniciativa) |
| `/suggestions` | lista las sugerencias abiertas |
| `/accept N`, `/dismiss N` | acepta o descarta la sugerencia N |
| `/memory` | muestra el alma y la memoria que ve el cerebro |
| `/remember DATO`, `/forget DATO` | lo mismo que `recordar` y `olvidar` (si no es exacto, `/forget` muestra los parecidos y no borra) |
| `/remind SEGUNDOS [TEXTO]`, `/remind AAAA-MM-DDTHH:MM [TEXTO]` | lo mismo que `avisame` |
| `/reminders`, `/cancel N` | lo mismo que `recordatorios` y `cancelar_recordatorio` |
| `/now` | la fecha y la hora que ve el cerebro |
| `/help`, `/quit` | ayuda, salir |

El cerebro falso reacciona a algunas palabras del texto: `borrar` pide confirmación, `clima` usa una herramienta con aviso hablado, `rompe` falla, y `lento` se queda pensando hasta que uses `/brain`. Con `lento` podés probar la unión de frases: escribí `lento poné música` y después `de los Beatles`.

En un turno de iniciativa el cerebro falso intenta usar la herramienta que nombra el texto y después deja una sugerencia. Probá `/initiative borrar fotos viejas`: `borrar` escribe, así que el núcleo la rechaza y queda como sugerencia. Con `/accept 1` se vuelve un pedido tuyo y ahí sí te pide confirmación.

El simulador usa la carpeta `~/.wawi2` (o `$WAWI2_HOME`) y la crea con un `SOUL.md` inicial si no existe.

También acepta entrada por pipe:

```sh
printf 'hola\n/done\n/done\n/quit\n' | cargo run -q -p wawi-app
```

## Herramientas

Pocas y que anden. Son las únicas que ve el modelo (más `suggest`, la de la iniciativa), con estos nombres y descripciones en castellano. Ninguna pide confirmación: son cosas del dueño, chicas y que se deshacen.

| Herramienta | Qué hace | Permiso |
| --- | --- | --- |
| `recordar(dato)` | Guarda un dato en `MEMORY.md`, con la fecha local de hoy. No guarda nada que parezca una clave. | escribe |
| `olvidar(dato)` | Borra el dato que coincide exacto (sin importar mayúsculas, tildes ni signos). Si no hay uno igual, no borra nada y le devuelve al modelo los tres más parecidos. | escribe |
| `avisame(en_segundos o a_las, texto)` | Un temporizador («avisame en diez minutos») o un recordatorio a una hora («recordame mañana a las ocho que llame a mamá»). | escribe |
| `recordatorios()` | Lista los avisos pendientes, con su número. | lee |
| `cancelar_recordatorio(id)` | Cancela uno por su número. | escribe |

- **La fecha y la hora no son una herramienta.** Cada mensaje al cerebro empieza con una línea como `[Ahora: viernes 20 de noviembre de 2026, 17:43 (2026-11-20T17:43, hora local).]`. Cuesta unas veinte palabras por turno, no agrega una vuelta de herramienta (casi un segundo en voz) y es justo lo que el modelo necesita para calcular un «mañana a las ocho». Va en el mensaje y no en el prompt de sistema, así el caché del prompt sigue sirviendo. «¿Qué hora es?» lo sigue contestando el núcleo solo, sin cerebro.
- **La zona horaria es la de la Mac:** `TZ` si está, y si no `/etc/localtime`, leídas por la biblioteca de C (`localtime_r` y `mktime`). Sin dependencias nuevas.
- **Cómo se pide la hora.** El modelo pasa `en_segundos` (un entero) o `a_las` (hora local `AAAA-MM-DDTHH:MM`, con segundos opcionales, sin zona). Nada de palabras: el castellano lo entiende el modelo y la herramienta solo valida. Se rechaza lo que no tenga exactamente ese formato, una fecha que no existe, una hora que ya pasó (el error le dice al modelo qué hora es), una hora que el cambio de horario se saltea, más de un año, los dos a la vez o ninguno, un argumento que no conoce (`en_minutos`) y un texto de más de una línea, de más de 200 letras o que parezca una clave. Hasta 50 pendientes.
- **Cuando llega la hora, Wawi lo dice sola**: «Te aviso: sacar la ropa.» o «Terminó el temporizador de 10 minutos.». Pasa por el pedido de palabra del núcleo con prioridad alta: si está tranquila habla en el acto; si está hablando o pensando, espera a terminar; si el dueño está apretando la tecla, espera a que termine su turno. Nunca le habla encima. Sale en el globito de la mascota como cualquier frase suya, y en la terminal como `wawi (aviso)`. Si llega tarde (la Mac estaba dormida), lo dice: «Se me pasó la hora, era hoy a las 17:44: sacar la ropa.».
- **Se guardan en `~/.wawi2/reminders.json`** (o `$WAWI2_HOME`), así sobreviven a cerrar el programa. Lo que venció mientras estaba cerrado lo dice al arrancar, una sola vez: «Mientras no estaba se pasó un aviso: hoy a las 17:44, sacar la ropa.». Un aviso sale del archivo apenas se pide la palabra, así no se repite. Si el archivo no se entiende, lo deja aparte como `reminders.json.bad` y arranca sin avisos: no borra nada.
- **La memoria nueva le llega al cerebro sola**, en el bloque de memoria del turno siguiente (el que ya existía). Los datos se fechan con el día local, no con el UTC.

Con el cerebro real se probó así (20 de noviembre de 2026, `--voice --mute --input-wav`, Sonnet 5.5, con un `WAWI2_HOME` aparte): Paulina dice «Avisame en un minuto que saque la ropa», Parakeet oye «Avísame en un minuto que saque la ropa.», el modelo llama `avisame` con `{"en_segundos":60,"texto":"sacar la ropa"}` y contesta «Listo, en un minuto te aviso que saques la ropa.» (primer audio a los 2,6 s). El aviso quedó en `reminders.json`, y el simulador lo anunció al arrancar después de la hora.

Para verlos andar sin micrófono ni cerebro:

```sh
printf '/remind 60 sacar la ropa\n/reminders\n/tick 60000\n/done\n/remember Toma mate amargo\n/forget mate\n/quit\n' | cargo run -q -p wawi-app
```

## Plugins

Un plugin es un servidor MCP (local por stdio o remoto por HTTP) y una carpeta `~/.wawi2/plugins/<nombre>/` con `plugin.toml` y, si querés, `instrucciones.md`. La guía completa, para quien escribe uno, está en `docs/PLUGINS.md`.

- Sus herramientas le llegan al cerebro como `<plugin>__<herramienta>`, por el servidor `wawi`. `--allowedTools mcp__wawi` sigue siendo el único permiso.
- Cada herramienta es `read` o `act`. La iniciativa solo lee. Una `act` pregunta antes («¿Confirmás? Decime sí o no.»).
- Las claves y los permisos van al llavero de macOS, nunca a un archivo. `~/.wawi2/secrets.toml` sigue andando para lo viejo.
- Cada llamada queda en `~/.wawi2/logs/plugins.log`, sin argumentos, resultados ni secretos.

Hay dos clases de plugins:

- **Los de Wawi**: instinto, carácter, curiosidad y autoarreglo. Vienen con Wawi y arrancan apagados. Para prender uno, decile «Wawi, conectá el instinto». Wawi lo instala sola. Su programa (`wawi-instinto`, por ejemplo) tiene que estar junto a `wawi2` o en el PATH: si falta, Wawi te dice cómo compilarlo.
- **Los conectores**: servidores MCP remotos, como Linear. Ninguno viene instalado. Wawi trae una lista corta de sugeridos (`assets/catalogo/sugeridos.toml`) y también acepta cualquier dirección.

### Conectar Linear

1. Decile a Wawi: «Wawi, conectá Linear».
2. Wawi te abre Linear en el navegador. Entrá con tu cuenta y tocá «Permitir».
3. El navegador dice «Listo, ya podés volver a Wawi». Volvé.
4. Listo. Wawi avisa que quedó conectado y ya usa tus tareas.

No hay que copiar carpetas, ni escribir claves, ni hacer `chmod`. El permiso queda en el llavero de macOS y Wawi lo renueva sola. Si algún día Linear lo revoca, Wawi lo marca «desconectado»: decile «conectá Linear» otra vez.

Para otro servidor, decí su dirección: «conectá https://ejemplo.com/mcp». Wawi averigua si usa login (OAuth) o una clave. Si usa una clave, te abre una ventanita segura para pegarla. Nunca la digas en voz alta.

Para saber qué tenés, preguntá «¿qué plugins tenés?». La guía completa, con el campo `auth` y la API para una pantalla de Ajustes, está en `docs/PLUGINS.md`.

## Con el cerebro real

```sh
cargo run -p wawi-app -- --brain claude
```

Usa el `claude` instalado (`~/.local/bin/claude`, o el que diga `$WAWI2_CLAUDE_BIN`) con tu sesión de Claude Code, así que gasta de tu suscripción. Escribís y la respuesta aparece sola, frase por frase; acá la reproducción termina en el acto, no hace falta `/done`. `/stop` o escribir encima cortan el turno. Además de las herramientas de verdad están las de mentira (`clima`, `borrar`); el modelo las usa todas de verdad, y `borrar` pide confirmación igual que siempre. `WAWI2_CLAUDE_MODEL` y `WAWI2_CLAUDE_EFFORT` eligen modelo y esfuerzo (sin ellas, `claude-sonnet-5-5` y `low`), y `WAWI2_CLAUDE_TRACE=archivo` guarda cada línea que va y viene, con milisegundos. Las pruebas nunca llaman al `claude` real: usan un `claude` falso que habla el mismo protocolo. La única que lo llama está apagada y se corre a mano:

```sh
cargo test -p wawi-brain-claude --test real_cli -- --ignored --nocapture
```

El proceso corre aislado de tu configuración: en `~/.wawi2/claude`, sin las herramientas propias de Claude Code, sin tus servidores MCP, sin tus ajustes, hooks ni plugins, sin tu `CLAUDE.md` y sin skills. Estas son las opciones y para qué está cada una:

| Opción | Para qué |
| --- | --- |
| `-p --input-format stream-json --output-format stream-json --verbose` | Un proceso que no se cierra y habla JSON por línea, en los dos sentidos. |
| `--include-partial-messages` | El texto llega en pedacitos mientras se escribe, no al final. |
| `--tools ""` | Ninguna herramienta propia de Claude Code: solo las de Wawi. |
| `--strict-mcp-config --mcp-config '{"mcpServers":{"wawi":{"type":"sdk",...}}}'` | El único servidor MCP es el de Wawi, servido por el mismo programa a través del protocolo de control. |
| `--allowedTools mcp__wawi --permission-mode dontAsk` | Las herramientas de Wawi no pasan por los permisos de Claude Code (los decide el núcleo) y cualquier otra cosa se rechaza sin preguntar. |
| `--setting-sources=` | No carga ajustes de usuario, proyecto ni locales: ni tu `CLAUDE.md`, ni hooks, ni plugins. |
| `--disable-slash-commands`, `--no-chrome` | Sin skills ni Chrome. |
| `--system-prompt ...` | Reemplaza el prompt de programador por el de Wawi: reglas para hablar y alma. Los datos de la memoria no van acá: van al principio del mensaje del turno, solo cuando cambiaron, así el prompt no cambia y el caché sirve. |
| `--system-prompt-snapshot off` | Si cambia el alma y el proceso se reemplaza, la sesión retomada usa el prompt nuevo. |
| `--model claude-sonnet-5-5 --effort low` | El modelo y el esfuerzo para la voz (ver `docs/LATENCIA.md`). Se cambian con `WAWI2_CLAUDE_MODEL` y `WAWI2_CLAUDE_EFFORT`. |
| `--resume ID` | Solo al reemplazar el proceso: sigue la misma conversación. |

Además se borran del entorno `CLAUDECODE`, `CLAUDE_CODE_*` y `ANTHROPIC_*`, para que no se cuele una clave de API ni la configuración de una sesión de Claude Code que esté corriendo a Wawi.

## Reconocimiento de voz con Parakeet

`crates/wawi-stt-parakeet` implementa `Stt` con [Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) de NVIDIA, en la versión ONNX cuantizada a int8 que hizo istupakov y que también usa Handy. Corre en la CPU con ONNX Runtime, a través del crate `transcribe-rs`. ONNX Runtime viene precompilado y queda adentro del binario, así que sirve igual para Mac, Windows y Linux. El idioma lo detecta el modelo solo, sin pista. Por eso hay un filtro barato (`language.rs`): si la transcripción parece francés, inglés, italiano, portugués o alemán, y no castellano, puede salir como `UnclearTranscript` y Wawi contesta «No te entendí, ¿me lo repetís?» sin llamar al cerebro. Se prende con `ParakeetStt::set_language_guard(true)`. Con las 22 tomas del dueño (`~/Desktop/wawi-voz`) no rechaza ninguna, y con 7 frases dichas por voces de macOS en esos cinco idiomas las rechaza todas:

```sh
WAWI2_STT_GUARD_DIR=~/Desktop/wawi-voz cargo test --release -p wawi-stt-parakeet -- --ignored --nocapture guard
```

El modelo no está en el repo. Se baja una vez a `~/.wawi2/models/parakeet-tdt-0.6b-v3-int8/` (o a `$WAWI2_HOME/models/...`), unos 670 MB, y se revisa el tamaño y el SHA-256 de cada archivo:

- Fuente: <https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx>, fijada en la revisión `8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce`.
- Licencia: CC-BY-4.0 (el modelo original es de NVIDIA, con la misma licencia). Hay que dar el crédito si se distribuye.

```sh
cargo run --release -p wawi-stt-parakeet --bin stt-bench -- download
cargo run --release -p wawi-app -- --stt parakeet      # lo baja solo si falta
```

En el simulador, `/hear archivo.wav` hace de cuenta que el dueño dijo eso: abre el tramo, le pasa el audio a Parakeet y lo cierra. Lo escrito sigue pasando por la transcripción de mentira. Sirve con `--brain claude` también. Cargar el modelo tarda alrededor de 1 s y ocupa unos 1,5 GB de memoria.

Las pruebas no necesitan el modelo. La única que lo usa está apagada:

```sh
cargo test --release -p wawi-stt-parakeet -- --ignored --nocapture
```

### Medir

`stt-bench` mide velocidad (RTF), demora por frase, WER, memoria pico y tiempo de carga sobre una carpeta de pares `nombre.wav` + `nombre.txt` (el `.txt` es lo que se dijo de verdad). El WER se calcula en minúsculas, sin signos y con tildes ("sí" no es "si"). Los números no se pasan a palabras: si alguien dijo "siete" y sale "7", cuenta como error.

```sh
# 22 frases de charla con un asistente, con las voces en español de macOS, limpias y con ruido rosa a 15 dB
cargo run --release -p wawi-stt-parakeet --bin stt-bench -- make-synthetic --out /tmp/stt-set
cargo run --release -p wawi-stt-parakeet --bin stt-bench -- run /tmp/stt-set/clean /tmp/stt-set/noisy --json parakeet.json
# varios resultados lado a lado, con el mismo puntaje
cargo run --release -p wawi-stt-parakeet --bin stt-bench -- report parakeet.json whisper.json
```

Las frases están en `crates/wawi-stt-parakeet/bench/phrases.tsv`. **Las voces son sintéticas**, y mexicanas o españolas: macOS no trae una voz rioplatense. El WER que importa sale con tu voz. Para eso, grabá una carpeta con pares `nombre.wav` + `nombre.txt` (cualquier frecuencia y cantidad de canales; se convierte a 16 kHz mono) y corré `stt-bench run TU_CARPETA`. En la Mac, grabá cada frase con QuickTime (Archivo > Nueva grabación de audio) y convertila con `afconvert`, que viene con macOS:

```sh
mkdir mis-frases && cd mis-frases
echo "Che, acordate de comprar pan." > 01_pan.txt
afconvert -f WAVE -d LEI16@16000 -c 1 ~/Desktop/Grabación.m4a 01_pan.wav
```

Conviene grabar como hablás con Wawi: frases cortas, la distancia de siempre al micrófono y algo de ruido de casa. Con 20 o 30 frases alcanza para comparar.

Primera medición (M4 Pro, 20 de noviembre de 2026, 22 frases sintéticas × 2), contra el Whisper large-v3-turbo de Wawi 1 por mlx-whisper en la GPU, con su idioma (`es`) y su frase semilla, sin el filtro de basura que Wawi 1 aplica después:

| | Parakeet v3 int8, CPU | Whisper large-v3-turbo, GPU (Wawi 1) |
| --- | --- | --- |
| Demora p50 / p90, frases de 2 a 5 s | 86 / 111 ms | 505 / 523 ms |
| Demora p50, frases de menos de 2 s | 55 ms | 486 ms |
| RTF | 0,028 | 0,147 |
| WER, limpio | 19,7 % | 10,1 % |
| WER, con ruido a 15 dB | 35,9 % | 12,1 % |
| Carga | 1,0 s | 0,8 s |
| Memoria | 1,6 GB RSS | 2,4 GB pico de MLX |

Parakeet contesta seis veces más rápido y sin GPU, pero se equivoca el doble con voz limpia y el triple con ruido. Además pasa el voseo a tuteo ("tenés" → "tienes", "podés" → "puedes") y pierde tildes del imperativo ("poné" → "pone"). Con ruido, dos frases le salieron vacías. Antes de decidir hay que medir con tu voz.

## La voz con Kokoro

`crates/wawi-tts-kokoro` implementa `Tts` con [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) v1.0, el mismo modelo y la misma voz de Wawi 1, con los ajustes de su `private/config.yaml`: `jf_alpha` (la voz japonesa de Kokoro hablando castellano, la "voz de cachorra"), velocidad 1,05 y tres semitonos más aguda. Corre en la CPU con ONNX Runtime, con el mismo `ort` que Parakeet, así que el binario lleva un solo ONNX Runtime. Sale a 24 kHz, mono.

El modelo no está en el repo. Se baja una vez a `~/.wawi2/models/kokoro-82m-v1.0/` (o a `$WAWI2_HOME/models/...`), unos 330 MB, y se revisa el tamaño y el SHA-256 de cada archivo:

- Fuente: <https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX>, fijada en la revisión `1939ad2a8e416c0acfeecc08a694d14ef25f2231`: `onnx/model.onnx` (fp32) y las voces `jf_alpha` y `ef_dora`. Con los mismos fonemas da exactamente el mismo audio que el `kokoro-v1.0.onnx` de Wawi 1.
- Licencia: Apache-2.0.

```sh
cargo run --release -p wawi-tts-kokoro --bin tts-say -- download
cargo run --release -p wawi-tts-kokoro --bin tts-say -- --play "¿Querés que te lea las noticias?"
cargo run --release -p wawi-tts-kokoro --bin tts-say -- --bench --out hola.wav "Hola, ¿cómo andás?"
```

`tts-say` escribe un WAV (`--out`, por defecto `tts-say.wav`) y con `--play` lo hace sonar con `afplay`. `--accent rioplatense` cambia a la pronunciación de acá, `--voice ef_dora` cambia la voz, `--speed` y `--pitch` cambian velocidad y tono, y `--phonemes` muestra los fonemas sin sintetizar.

**Pronunciación.** Kokoro no lee letras, lee fonemas. Wawi 1 los sacaba de espeak-ng, que es GPL-3: meterlo en el binario obligaría a publicar todo Wawi bajo GPL. Wawi 2 tiene su propio conversor, con reglas (`src/g2p`), porque la ortografía del castellano es muy regular. Usa los mismos símbolos y convenciones que espeak-ng, que es con lo que se entrenó Kokoro, y con el acento castellano da lo mismo que espeak-ng en 93 de 99 frases de prueba (0,2 % de símbolos distintos). Por defecto habla como Wawi 1, que decía la z como en España y la ll como "li": al dueño le gusta ese tono. Con `--accent rioplatense` usa "s" para la z y la c, y "sh" para la ll y la y ("yo" suena "sho"). Lee números, horas, fechas, plata, porcentajes, ordinales, unidades, abreviaturas comunes (Sr., Dra., etc.), siglas (USB letra por letra, NASA como palabra) y unas sesenta palabras en inglés como se dicen acá (WhatsApp, YouTube, mail, link, wifi). Las demás palabras en inglés se leen con reglas del castellano.

Primera medición (M4 Pro, 20 de noviembre de 2026), con la misma voz, la misma velocidad y el mismo tono, contra el kokoro-onnx de Wawi 1:

| | Wawi 2 | Wawi 1 |
| --- | --- | --- |
| Carga | 0,3 s, más 0,18 s de precalentado | 0,3-0,4 s, más 1,3 s de precalentado |
| Primera frase después de cargar (de 0,7 a 1,8 s de audio) | 180-370 ms | 770-830 ms (`tts_first_synth` en el log: ~1,19 s) |
| Frases siguientes de ~1,6 s | ~350 ms | ~350 ms |
| RTF, frases siguientes | 0,15-0,22 | 0,20-0,24 |
| Memoria | 600-720 MB RSS | ~610 MB RSS |

Ya caliente, la velocidad es la misma (es el mismo modelo en el mismo ONNX Runtime); la diferencia está en la primera frase. Para que la primera palabra salga rápido, una primera frase de más de 60 caracteres se corta en su primera coma o dos puntos: "Tenés 3 reuniones hoy:" suena a los ~350 ms y el resto ya está listo antes de que termine. Pasadas por Parakeet, las muestras se entienden bien, incluidos WhatsApp, YouTube y Spotify.

Las pruebas no necesitan el modelo. La única que lo usa está apagada:

```sh
cargo test --release -p wawi-tts-kokoro -- --ignored --nocapture
```

## Prueba de cancelación de eco (solo Mac)

`aec-test` mide si la cancelación de eco de macOS saca la voz de Wawi del micrófono. Es un banco de pruebas aparte.

Antes de correrla: sin auriculares, con el volumen fijo y el ambiente en silencio, sin hablar ni tocar la compu durante los 30 s que dura. Si no, los números no sirven. El programa avisa cuando detecta alguno de estos problemas.

```sh
cargo run -p wawi-audio-mac --bin aec-test
```

Suena tres veces una frase corta: con la cancelación apagada (A), prendida (B) y prendida en bypass, como control (C). Al final imprime una tabla y deja los WAV en una carpeta temporal, o en la que elijas con `--out DIR`.

Para saber si tu voz sobrevive, que solo se puede juzgar de oído:

```sh
cargo run -p wawi-audio-mac --bin aec-test -- --talk-over
```

Cuando aparezca "hablá ahora", hablá normal hasta que aparezca "listo", y escuchá `talk_over.wav`. Tu voz tiene que oírse igual de clara con la frase sonando y sin ella.

`--mute` hace el recorrido a -80 dB, sin que se oiga. Sirve para probar el programa y para medir el ambiente sin eco, y no mide cancelación.

Si falta el permiso del micrófono, el programa lo dice y sale sin hacer sonar nada. El permiso se le da a la app desde la que corrés esto, por ejemplo la terminal.

## Hablar con la tecla (solo Mac)

`ptt-test` prueba el micrófono, la tecla y la reproducción juntos, sin el núcleo:

```sh
cargo run -p wawi-audio-mac --bin ptt-test
```

Mantené apretada la ⌘ derecha, hablá y soltala: te devuelve lo que dijiste e imprime cuánto duró y a qué nivel llegó. Si volvés a apretar mientras suena, la corta en el acto (es lo mismo que va a pasar cuando interrumpas a Wawi). Un toque de menos de 250 ms no manda nada. Enter para salir.

Necesita dos permisos, y los dos se le dan a la app desde la que lo corrés (Terminal, iTerm, tu editor), no al programa: **Micrófono** y **Monitoreo de entrada** (o Accesibilidad), en Ajustes del Sistema > Privacidad y seguridad. Si falta alguno, el programa dice cuál y qué app habilitar, y sale sin hacer sonar nada. Después de habilitarlo hay que cerrar la app del todo y volver a abrirla.

El programa nunca toca el volumen del sistema. Igual lo lee al empezar, al arrancar el audio y mientras corre, y avisa si cambia, para encontrar la causa del salto de volumen que se vio una vez.

Cada tramo arranca con los 400 ms de antes de apretar y sigue 300 ms después de soltar, así que podés empezar a hablar mientras apretás y soltar sobre la última sílaba. El silencio de las puntas se recorta.

`voice-record` te guía por las frases de prueba del STT y guarda cada toma como `nombre.wav` + `nombre.txt` en `~/Desktop/wawi-voz` (u otra carpeta, como primer argumento). Para rehacer solo algunas, pasá sus números o nombres; la toma vieja se mueve a `anteriores/`, no se pisa:

```sh
cargo run -p wawi-audio-mac --bin voice-record -- ~/Desktop/wawi-voz 3 5 19_luces
```

La arquitectura está en [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Licencia

El código está bajo la [Licencia Apache 2.0](LICENSE): podés usarlo, cambiarlo y distribuirlo, también en proyectos comerciales, siempre que conserves el aviso de copyright y el archivo [NOTICE](NOTICE). Incluye una licencia de patentes.

Los dibujos de Wawi (`assets/mascotas/wawi/`) no entran en esa licencia: son CC BY-NC 4.0, solo para uso no comercial y dando crédito ([LICENSE-art.md](assets/mascotas/wawi/LICENSE-art.md)). Los modelos de voz que se descargan tienen sus propias licencias, indicadas arriba.
