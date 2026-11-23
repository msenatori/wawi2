# Cómo hacer una mascota para Wawi

Wawi es una perrita dibujada a mano. Pero el programa no está atado a ella. Podés hacer otra mascota, por ejemplo una gata que se llame Kitty, con su nombre, sus dibujos y sus gestos. No hace falta saber programar ni tocar el código.

Esta guía es para artistas. Explica todo paso a paso.

## La idea en pocas palabras

- Una mascota es una carpeta.
- Adentro hay dibujos en PNG y una ficha de texto, `mascota.toml`.
- La ficha dice cómo se llama, quién la dibujó, qué licencia tiene y qué dibujos usar en cada momento.
- Wawi lee la carpeta. Si algo está mal, avisa y sigue con Wawi.

Las mascotas que instalás van en `~/.wawi2/mascotas/<id>/`. Por ejemplo, Kitty va en `~/.wawi2/mascotas/kitty/`. Wawi misma viene adentro del programa, en `assets/mascotas/wawi/`, y sirve de ejemplo completo.

## Paso a paso

### 1. Crear la carpeta con dibujos de ejemplo

```
mascota nueva kitty --nombre Kitty
```

Esto crea `~/.wawi2/mascotas/kitty/` con:

- `mascota.toml`, la ficha, con un comentario en cada línea.
- `cuadros/`, con dibujos de ejemplo: un bichito redondo con orejas.
- `barra.png`, el ícono para la barra de menú.
- `LICENCIA-arte.md`, para que escribas de quién es el arte.

Si querés crearla en otro lugar, usá `--en CARPETA`.

### 2. Mirarla

```
mascota ver ~/.wawi2/mascotas/kitty
```

Se abre la mascota en pantalla y pasa por todos sus estados y acciones. Para terminar, apretá Ctrl+C o elegí «Salir» en su menú.

Para ver una sola cosa, agregá el nombre:

```
mascota ver ~/.wawi2/mascotas/kitty speaking
mascota ver ~/.wawi2/mascotas/kitty saludar
```

Para guardar una foto de cómo se ve, agregá `--foto kitty.png`. La foto se toma al segundo y medio. Con `--cuando 3` se toma a los 3 segundos.

Si tu mascota tiene cuerpo (más abajo), también podés mirar:

```
mascota ver ~/.wawi2/mascotas/kitty caer
mascota ver ~/.wawi2/mascotas/kitty pasear
```

### 3. Reemplazar los dibujos

Abrí `cuadros/` y reemplazá cada PNG por el tuyo, con el mismo nombre. Después volvé a mirarla. No hace falta reiniciar nada: `mascota ver` lee los dibujos cada vez.

Podés agregar dibujos nuevos y nombrarlos en la ficha.

### 4. Revisarla

```
mascota validar ~/.wawi2/mascotas/kitty
```

Te dice, en castellano:

- si la ficha tiene un error, y en qué línea;
- si falta un dibujo;
- si un dibujo tiene otro tamaño;
- si un dibujo no tiene transparencia;
- si los fps están fuera de rango;
- qué estados tiene y cuáles usan otro en su lugar;
- si una animación no se usa nunca;
- si falta la licencia o el autor.

Los **errores** hay que arreglarlos. Los **avisos** son consejos: la mascota anda igual.

### 5. Elegirla

En el menú de Wawi (en la barra de menú, o con clic derecho sobre la mascota) está «Mascota». Ahí aparecen todas las mascotas instaladas. Elegí la tuya y cambia al instante, sin reiniciar.

Wawi se acuerda de la elección en `~/.wawi2/mascot.json`, en la línea `"mascota": "kitty"`. Para volver a Wawi, elegí «Wawi» en el mismo menú.

Para probar una mascota sin instalarla:

```
wawi2 --ui-demo --mascota CARPETA
```

O con la variable `WAWI2_MASCOTA=CARPETA`, que vale también para `wawi2 --voice`.

## Los dibujos

### Tamaño

