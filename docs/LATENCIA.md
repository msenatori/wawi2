# Latencia del cerebro

En la primera sesión en vivo (20 de noviembre de 2026, con Opus 5.5 por defecto) el silencio que esperaba el dueño fue de 2,9 a 4 s por turno con el cerebro: 0,4 a 0,6 s de transcripción (con los 300 ms de cola al soltar la tecla), 2,5 a 3,3 s hasta la primera frase y 0,35 a 0,7 s más hasta el primer audio. Casi todo era el modelo. Wawi 1 andaba en 5,45 s (mediana).

## Cómo se midió

`crates/wawi-brain-claude/examples/voice_bench.rs` corre el `claude` real a través de `ClaudeBrain`, con las mismas opciones de aislamiento que usa Wawi, la memoria armada por `wawi-memory` (el alma inicial y dos datos: el dueño se llama Marcos y vive en Buenos Aires) y una herramienta `clima` de mentira. El proceso arranca antes (`prewarm`), hace un turno de calentamiento que no se cuenta y después ocho turnos cortos en castellano: un saludo, un dato, una cuenta chica, una opinión, una pregunta que depende de la anterior, una que usa la memoria, una que necesita la herramienta y un pedido de respuesta larga.

```sh
cargo run --release -p wawi-brain-claude --example voice_bench -- ETIQUETA [MODELO] [ESFUERZO]
cargo run --release -p wawi-brain-claude --example voice_bench -- replay traza.log ...
```

Sin modelo ni esfuerzo usa los de Wawi; `-` deja que decida el CLI. `replay` vuelve a pasar los pedacitos de texto de una traza por los dos cortes (frases enteras y el de ahora) sin gastar nada. Los tiempos se cuentan desde que el turno le llega al cerebro, no desde que se suelta la tecla: a todos hay que sumarles la transcripción y la síntesis.

- **Primer texto:** llega el primer pedacito de texto del modelo.
- **Primer trozo:** sale el primer `BrainText`, lo primero que se manda a hablar, con el corte de ahora (ver más abajo).
- **Total:** termina el turno.

## Resultados

M4 Pro, 20 de noviembre de 2026, Claude Code 2.1.329. Mediana de los seis turnos cortos (sin la herramienta ni la respuesta larga). Opus por defecto son tres corridas; Sonnet con `low`, tres; los demás, una. El costo es el que informa el CLI para toda la corrida (nueve turnos, calentamiento incluido), en dólares de API: con la suscripción no se paga aparte, pero sirve para comparar.

| Modelo y esfuerzo | Primer texto | Primer trozo | Total | Con herramienta (primer trozo) | Larga (primer trozo / total) | Costo de la corrida |
| --- | --- | --- | --- | --- | --- | --- |
| Opus 5.5, por defecto (lo que se usaba) | 3,98 s | 4,17 s | 4,69 s | 8,57 s | 5,88 s / 14,1 s | 0,040 a 0,056 |
| Opus 5.5, `medium` | 3,84 s | 3,93 s | 4,52 s | 7,16 s | 4,51 s / 13,2 s | 0,045 |
| Opus 5.5, `low` | 2,12 s | 2,18 s | 2,61 s | 7,40 s | 5,53 s / 13,9 s | 0,051 |
| Sonnet 5.5, por defecto | 0,62 s | 0,83 s | 1,07 s | 2,27 s | 1,46 s / 9,0 s | 0,028 |
| **Sonnet 5.5, `low`** (el elegido) | **0,61 s** | **0,96 s** | **1,08 s** | **2,13 s** | **0,97 s / 9,4 s** | **0,023** |
| Haiku 5.5, por defecto | 0,63 s | 0,86 s | 1,01 s | 1,62 s | 3,03 s / 4,9 s | 0,002 |

Lo que más pesa es el modelo, no el proceso: Opus tarda de 2 a 4 s en escribir la primera letra (piensa aunque no se vea; con `low` piensa menos), y Sonnet y Haiku, 0,6 s. Sonnet por defecto y con `low` andan igual de rápido; con `low` no piensa nada y sale un poco más barato. En el turno con herramienta, Opus tarda 7 a 8,5 s y Sonnet 2,1 s.

## Cómo contestan

Todos hablan de vos, corto y sin markdown, y todos acertaron la capital, la cuenta y el nombre. Las diferencias:

