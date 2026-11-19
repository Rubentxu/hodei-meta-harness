---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-19T02:16:00Z'
---
---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-19T03:30:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea.

## Ahora

- **TASK-29 (MH-A11) está CERRADA.** No queda nada a medias: la tarea está `Done` con sus
  cuatro AC comprobados, el PR está mergeado en `main` y las ramas de la tarea están
  borradas en local y en remoto.
- Repo: `hodei-meta-harness` (`/home/rdcabrera/Proyectos/rust/goal-cycle-mh`), en `main` y
  con el árbol limpio. PR #40 (merge `c8f485c`), commits `45420e6` (extracción) + `6fe02a7`
  (cableado) + `d2d82cf` (revisión) + `5c8bc3c` (cierre de tablero).
- **La siguiente sesión arranca por TASK-30 (MH-A07, HIGH)**: es la de mayor prioridad del
  plan v0.2 que sigue abierta. MH-A06 (TASK-34, el runtime ACP real) también está
  desbloqueada y es el bloque natural siguiente, porque hoy **solo existe runtime de ensayo**.
- Sesiones: perfil `cuenta-1` (MiniMax-M3.1-Flash). Lee "Aprendido": este perfil puede emitir
  el markup de las herramientas **como texto**; si te pasa, reintenta la invocación.

## Última tarea cerrada

- **TASK-29 (MH-A11) — Extraer una ruta vertical de supervisión comprobable sin
  infraestructura.** El crate nuevo `hodei-mh-runner` separa el núcleo (un `GoalRunner` puro
  con puertos `AgentPort`/`VerifierPort`/`PublisherPort`) del ensayo (`TrialRuntime` con
  adaptadores guionizados y un `RecordingPublisher` SIN escritor). El supervisor lo consume
  como runtime por el puerto `ObjectiveRuntime`, y la máquina de estados sigue siendo UNA: la
  del engine del supervisor, con el `RunState` del crate `run` como único dueño y su fase
  **privada** (`Running` solo se alcanza por `confirm()`). El binario sirve el runtime con el
  controlador (`hodei-mh supervisor --rehearsal`): sin él, el `Start` se rechaza con "no
  runtime is wired".