- Todos los cuadros de una mascota miden lo mismo, en píxeles. Ese tamaño va en `[cuadro]`.
- Con 160 a 400 píxeles de lado alcanza. Wawi usa 320 × 286.
- En pantalla, la mascota mide unos 120 puntos de ancho. Lo ajustás con `escala`.
- Si un cuadro tiene otro tamaño, se estira para que entre. El validador avisa.

### Transparencia

- Usá PNG con fondo transparente. Si el PNG no tiene transparencia, se ve un rectángulo alrededor.
- Dejá un poco de aire alrededor del dibujo, sobre todo arriba. Ahí va el globito de estado.
- Apoyá a la mascota siempre en el mismo lugar del cuadro: abajo y al medio. Así no "salta" entre un cuadro y otro.

### Fps (cuadros por segundo)

- 6 a 12 fps se ve bien para la mayoría de las cosas.
- Para respirar o dormir, mucho menos: un cuadro cada uno o dos segundos.
- Si querés tiempos distintos por cuadro, usá `duraciones`.
- El máximo es 60.

### Tres maneras de dar los cuadros

Cada animación usa una de estas tres:

1. **Una lista de archivos** de la carpeta `cuadros/`:

   ```toml
   cuadros = ["idle_a", "idle_b"]
   ```

   Sin el `.png`. Un mismo archivo puede repetirse.

2. **Una carpeta entera**, en orden por nombre (`paso1`, `paso2`, …, `paso10`):

   ```toml
   secuencia = "caminar"
   ```

   Lee todos los PNG de `cuadros/caminar/`. Sirve si tu programa de dibujo exporta una carpeta por animación.

3. **Una hoja** (sprite sheet): una sola imagen con los cuadros en grilla, de izquierda a derecha y de arriba abajo.

   ```toml
   hoja = "festejar.png"
   columnas = 4
   filas = 1
   ```

   Cada celda mide lo que dice `[cuadro]`. Con `total` usás menos celdas que las de la grilla. Con `orden = [0, 1, 2, 1]` elegís el orden (se cuenta desde 0).

## Los estados

Un estado es lo que la mascota muestra según lo que pasa en la charla. Cada estado usa la animación que tiene su mismo nombre. Solo `idle` es obligatorio.

| Estado | Cuándo se ve | Si falta, usa |
|---|---|---|
| `idle` | Quieta, sin hacer nada. **Obligatorio.** | — |
| `sleeping` | Dormida, después de un rato quieta. | `idle` |
| `listening` | Mientras le hablás (la tecla apretada). | `idle` |
| `transcribing` | Entendiendo lo que dijiste. | `thinking`, y si no, `idle` |
| `thinking` | Pensando la respuesta. | `idle` |
| `speaking` | Hablando. | `idle` |
| `interrupted` | La interrumpiste. Se ve una vez. | `error`, y si no, nada |
| `error` | Algo falló. Se ve una vez. | nada |
| `muted` | Con el micrófono apagado. | nada |

Así, una mascota con un solo dibujo en `idle` ya funciona.

Si tu animación tiene otro nombre, conectala en `[estados]`:

```toml
[estados]
thinking = "pensar"
```

Detalles:

- `interrupted` y `error` se juegan una vez y vuelven al estado de antes.
- `muted`: si se repite (`repetir = true`), reemplaza a `idle` mientras no escucha. Si es de una vez, la hace seguido, como un gesto.
- `speaking` puede mover la boca al ritmo de la voz. Poné tres cuadros en `boca`: cerrada, media y abierta.
- Sobre la mascota aparecen solos un puntito verde (escuchando), tres puntitos (entendiendo) y un ícono de micrófono apagado. No hace falta dibujarlos.

## Las acciones

Una acción es un gesto con nombre, como `saludar`, `festejar`, `cafe` o `bostezo`. Otras partes de Wawi pueden pedirlo por su nombre.

```toml
[acciones]
saludar = "saludar"     # nombre de la acción = animación que juega
cafe = "taza"
```

