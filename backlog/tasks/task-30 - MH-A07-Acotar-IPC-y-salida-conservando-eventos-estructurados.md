---
id: TASK-30
title: '[MH-A07] Acotar IPC y salida conservando eventos estructurados'
status: Done
assignee: []
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-19T03:04:00Z'
labels:
  - audit-v0.2
  - security
  - performance
  - p1
dependencies: []
references:
  - crates/hodei-mh-supervisor/src/server.rs
  - crates/hodei-mh-supervisor/src/engine.rs
  - crates/hodei-mh-supervisor/src/protocol.rs
  - crates/opensandbox/src/execd/oneshot.rs
priority: high
ordinal: 30000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P1. server.rs:68-90 elimina ruta tras comprobación no atómica; :95-129 conexiones/líneas sin límites. engine.rs:83-93 convierte JSON a String y drena Vec; :238-243 suma TTL sin check. oneshot acumula bytes sin cupo. Proteger socket con directorio privado/lock, límites de tamaño/tiempo y salida acotada; mantener gap explícito y payload tipado. TASK-15/19. Usar directorio runtime del usuario 0700, socket restrictivo y lock por identidad, no solo nombre de ruta. Autenticación por peer cuando sea aplicable. Versionar contrato si cambia formato. No cambiar a gRPC por defecto.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Dos supervisores concurrentes no pueden asumir la misma identidad; un archivo no socket nunca se elimina.
- [x] #2 NDJSON sobredimensionado, cliente lento y salida excesiva quedan acotados; un cliente normal conserva servicio.
- [x] #3 El evento conserva su estructura, época y cursor; pérdida de historial se anuncia al cliente.
- [x] #4 Tests de socket y collector locales, sin contenedores; no ocultar truncamiento.
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Contrato del socket: directorio runtime privado (0700), socket 0600, dos bloqueos exclusivos (identidad del proyecto y ruta del socket) y negativa dura ante un fichero que no sea socket.
2. Límites del IPC: línea de petición (64 KiB), respuesta (1 MiB), página de eventos (256) con límite pedido por el cliente, cuerpo de evento (4 KiB), plazo de escritura (10 s), inactividad (5 min) y conexiones simultáneas (64).
3. Eventos con estructura: payload tipado y redactado en árbol, época por evento, cursor next_seq y aviso de pérdida de historial (oldest_seq/history_lost). Contrato versionado (v2) en cada respuesta.
4. Salida acotada: cupo de 1 MiB en el colector del execd con contabilidad separada de bytes recibidos, guardados, ausentes y descartados.
5. Verificación: tests de socket y de colector locales (sin contenedores) y gates local/workspace.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementación (PR #41, rama `task/task-30-bounded-ipc-and-output`, merge `c83280f`):

- **Contrato del socket.** El socket por defecto deja de vivir en `/tmp` (directorio que
  todo usuario local puede escribir) y pasa a `$XDG_RUNTIME_DIR/hodei-mh/supervisor.sock`:
  directorio `0700`, socket `0600`. Los dos bloqueos viven TAMBIÉN dentro del directorio
  privado: un fichero de bloqueo junto al socket sería un DoS preparable por otro usuario.
  Ficheros nuevos: `crates/hodei-mh-supervisor/src/paths.rs` y `src/lock.rs`.
- **La propiedad se decide con dos `flock()` exclusivos**, no con una comprobación de
  nombre ni con un `connect`: son atómicos y los libera el kernel al morir el proceso, así
  que un rechazo nombra al tenedor (`held by <proyecto>`). Un fichero que NO es socket en la
  ruta del socket se rechaza y **no se borra nunca**; el supervisor borra solo el socket que
  él ató (device + inodo), tanto al parar limpiamente como cuando ya no lo es.
- **Límites.** Petición 64 KiB, respuesta 1 MiB, 256 eventos por página con `limit` pedido
  por el cliente, cuerpo de evento 4 KiB, escritura 10 s, inactividad 5 min, 64 conexiones.
  Cada exceso tiene respuesta explícita. Una negativa se **drena** lo que el cliente sigue
  mandando (acotado por cuatro veces el límite y por el silencio del cliente) para que la
  pueda leer: un cierre con datos sin leer resetea el socket y el cliente perdería la
  respuesta que dice por qué fue rechazado.
- **Eventos.** Payload **tipado** y redactado en árbol (no solo el primer nivel), `epoch`
  por evento, cursor `next_seq`, `oldest_seq` e `history_lost`. Cuando el historial se
  perdió, la página empieza en `oldest_seq`: el hueco se **anuncia**, no se disimula. El
  CLI y la TUI **siguen el cursor**, que antes ignoraban.
- **Contrato v2.** Cada respuesta declara su versión y el cliente **rechaza** la respuesta
  de otra versión en vez de adivinarla. La renovación del TTL usa suma comprobada (el
  debug build entraba en pánico) y un techo de siete días; las dos negaciones son
  respuestas reales con su motivo.
- **Salida del execd.** Cupo de 1 MiB y **cuatro contadores separados** (bytes recibidos,
  guardados, ausentes, descartados). El truncamiento se dice en el resultado y en el texto
  (`[output truncated: N bytes exceeded the quota of M bytes]`): no se oculta.
- **Contrato escrito.** `docs/supervisor-protocol.md` documenta la superficie: dónde vive
  el socket, los dos bloqueos, quién puede hablar con el supervisor, cada límite y qué
  pasa al excederlo, la forma del cable v2, el cupo de salida y la migración.

Evidencia: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked
-- -D warnings`, `cargo test --workspace --locked` y `cargo test --workspace --doc --locked`
PASS. 15 tests locales de socket (`crates/hodei-mh-supervisor/tests/socket_integration.rs`,
sin contenedores ni red) y los tests del colector en `crates/opensandbox/src/execd/oneshot.rs`.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-30 (MH-A07) cerrada: la superficie de control del supervisor está acotada, es
propiedad de una sola identidad y conserva los eventos estructurados.

Qué cambió: el socket por defecto pasa de `/tmp` al directorio runtime privado del usuario
(`0700`, socket `0600`) con autenticación por credenciales del peer; la propiedad se decide
con dos bloqueos exclusivos del kernel (identidad del proyecto y ruta del socket) en vez de
una comprobación de nombre racy; un fichero que no es socket nunca se borra; cada dimensión
del intercambio tiene un límite explícito (64 KiB de petición, 1 MiB de respuesta, 256
eventos por página, 4 KiB de cuerpo, 10 s de escritura, 5 min de inactividad, 64 conexiones);
el evento conserva payload tipado, redactado en árbol, época y cursor, y la pérdida de
historial se anuncia; el contrato pasa a la versión 2 y los clientes rechazan otra versión;
la salida del execd tiene cupo de 1 MiB con los bytes recibidos, guardados, ausentes y
descartados contados por separado.

Verificación: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked
-- -D warnings`, `cargo test --workspace --locked` y `cargo test --workspace --doc --locked`
PASS; 15 tests locales de socket sin contenedores y los tests del colector. El contrato
queda escrito en `docs/supervisor-protocol.md`. Mergeado en `main` con `c83280f` (PR #41).

Riesgos y continuación: la pista de sondas completa (`multiproject-probe`) sigue fallando
por INFRAESTRUCTURA en esta máquina (no hay lifecycle server en `:8081`), no por este
cambio. El cambio de ruta del socket es observable: quien lo use por su ruta anterior
(`/tmp/hodei-mh-supervisor.sock`) debe pasar `--socket` o aceptar el nuevo sitio. La
revisión de diseño fue independiente pero de aislamiento PARCIAL (el host inyecta
`CLAUDE.md`, la memoria y el estado de git al revisor): sus hallazgos son candidatos, y
cada uno se resolvió contra el código antes de cerrar.
<!-- SECTION:FINAL_SUMMARY:END -->
