---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-19T03:06:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea.

## Ahora

- **TASK-30 (MH-A07) está CERRADA.** No queda nada a medias: la tarea está `Done` con sus
  cuatro AC comprobados, el PR #41 está mergeado en `main` y las ramas de la tarea están
  borradas en local y en remoto.
- Repo: `hodei-meta-harness` (`/home/rdcabrera/Proyectos/rust/goal-cycle-mh`), en `main` y
  con el árbol limpio. PR #41 (merge `c83280f`), commits `80ac425` (superficie y eventos),
  `b73e677` (salida acotada), `12244ca` (plan en el tablero) y `ebf30c2` (el socket lo borra
  quien lo ató, y solo él).
- **La siguiente sesión arranca por TASK-34 (MH-A06, HIGH)**: conecta el supervisor de
  producto con el bucle real de objetivos ACP, y hasta ahora solo existe runtime de ensayo.
  La otra candidata de prioridad alta es TASK-33 (MH-A12): reconciliar la promoción Git y el
  cierre de ticket con una intención durable.
- Sesiones: perfil `cuenta-1` (MiniMax-M3.1-Flash). Lee "Aprendido": este perfil puede emitir
  el markup de las herramientas **como texto**; si te pasa, reintenta la invocación.

## Última tarea cerrada

- **TASK-30 (MH-A07) — Acotar IPC y salida conservando eventos estructurados.** La superficie
  de control del supervisor pasa de "`/tmp` y el nombre de un fichero" a "directorio privado
  del usuario, dos bloqueos exclusivos del kernel e identidad por proyecto":
  - El socket por defecto vive en `$XDG_RUNTIME_DIR/hodei-mh/supervisor.sock` (directorio
    `0700`, socket `0600`) y los peers se autentican con `SO_PEERCRED` antes de servirlos.
  - "Quién es el dueño" ya no se decide preguntando si existe un fichero ni con un
    `connect`: se decide con **dos `flock()` exclusivos** (identidad del proyecto y ruta del
    socket), atómicos y liberados por el kernel cuando el proceso muere. Un rechazo nombra al
    tenedor. Un fichero que **no** es socket en la ruta del socket se rechaza y **no se
    borra nunca**; el supervisor borra solo el socket que él ató, y solo si sigue siendo el
    mismo fichero (device e inodo).
  - Cada dimensión del intercambio tiene tope: petición 64 KiB, respuesta 1 MiB, 256 eventos
    por página (con `limit` pedido por el cliente), cuerpo de evento 4 KiB, escritura 10 s,
    inactividad 5 min, 64 conexiones y 1 MiB de salida recogida. Cada exceso tiene una
    respuesta explícita y un cliente normal conserva el servicio.
  - El evento conserva **estructura** (payload tipado y redactado en árbol), **época** y
    **cursor**; la pérdida de historial se **anuncia** (`oldest_seq` + `history_lost`) y la
    página empieza en `oldest_seq`. El CLI y la TUI **siguen el cursor**: antes lo ignoraban
    y presentaban un historial truncado como si fuera el entero.
  - El contrato pasa a la **versión 2**: cada respuesta declara su versión y el cliente
    rechaza la de otra versión en vez de adivinarla. La renovación del TTL usa suma
    comprobada (el build de depuración entraba en pánico con el desbordamiento) y un techo de
    siete días. La salida del execd tiene cupo de 1 MiB con los bytes recibidos, guardados,
    ausentes y descartados **contados por separado**, y el truncamiento se dice en el texto.
  - El contrato está escrito en `docs/supervisor-protocol.md`: dónde vive el socket, los dos
    bloqueos, quién puede hablar con el supervisor, cada límite y qué pasa al excederlo, la
    forma del cable v2, el cupo de salida y la migración.