- Se juega una vez, encima de lo que esté haciendo, y después sigue.
- Usá nombres en minúscula y sin espacios.
- Poné `descripcion` en la animación: dice qué hace, en pocas palabras.
- Wawi trae: saludar, festejar, bailar, cafe, bostezo, estirarse, rascarse, mover_la_cola, ladrar, estornudar y reirse.

Hoy las acciones se ven con `mascota ver`. Más adelante Wawi va a poder hacerlas sola cuando charla (una herramienta `hacer(accion)`).

## Su vida mientras está quieta

En `[vida]` decís qué hace sola:

```toml
[vida]
parpadeo = "parpadeo"      # un parpadeo cada tanto
cada_parpadeo = [2, 6]     # entre 2 y 6 segundos
gestos = ["mirar", "rascarse"]   # cada tanto hace uno al azar
cada_gesto = [6, 20]
dormir_tras = 10           # segundos quieta hasta dormirse (0 = nunca)
dormirse = "bostezo"       # lo que hace antes de dormirse
despertarse = "estirarse"  # lo que hace al despertarse con un clic
toque = "saltito"          # lo que hace si le hacés clic
balanceo = true            # se mece un poco al hablar y al pensar
```

Todo es opcional. El balanceo lo hace el programa, encima de tus dibujos. Si tus dibujos ya se mueven solos, poné `balanceo = false`.

Un gesto no se repite dos veces seguidas. Y si tiene otros para elegir, tampoco repite uno que hizo en el último minuto. Así nunca entra en un ciclo.

## Darle un cuerpo: caer, caminar, trepar

Con `[cuerpo]`, tu mascota siente la gravedad:

- Si la agarrás con el mouse y la soltás en el aire, cae.
- Aterriza en el piso de la pantalla (arriba del Dock) o arriba de una ventana.
- Si la ventana donde está sentada se mueve o se cierra, se cae.
- Cada tanto se levanta y camina a otro lugar. A veces trepa por el costado de una ventana y se sube arriba.
- A veces baja de una ventana alta en paracaídas.

Sin `[cuerpo]`, la mascota se queda donde la ponés, sin gravedad, como siempre.

### Las animaciones del cuerpo

Cada una es el nombre de una animación tuya. Todas son opcionales.

```toml
[cuerpo]
mira = "izquierda"        # hacia dónde mira tu dibujo
caminar = "caminar"
trotar = "trotar"
trepar = "trepar"
caer = "caer"
aterrizar = "aterrizar"
porrazo = "porrazo"
paracaidas = "paracaidas"
aterrizar_paracaidas = "plegar"
posada = "sentada_borde"
```

| Clave | Cuándo se ve | Si falta |
|---|---|---|
| `mira` | Hacia dónde mira el dibujo: `"izquierda"` o `"derecha"`. Cuando camina para el otro lado, el programa la da vuelta. | `"izquierda"` |
| `caminar` | Mientras camina. Se repite. | No pasea: solo cae cuando la soltás. |
| `trotar` | Un paseo largo, más rápido. | `caminar` |
| `trepar` | Subiendo por el costado de una ventana. | No trepa. |
| `caer` | Mientras cae. También mientras la tenés agarrada. | Se ve quieta (`idle`). |
| `agarrada` | Mientras la tenés agarrada con el mouse. | `caer` |
| `aterrizar` | Al tocar el suelo. Se juega una vez. | Un aplastón que hace el programa. |
| `porrazo` | Al caer de muy alto (ver `porrazo_desde`). | `aterrizar` |
| `paracaidas` | Bajando en paracaídas. | `caer` |
| `aterrizar_paracaidas` | Al llegar abajo con el paracaídas. | `aterrizar` |
| `posada` | Quieta arriba de una ventana, en vez de `idle`. | `idle` |

Consejos:

- `caminar`, `trotar`, `trepar`, `caer` y `paracaidas` tienen que repetirse (no pongas `repetir = false`).
- `aterrizar`, `porrazo` y `aterrizar_paracaidas` se juegan una vez. Mientras duran, la mascota no se mueve.
- Dibujá un solo sentido. El programa la da vuelta.
- Para un paracaídas o algo que lleva encima, usá `capa` (más abajo).

