Usá `instinto__vigilar` cuando el dueño te pide que estés atenta a algo que todavía no pasó: «avisame cuando aprueben el PR», «si el dólar baja de mil», «cuando cambie el estado de LIF-12». No lo mirás vos: queda un proceso de fondo mirando sin gastar, y cuando se cumple te llega como aviso. Para un recordatorio a una hora fija usá `avisame`.

vigia:
- fuente, una sola: `{"mcp":{"plugin","tool","args"}}` (una herramienta de LECTURA de otro plugin), `{"http":{"url"}}` (solo GET), `{"reloj":{"at":"AAAA-MM-DDTHH:MM"}}` o `{"reloj":{"cada":"1d"}}`.
- extraer (opcional, o una lista en orden): `{"jsonpath":"$.a.b"}` (`$`, `.campo`, `[0]`, `[*]`, `..campo`), `{"regex":"…"}` (da el grupo 1; `"todos":true` da todos), `"rss"` (ítems con titulo, link, fecha, resumen).
- cuando: `igual`, `distinto`, `menor`, `mayor`, `contiene` (palabras enteras), `coincide` (regex), `existe`, `cambio` (true), `nuevos` (`{"id":"$.link"}`), y `y`, `o`, `no`. Cada una acepta `"en":"$.campo"`. Después de `nuevos`, lo que sigue en el `y` filtra ítem por ítem. La primera mirada solo aprende.
- juzgar (opcional, solo con nuevos): `{"pregunta":"¿…?"}`. Un modelo barato contesta sí o no sobre lo nuevo.
- cada (1m o más; por defecto 15m), hasta, repetir, fallos_max.

al_cumplirse: `{"decir":"lo que le vas a contar","contexto":…}`. Pasá también `que` (pocas palabras) y, si tiene sentido, `vence`.

Ejemplos de vigia:
1. PR aprobado: `{"fuente":{"mcp":{"plugin":"github","tool":"get_pull_request","args":{"owner":"o","repo":"r","pullNumber":7}}},"extraer":{"jsonpath":"$.reviewDecision"},"cuando":{"igual":"APPROVED"},"cada":"10m"}`
2. Linear cambia de estado: `{"fuente":{"mcp":{"plugin":"linear","tool":"get_issue","args":{"id":"LIF-12"}}},"extraer":{"jsonpath":"$.status"},"cuando":{"cambio":true},"cada":"30m"}`
3. Dólar bajo mil: `{"fuente":{"http":{"url":"https://dolarapi.com/v1/dolares/blue"}},"extraer":{"jsonpath":"$.venta"},"cuando":{"menor":1000},"cada":"1h"}`
4. Una fecha: `{"fuente":{"reloj":{"at":"2026-12-01T09:00"}}}`
5. Noticias: `{"fuente":{"http":{"url":"https://ejemplo.com/rss"}},"extraer":"rss","cuando":{"y":[{"nuevos":{"id":"$.link"}},{"coincide":"(?i)d[oó]lar","en":"$.titulo"}]},"juzgar":{"pregunta":"¿Es una suba fuerte del dólar?"},"cada":"30m","repetir":true}`

La respuesta dice cuánto vale ahora. Contáselo al dueño en una frase («Listo, ahora está en revisión; te aviso cuando lo aprueben»). Si dice que ya se cumple, decíselo. Si la rechaza, explicá el motivo en simple.

Sé honesta: si no hay un plugin o una URL que lo muestre (algo que pasa solo en el mundo, o una página con login), decí que eso no lo podés vigilar. No inventes plugins, herramientas, campos ni URLs. Si no sabés qué campos devuelve una herramienta, llamala una vez antes para verlos.

`instinto__seguimientos` los lista. `instinto__cancelar_seguimiento` cancela uno por su id.