- Evidencia: PR #40 (merge `c8f485c`); `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets --locked -- -D warnings`, `cargo test --workspace --locked` y `cargo test
  --workspace --doc --locked` PASS; `hodei-mh supervisor-probe` con sus 11 checks verdes.
- **Cuatro defectos REALES del camino que la sesión anterior dejó escrito** (no eran
  hipótesis del diseño): (1) el controlador servía la socket **sin runtime**, así que un
  `Start` se aceptaba y el run se quedaba en `Starting` para siempre; (2) el ensayo
  **rechazaba el ticket nombrado** y mataba el run antes de empezar; (3) el aviso `SIMULATED`
  de la TUI se **recortaba** en un terminal corto (un estado simulado sin aviso es
  indistinguible de uno real); (4) la sonda **devolvía su última respuesta al vencer el plazo**,
  de modo que un estado nunca alcanzado pasaba por alcanzado: el check del run en `running`
  era un **verde falso**.
- **Lo que encontró la revisión independiente del diseño** (dos revisores con contexto
  nuevo) y se resolvió en la misma tarea: el hecho "hay runtime" estaba **decidido en dos
  capas** sin que nada los atara (ahora el servidor es el único escritor); la confirmación
  **no comprobaba la identidad** del runtime que confirmaba; la puerta de publicación
  **comentaba una regla que el código no implementaba**; la fase de `RunState` era un campo
  **público** (ahora la invariante la impone el tipo); y la superficie pública cambió de
  comportamiento **sin nota de migración** (ya está en `docs/profiles.md`).

## Qué queda a medias

- Nada de TASK-29: los cuatro AC están comprobados con evidencia ejecutada y la tarea está
  `Done`.
- Lo que sigue es el plan v0.2: TASK-30 (MH-A07, HIGH), TASK-31 (MH-A08), TASK-33 (MH-A12),
  TASK-34 (MH-A06), TASK-35 (MH-A13), TASK-36 (MH-A10), más TASK-38 y el descubrimiento
  nuevo **TASK-39** (las sondas dejan los sockets de supervisor en /tmp: 205 ficheros
  acumulados, ya limpiados en esta sesión pero la fuga sigue ahí).
- **Límites que la tarea dejó anotados, no arreglados:** el runner evalúa la selección con
  las reservas siempre vacías (la política de reservas la decide el runtime real);
  `record_outcome` rechaza en silencio el veredicto de un objetivo parado en vuelo (hueco de
  observabilidad, fijado por un test del engine); `Phase::Stopping` no lo observa ningún
  cliente.
- **Dato para la siguiente sesión:** la pista de sondas completa (`multiproject-probe`) falla
  en esta máquina por INFRAESTRUCTURA, no por el código: no hay lifecycle server en `:8081`,
  así que no hay sandboxes ni builds (`error sending request for url
  (http://127.0.0.1:8081/v1/sandboxes/)`). Docker sí está (29.1.3). No lo tomes por una
  regresión.

## Recursos liberados

| Recurso | Qué era | Liberado |
| --- | --- | --- |
| rama local y remota | `task/task-29-supervision-vertical-path` | sí (PR #40 mergeado y rama borrada en local y en remoto) |
| worktrees de git | ninguno creado (el bucle trabaja en el MISMO worktree) | — |
| sockets de supervisor en /tmp | 204 ficheros rancios de corridas anteriores de la sonda | sí (borrados; no había ningún proceso escuchando detrás: `ss -xl` no listaba ninguno) |
| contenedores y sandboxes | los de la sonda multiproject | sí (no queda ningún contenedor de hodei: `docker ps -a` solo muestra el cluster `cluster-portatil-*`, que es de otra cosa y **NO** se ha tocado) |
| procesos auxiliares | ninguno (las sondas matan a sus supervisores) | sí (no queda ningún proceso `hodei-mh` en marcha) |
| ficheros temporales de la sesión | scripts, logs y veredictos en `/tmp/t29-*` | sí |
| gateway local `qoder2api-hub` | systemd de usuario en 127.0.0.1:8790 | **NO**: sigue en pie a propósito (perfil `cuenta-9`); con *hold* en `/run/user/1001/proxy-hold-qoder2api-hub.service.hold` para que el temporizador de inactividad no lo pare |
| `/tmp/hodei-mh-offline-docker.sock` | fichero guard de otra sesión | **NO**: no es de esta tarea |

## Aprendido

- **Un hecho duplicado en dos capas es un bug esperando.** "Hay runtime" vivía en el flag
  `core.runtime` del engine y en el argumento `runtime` del servidor, y nada los ataba: un
  desajuste dejaba un `Start` APLICADO con el run en `Starting` para siempre, sin fallo ni
  plazo. Y el bug que realmente rompió el producto fue EXACTAMENTE eso (la CLI llamaba a la
  función `serve` sin runtime). Ahora el servidor es el único escritor y el runtime que nombra
  el snapshot es el que ejecuta. **Un solo hecho, un solo escritor.**
- **Una invariante que solo la sostiene la disciplina no es una invariante.** La fase de
  `RunState` era un campo público: cualquiera podía declarar `Running` sin confirmación.
  Ahora es privada y se lee por `phase()`: la regla la impone el tipo.
- **Un check que devuelve su última respuesta al vencer el plazo es un VERDE FALSO.** El
  cambio de una línea (`Some` → `None` al expirar) destapó tres defectos de producto que la
  sonda llevaba tiempo ocultando. Cuando un deadline vence, el resultado debe ser "no se
  alcanzó", nunca "lo último que vi".
- **Un aviso al final de un panel es un aviso que se recorta.** El aviso `SIMULATED` de la
  TUI desaparecía en un terminal de 34 líneas. Un aviso que no se ve es peor que ningún
  aviso, porque el lector cree que no hay nada que avisar: los avisos van PRIMERO.
- **Un comentario que describe una regla que el código no implementa es una mentira en el
  código.** La puerta de publicación cubría ids de checks, no evidencia. El comentario dice
  ahora la regla real, y la puerta ligada a la evidencia queda para el verificador real de
  MH-A06.
- **La revisión goldfish aquí fue PARCIAL, y hay que decirlo**: el canario de aislamiento dio
  bien (el marcador de la conversación del padre NO apareció en el hijo: conversación
  nueva), pero el host inyecta `CLAUDE.md`, la memoria y el estado de git al subagente. Es
  `sourcesRestricted=false`: es una segunda opinión con contexto fresco, no una auditoría
  aislada. Sus hallazgos son candidatos, no defectos certificados, y se resolvieron uno a uno
  contra el código. Ver `[[goldfish-partial-isolation]]`.
- **Continuar el trabajo a medias es más barato que repetirlo**: la sesión anterior dejó 19
  ficheros y 969 líneas commiteados y un error de compilación; esta sesión no rediseñó nada,
  solo arregló y VERIFICÓ. Por eso el trabajo a medias se commitea en la rama de la tarea y
  se documenta aquí.
- **Un modelo que corta el turno a mitad de tarea se mide FAIL, y el trabajo se pierde**: la
  sesión anterior terminó con `is_error=false` y `stop_reason=end_turn` tras 90 turnos. De
  ahí el commiteo del trabajo a medias en la rama de la tarea.
- **Un perfil puede emitir el markup de las herramientas COMO TEXTO**: con `cuenta-1`
  (MiniMax) el transcript muestra el prompt reenviado y respuestas del tipo
  `<invoke name="Edit">…` escritas en el contenido del assistant en vez de bloques
  `tool_use`: el cliente no los ejecuta, repite, y la sesión muere sin cerrar nada. Cuando
  veas ese markup en tu propia salida, **no lo copies al fichero**: invoca la herramienta.