### Los números

Todos son opcionales. Si no los ponés, usa los de Wawi.

| Clave | Qué es | Wawi |
|---|---|---|
| `gravedad` | Cuánto acelera al caer, en puntos por segundo al cuadrado. | `1500` |
| `velocidad_maxima` | Más rápido que esto no cae (puntos por segundo). | `950` |
| `velocidad_caminar` | Puntos por segundo caminando. | `42` |
| `velocidad_trote` | Puntos por segundo trotando. | `130` |
| `velocidad_trepar` | Puntos por segundo trepando. | `30` |
| `velocidad_paracaidas` | Puntos por segundo bajando en paracaídas. | `75` |
| `rebote` | Cuánto rebota al caer, de `0` a `0.9`. `0` = no rebota. | `0` |
| `aterrizaje` | Segundos del aplastón al aterrizar. | `0.35` |
| `porrazo_desde` | Desde qué altura (en puntos) el aterrizaje es un porrazo. `0` = nunca. | `500` |
| `chance_paracaidas` | De `0` a `1`: cuántas veces baja en paracaídas de una ventana alta. | `0.35` |
| `chance_trote` | De `0` a `1`: cuántos paseos largos son al trote. | `0.1` |
| `cuanto_camina` | Minutos entre paseos, `[mínimo, máximo]`. | `[8, 25]` |
| `cada_trepada` | Minutos de espera entre dos trepadas. | `30` |
| `cada_paracaidas` | Minutos de espera entre dos paracaídas. | `10` |
| `cada_trote` | Minutos de espera entre dos trotes. | `5` |

Consejos:

- Con 1500 de gravedad, caer media pantalla tarda menos de un segundo. Más gravedad, más pesada se siente.
- Caminando, que vaya más lento que el cursor. Así no distrae.
- Un poco de rebote (`0.2` o `0.3`) va bien para una pelota o un gato liviano. Wawi no rebota: se aplasta.
- Trepar, trotar y el paracaídas nunca pasan dos veces seguidas, y cada uno espera sus minutos. Caminar es lo de siempre.

### Cómo se porta

- Mientras la escuchás, transcribe, piensa o habla, se queda quieta. Si estaba cayendo, termina de caer.
- Mientras hace una acción, tampoco camina.
- Si está dormida y le toca pasear, primero se despierta.
- No camina por encima del texto de la ventana de adelante ni del cursor. Si se quedó encima del texto, cada tanto se corre.
- Con una app en pantalla completa, o con «Reducir movimiento» en Accesibilidad, no anda por la pantalla.
- El globito de texto la sigue.

### Quedate quieta

En el menú de Wawi está «Quedate quieta». Apaga la gravedad y los paseos: se queda donde está. Para volver, «Podés moverte».

Se guarda en `~/.wawi2/mascot.json`, en la línea `"movimiento": false`.

### Dibujos de otro tamaño

Un cuadro de caída o de paracaídas suele tener otro tamaño que los demás. Para que no se estire, en `[cuadro]`:

- `ajuste = "natural"`: cada dibujo de otro tamaño va a su tamaño, parado abajo y al medio.
- `cajas = "cuadros/cajas.json"`: dónde va cada dibujo, en píxeles del cuadro, desde abajo a la izquierda: `{"caer": [8, -10, 304, 227]}` es x, y, ancho y alto. Una `y` negativa deja colgar algo por debajo del suelo, como las patas sobre el borde de una ventana.

## La ficha completa: `mascota.toml`

El formato es TOML. Lo básico:

- `nombre = "valor"`: los textos van entre comillas.
- Los números van sin comillas: `fps = 8`, `escala = 0.5`.
- Sí o no: `true` o `false`.
- Las listas van entre corchetes: `["a", "b"]`.
- `[seccion]` empieza una sección.
- Lo que va después de `#` es un comentario.