| Turno | Opus 5.5 por defecto | Sonnet 5.5 `low` | Haiku 5.5 |
| --- | --- | --- | --- |
| Hola Wawi, ¿cómo andás? | ¡Hola, Marcos! Bien por acá, ¿y vos? ¿Ya tenés el mate en la mano? | ¡Hola, Marcos! Todo bien por acá, listo para ayudarte. ¿Y vos, cómo venís con el día? ¿Ya arrancaste con el mate? | ¡Hola, Marcos! Ando bien, gracias por preguntar. ¿Y vos, cómo andás? ¿En qué te puedo ayudar? |
| ¿Cuál es la capital de Australia? | Es Canberra. Mucha gente piensa que es Sídney, pero no. | Canberra. Mucha gente cree que es Sídney o Melbourne, pero la capital es Canberra. | La capital de Australia es Canberra, no Sídney como mucha gente cree. |
| ¿Y vos con qué lo acompañarías? (después del mate amargo) | Con unas medialunas de manteca, que con el amargo van bárbaro. Y si es a la tarde, unas tostadas con manteca y dulce de leche. | Con unas facturas, sobre todo medialunas de manteca, que con el amargo hacen una dupla bárbara. Si querés algo más simple, una tostada con queso crema también va muy bien. | Como no tomo mate, la verdad es que no tengo estómago para elegir, pero si me dejás opinar, lo acompañaría con unos bizcochitos de manteca o un pedazo de queso y dulce. […] |
| ¿Va a llover mañana en Buenos Aires? (con la herramienta) | Sí, Marcos, mañana a la tarde hay un ochenta por ciento de probabilidad de lluvia y va a hacer diecinueve grados. Llevá paraguas si salís. | Sí, Marcos, mañana hay un ochenta por ciento de probabilidad de lluvia por la tarde, con unos diecinueve grados. Llevate el paraguas si salís. | Sí, mañana hay bastante probabilidad de lluvia en Buenos Aires, alrededor del 80 por ciento, sobre todo por la tarde. La máxima va a rondar los 19 grados. […] |

- **Opus** es el más natural y el más corto, pero es tres a cuatro veces más lento para empezar.
- **Sonnet** suena casi igual de bien. Nombra un poco más al dueño y a veces agrega un dato que no le pidieron ("y ya sé que vivís en Buenos Aires"). En una respuesta larga escribió "1880" y en otra "19 grados" con números: Kokoro igual los lee bien.
- **Haiku** es igual de rápido pero se va más de tema y alarga (la respuesta de seguimiento, cuatro líneas; la de la memoria, una pregunta de más), y escribe los números con cifras más seguido, contra la regla del estilo. Su respuesta larga es corta para lo que se pidió.

## Lo elegido

`claude-sonnet-5-5` con esfuerzo `low` (`DEFAULT_MODEL` y `DEFAULT_EFFORT` en `wawi-brain-claude`): el primer trozo sale en menos de un segundo en vez de cuatro, contestando casi tan bien como Opus. Con la transcripción (0,4 a 0,6 s) y el primer audio (0,35 a 0,7 s), el silencio esperado baja de 2,9 a 4 s a alrededor de 1,8 a 2,3 s. `WAWI2_CLAUDE_MODEL` y `WAWI2_CLAUDE_EFFORT` lo siguen cambiando (por ejemplo `WAWI2_CLAUDE_MODEL=claude-opus-5-5`).

## Hablar antes: el primer trozo

Antes, el adaptador soltaba solo oraciones enteras, y la primera esperaba a que el modelo la terminara. Ahora el primer trozo de cada turno puede salir antes:

- en una coma, punto y coma o dos puntos seguidos de un espacio, o justo antes de " y " o " pero ", si ya tiene 30 letras o más;
- en un punto, signo o puntos suspensivos al final de lo que llegó, sin esperar el espacio que viene después.

Nunca corta entre números ("3,5", "7:30", "el 3, 4 y 5"), ni después de una abreviatura o una inicial ("Dra. Fernández", "J. R. R. Tolkien", "etc." seguido de minúscula), ni adentro de una pregunta o exclamación abierta. Los trozos siguientes son oraciones enteras, que suenan mejor. Estas reglas de abreviaturas valen ahora para todos los cortes, no solo para el primero.

Ganancia, repasando las trazas de las corridas de arriba: en la mitad de los turnos (28 de 56) el primer trozo sale antes, en promedio 0,2 s antes en esos turnos (hasta 0,84 s); en promedio de todos, 0,1 s. Con Sonnet `low`, la mediana del primer trozo bajó de 1,00 s a 0,96 s; con Sonnet por defecto, de 1,04 s a 0,83 s. Ayuda sobre todo en las opiniones y las respuestas que arrancan con una oración larga ("Yo lo acompañaría con unas facturas," salió 0,27 s antes; "Y mirá, el mate amargo es el clásico de los que lo toman en serio:", 0,43 s). Las respuestas que arrancan con una oración corta ("¡Hola, Marcos!", "Canberra.") no cambian.

## Lo que se gastó

Las corridas de este documento costaron unos 0,35 dólares según el CLI (con la suscripción del dueño), contando tres corridas que salieron con el modelo por defecto por un error de la línea de comandos y que quedaron como las tres corridas de Opus por defecto.