- Evidencia: PR #41 (merge `c83280f`); `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets --locked -- -D warnings`, `cargo test --workspace --locked` y `cargo test
  --workspace --doc --locked` PASS; 15 tests locales de socket
  (`crates/hodei-mh-supervisor/tests/socket_integration.rs`, sin contenedores ni red) y los
  tests del colector en `crates/opensandbox/src/execd/oneshot.rs`.
- **Lo que encontró la revisión independiente del diseño** (dos revisores con contexto
  nuevo) y se resolvió en la misma tarea: el cursor era global y un cliente que andaba por
  páginas se saltaba eventos (ahora es por página); la tabla de límites era
  **insatisfacible** (256 × 8 KiB > 1 MiB) y el cliente no podía pedir una página más
  pequeña (ahora 4 KiB de cuerpo y `limit` en la petición); los clientes **ignoraban el
  cursor** (la TUI enseñaba siempre los 256 más antiguos); el bloqueo del socket vivía junto
  a un socket en `/tmp`, que es un DoS local preparable (ahora los dos bloqueos viven en el
  directorio privado); las credenciales del peer se leían **después** de partir la conexión
  (`peer_cred` solo existe en el socket); `missing_bytes` contaba como "nunca llegó" lo que
  la cuota había descartado (ahora los contadores van separados); la identidad era solo el
  nombre de la ruta (ahora hay un bloqueo por proyecto); y la sección de migración omitía
  seis cambios observables.

## Qué queda a medias

- Nada de TASK-30: los cuatro AC están comprobados con evidencia ejecutada y la tarea está
  `Done`.
- Lo que sigue es el plan v0.2: TASK-31 (MH-A08), TASK-33 (MH-A12), TASK-34 (MH-A06),
  TASK-35 (MH-A13), TASK-36 (MH-A10), más TASK-38 y el descubrimiento **TASK-39** (las
  sondas dejan los sockets de supervisor en `/tmp`).
- **Límites que la tarea dejó anotados, no arreglados:**
  - Los ficheros de bloqueo (`*.lock`) del directorio runtime **no se borran** al soltar el
    bloqueo: borrarlos abre la carrera de "abrir y bloquear" y puede dar dos dueños. Pero la
    **suite de tests deja unos 15 ficheros de bloqueo nuevos por corrida** (cada test usa una
    ruta de socket única con marca de tiempo): son diminutos y viven en el directorio privado
    del usuario, pero crecen. Limpiados en esta sesión; un test que quiera el árbol limpio
    puede apuntar `XDG_RUNTIME_DIR` a un directorio temporal.
  - El supervisor **no atiende `SIGTERM`**: al recibirlo muere sin la limpieza final y el
    socket queda rancio (que es justo lo que el siguiente controlador sabe tomar). Por eso
    TASK-39 sigue abierta: las sondas matan al supervisor y el fichero se queda. La parcela
    nueva (una parada limpia borra el socket que el controlador ató) reduce la fuga, no la
    cierra.
  - La respuesta se recorta a lo que cabe en 1 MiB **reduciendo el número de eventos**: 256
    eventos de 4 KiB llenan la respuesta justa, así que un cuerpo grande acorta la página.
    El cursor lo dice, pero no avisa de cuántos eventos se quedaron fuera.
- **Dato para la siguiente sesión:** la pista de sondas completa (`multiproject-probe`) falla
  en esta máquina por INFRAESTRUCTURA, no por el código: no hay lifecycle server en `:8081`,
  así que no hay sandboxes ni builds (`error sending request for url
  (http://127.0.0.1:8081/v1/sandboxes/)`). Docker sí está (29.1.3). No lo tomes por una
  regresión.

## Recursos liberados

| Recurso | Qué era | Liberado |
| --- | --- | --- |
| rama local y remota | `task/task-30-bounded-ipc-and-output` | sí (PR #41 mergeado; borradas las dos y comprobado que en GitHub solo queda `main`) |
| worktrees de git | ninguno creado (el bucle trabaja en el MISMO worktree) | — |
| sockets de supervisor en `/tmp` | 13 ficheros rancios de las corridas de test | sí (borrados; no había ningún proceso escuchando detrás) |
| bloqueos del directorio runtime | 24 de proyecto y 127 de socket, todos sin dueño | sí (borrados; el directorio privado queda vacío) |
| procesos auxiliares | un `cargo test` y el binario de tests **colgados** de una corrida anterior | sí (terminados: el binario llevaba más de diez minutos vivo con un bloqueo de proyecto tomado) |
| ficheros temporales de la sesión | scripts, logs y veredictos en `/tmp/t30-*` | sí |
| gateway local `qoder2api-hub` | systemd de usuario en 127.0.0.1:8790 | **NO**: sigue en pie a propósito (perfil `cuenta-9`); con *hold* en `/run/user/1001/proxy-hold-qoder2api-hub.service.hold` para que el temporizador de inactividad no lo pare |
| `/tmp/hodei-mh-offline-docker.sock` | fichero guard de otra sesión | **NO**: no es de esta tarea |

## Aprendido

- **Un test que lee hasta el FIN de la conexión contra un servidor que la mantiene abierta
  cuelga el binario de tests entero.** El test que comprobaba la versión del contrato leía
  con `read_to_string` esperando el EOF; el supervisor mantiene la conexión para la siguiente
  petición, así que el EOF no llegaba nunca. El gate entero se quedó colgado más de diez
  minutos. Y el daño se propagó: el binario colgado **mantenía el bloqueo del proyecto**, así
  que la corrida siguiente fallaba con `ProjectBusy` en un test sin relación con el culpable.
  **El síntoma (un rechazo de bloqueo) y la causa (un proceso vivo) eran de fallos
  distintos**: antes de culpar al código, mira quién tiene el bloqueo. Una línea por
  respuesta (`read_line`), no el final de la conexión.
- **Un socket cerrado con datos sin leer se RESETEA, y el cliente pierde la respuesta.** La
  negativa "la petición excede el límite" se escribía y luego se cerraba la conexión con la
  inundación del cliente sin leer: el kernel resetea el socket y el cliente se queda sin el
  mensaje que explica por qué fue rechazado. Ahora, tras la negativa, el supervisor
  **drena** lo que el cliente sigue mandando (acotado por cuatro veces el límite y por el
  silencio del cliente) antes de cerrar.
- **Una regla documentada que el código no cumple es una mentira en el código.** El módulo
  decía "el socket se borra al parar solo si es el fichero que este proceso ató" y
  `unlink_if_owned` no lo llamaba nadie: era código muerto. Implementado en el `Drop` del
  socket ató, con la identidad **registrada al atar** (no consultada al parar, que es cuando
  el fichero ya puede ser el de otro).
- **Un cursor que el cliente ignora es un historial truncado que se presenta como
  completo.** La CLI y la TUI leían una página y seguían sin más: el resultado era "estos son
  los eventos" cuando eran "estos 256 eventos". Ahora los dos **siguen el cursor**.
- **Cuando el historial se ha perdido, la página debe empezar donde empieza lo retenido y
  decir dónde está el hueco.** Si la página empieza por donde pidió el cliente, el hueco se
  disimula con eventos que nunca existieron.
- **Un hecho duplicado en dos capas vuelve a aparecer en cuanto hay dos capas.** La CLI
  borraba el socket al salir y el `Drop` del socket atado también: dos sitios para la misma
  regla, que pueden discrepar. Ahora lo borra el que **ató** el socket, y solo él.
- **Un límite que se responde pero no se puede leer no es una respuesta.** Importa cuando el
  cliente es el que abuse: no basta con cortar, hay que explicar el corte de forma que llegue.
- **La revisión goldfish aquí fue PARCIAL, y hay que decirlo**: el canario de aislamiento dio
  bien (el marcador de la conversación del padre NO apareció en el hijo: conversación
  nueva), pero el host inyecta `CLAUDE.md`, la memoria y el estado de git al subagente. Es
  `sourcesRestricted=false`: es una segunda opinión con contexto fresco, no una auditoría
  aislada. Sus hallazgos son candidatos, no defectos certificados, y se resolvieron uno a uno
  contra el código. Ver `[[goldfish-partial-isolation]]`.
- **Continuar el trabajo a medias es más barato que repetirlo**: la sesión anterior dejó el
  diseño, la revisión y el contrato escritos y commiteados; esta sesión solo corrigió lo
  que el gate señalaba y añadió el contrato que faltaba. Por eso el trabajo a medias se
  commitea en la rama de la tarea y se documenta aquí.
- **Un perfil puede emitir el markup de las herramientas COMO TEXTO**: con `cuenta-1`
  (MiniMax) el transcript muestra el prompt reenviado y respuestas del tipo
  `<invoke name="Edit">…` escritas en el contenido del assistant en vez de bloques
  `tool_use`: el cliente no los ejecuta, repite, y la sesión muere sin cerrar nada. Cuando
  veas ese markup en tu propia salida, **no lo copies al fichero**: invoca la herramienta.