Si escribís mal un nombre (por ejemplo `fsp` en vez de `fps`), el validador te avisa y te sugiere el correcto.

### Arriba de todo

| Clave | Qué es | Obligatoria |
|---|---|---|
| `id` | Nombre corto, igual que la carpeta. Minúsculas, números, `-` o `_`. | sí |
| `nombre` | Cómo se llama. Aparece en el menú: «Mostrar a Kitty». | recomendada |
| `autor` | Quién la dibujó. | recomendada |
| `licencia` | La licencia del arte, por ejemplo `"CC BY 4.0"`. | sí (para el validador) |
| `version` | Por ejemplo `"1.0"`. | recomendada |
| `descripcion` | Una línea sobre ella. | no |
| `carpeta` | Dónde están los cuadros. Si no está, `cuadros`. | no |

### `[cuadro]`

| Clave | Qué es | Si no está |
|---|---|---|
| `ancho`, `alto` | Tamaño de cada cuadro, en píxeles. | El del primer cuadro de `idle` |
| `ancla` | El punto que pisa el suelo, `[x, y]` de 0 a 1 (desde la izquierda y desde abajo). | `[0.5, 0.0]`: abajo al medio |
| `escala` | Puntos de pantalla por píxel. | La que da 124 puntos de ancho |
| `fps` | Cuadros por segundo de las animaciones que no dicen otra cosa. | `8` |
| `cabeza` | Dónde está la cabeza, `[x, y]` de 0 a 1. Ahí apunta el globo de texto. | `[0.5, 0.9]` |
| `ajuste` | Los dibujos de otro tamaño: `"estirar"` o `"natural"` (ver «Dibujos de otro tamaño»). | `"estirar"` |
| `cajas` | Un archivo JSON con la caja de cada dibujo de otro tamaño. | — |

Las hojas necesitan `ancho` y `alto`.

### `[iconos]`

| Clave | Qué es |
|---|---|
| `barra` | Ícono de la barra de menú (se achica a 18 puntos de alto). Si no hay, aparece el nombre. |
| `silencio` | Ícono de micrófono apagado sobre la cabeza. Si no hay, un circulito gris. |

Las rutas son relativas a la carpeta de la mascota.

### `[animaciones.<nombre>]`

Una sección por animación. El nombre puede ser un estado (`idle`, `speaking`, …) o cualquier otro.

| Clave | Qué es | Si no está |
|---|---|---|
| `cuadros` | Lista de archivos de `cuadros/`. | — |
| `secuencia` | Una carpeta dentro de `cuadros/` con todos los PNG en orden. | — |
| `hoja` | Una imagen con los cuadros en grilla. | — |
| `columnas`, `filas` | La grilla de la hoja. | 1 |
| `total` | Cuántas celdas de la hoja usar. | todas |
| `orden` | Qué celdas y en qué orden, desde 0. | 0, 1, 2, … |
| `fps` | Cuadros por segundo. | el de `[cuadro]` |
| `duraciones` | Segundos de cada cuadro, uno por cuadro. Gana sobre `fps`. | — |
| `repetir` | `true`: se repite. `false`: se juega una vez. | `true` |
| `siguiente` | La animación que sigue cuando termina (si no se repite). | — |
| `variantes` | Otras animaciones que pueden ir en su lugar, al azar. | — |
| `fundido` | Segundos que se funde un cuadro con el siguiente. | `0` |
| `movimiento` | Todo el cuerpo: `"ninguno"`, `"salto"`, `"retroceso"`, `"aplastar"`, `"tumbo"`, `"paso"`, `"galope"`, `"trepar"`, `"vaiven_izq"`, `"vaiven_der"`, `"mareo"`, `"sacudida"` u `"orgullo"`. | `"ninguno"` |
| `movimientos` | Lo mismo, uno por cuadro. | — |
| `boca` | Tres cuadros para hablar: cerrada, media, abierta. Solo en `speaking`. | — |
| `capa` | Un dibujo que va con ella mientras se juega, por encima. Por ejemplo, la tela del paracaídas. | — |
| `capa_en` | Dónde va la capa, `[x, y]` en partes del cuadro: `[0, 1]` es un cuadro entero más arriba. | `[0, 0]` |
| `descripcion` | Qué hace, en pocas palabras (para las acciones). | — |

Cada animación usa una sola de `cuadros`, `secuencia` u `hoja`. Una con `boca` puede no tener ninguna.

Ejemplo: escucha con las orejas paradas al principio y después espera.

```toml
[animaciones.listening]
cuadros = ["orejas_arriba"]
duraciones = [0.5]
repetir = false
siguiente = "escuchando"

[animaciones.escuchando]
cuadros = ["escucha", "orejas_arriba"]
duraciones = [2.2, 0.5]
```

Ejemplo: dos maneras de estar quieta, elegidas al azar en cada vuelta.

```toml
[animaciones.idle]
cuadros = ["quieta_a", "quieta_b"]
variantes = ["idle_cola"]

[animaciones.idle_cola]
cuadros = ["cola_a", "cola_b"]
```

### `[estados]`, `[acciones]`, `[vida]` y `[cuerpo]`

Explicadas más arriba.

### `[voz]` y `[alma]` (para más adelante)

Se pueden escribir ya. Wawi las lee, pero todavía no las usa.

```toml
[voz]
id = "ef_dora"      # una voz de Kokoro
velocidad = 1.0
tono = 0            # semitonos (Wawi: +3)

[alma]
archivo = "alma.md" # su personalidad, para empezar su SOUL.md
# o: texto = """Sos una gata curiosa y un poco dormilona."""
```

## Licencias del arte

El código de Wawi tiene una licencia. Los dibujos de cada mascota tienen la suya.

- Wawi está bajo **CC BY-NC 4.0**: se puede compartir y adaptar sin fines comerciales, nombrando a quien la dibujó. Ver `assets/mascotas/wawi/LICENSE-art.md`.
- Para tu mascota, elegí una licencia y escribila en `licencia`. Opciones comunes:
  - `CC0-1.0`: libre para todo, sin pedir nada.
  - `CC BY 4.0`: libre, nombrando al autor.
  - `CC BY-NC 4.0`: libre sin fines comerciales, nombrando al autor.
  - `Todos los derechos reservados`: solo para vos.
- Dejá un archivo de licencia en la carpeta (por ejemplo `LICENCIA-arte.md`) con el nombre del autor.
- Si usás dibujos de otra persona, respetá su licencia y nombrala.
- Los dibujos de ejemplo de `mascota nueva` son CC0: podés hacer lo que quieras con ellos.

## Si algo sale mal

- Una mascota con errores no rompe nada. Wawi avisa en el registro y muestra a Wawi en su lugar.
- Si falta un dibujo, se saltea ese cuadro.
- Si falta un estado, usa el de la tabla.
- Si la ficha no se puede leer, el validador dice la línea.
- `mascota lista` muestra las mascotas instaladas y cuál está activa.

## Para quien programa

- El código está en `crates/wawi-body/src/pack/`.
- `wawi_body::pack::active(...)` carga la mascota activa sin decodificar dibujos.
- `Pack::actions()` lista las acciones; `BodyLink::play(nombre)` hace una.
- `Pack::voice()` y `Pack::soul()` devuelven `[voz]` y `[alma]`.
- El enganche para una herramienta futura `hacer(accion)`: listar `Pack::actions()` (con `Pack::action_description`) en la descripción de la herramienta y, al llamarla, `link.play(accion)`. Es un envío por canal: no frena la voz.
- Los dibujos se decodifican de a poco, en un hilo aparte. Antes del primer cuadro solo se decodifica `idle`. Hay un tope de memoria para los dibujos decodificados.
- El cuerpo está en `crates/wawi-body/src/body/`: la física y los paseos, sin AppKit, con pruebas. `Pack::body` devuelve `[cuerpo]`.
