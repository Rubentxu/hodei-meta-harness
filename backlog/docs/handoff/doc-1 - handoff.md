---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-18T22:57:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea.

## Ahora

- Tarea: **TASK-27 — [MH-A04] Admitir y recuperar sandboxes de forma atómica y honesta**
  (en curso; el núcleo está implementado y compilando, faltan las pruebas con dientes, el
  smoke real, la documentación y el cierre).
- Repo: `hodei-meta-harness` (`/home/rdcabrera/Proyectos/rust/goal-cycle-mh`).
- Rama: `task/task-27-atomic-admission` (commit `239b8d6`, pusheada a origin).
- Worktree: el propio repositorio, sin worktrees adicionales.
- `main` local: sin cambios respecto de origin/main.

### Qué falta exactamente para cerrar TASK-27

Está TODO en las notas de la tarea (`backlog task view TASK-27 --plain`), resumido:

1. **Pruebas offline con dientes**: admisión concurrente determinista (un daemon simulado que
   acepta y no responde: la primera creación queda en vuelo con su slot reservado y la
   segunda recibe `429 SANDBOX::BUDGET_EXHAUSTED`; al abandonar la primera, la reserva se
   suelta); conflicto de clave (`409 SANDBOX::CREATION_CONFLICT`) y misma clave + mismo cuerpo
   → un solo entorno; liberación de la reserva tras fallo y tras reinicio reconciliado;
   fallo de DELETE (reintento acotado, y el fallo **no** aparece como liberado).
2. **Pruebas de `compose`**: huella de creación (misma intención → misma huella; intención
   distinta → huella distinta), labels de la clave, parsers que rechazan NaN/infinito/cero/
   desbordamiento, TTL de 0 rechazado.
3. **Smoke real** en la pista de sondas: creates **concurrentes** contra el presupuesto real
   (con el código viejo se admiten todos; con el nuevo, uno y `429` para el resto) y la misma
   clave de creación (un solo sandbox).
4. **Documentación** (`docs/profiles.md`, `README.md`) y el cliente (`profile_run` y el runner
   de `hodei-mh-verify` mandan `creationKey` = su marca única).
5. **Gates**: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `scripts/fast-check.sh`
   y `scripts/probes.sh`; después PR y cierre de la tarea.

## Última tarea cerrada

- TASK-26 (MH-A03): el coordinador responde solo lo que es durable — claim, release, intención
  de efecto y evento son transacciones `BEGIN IMMEDIATE`; el lock de escritura de SQLite es la
  exclusión entre procesos; el estado es por proyecto y vive fuera del workspace.
- Evidencia: PR #36 (merge `f137f36`), `cargo test --workspace` en verde, fmt y clippy limpios,
  `scripts/fast-check.sh` PASS, pista de sondas PASS sin residuo.

## Qué queda a medias

- TASK-27: implementada la autoridad de admisión, la reserva antes de los efectos, la
  idempotencia por clave de creación, el borrado honesto con reintento acotado y la
  reconciliación con el daemon. **Falta la verificación** (pruebas, sondas, docs) y el cierre.

## Recursos liberados

| Recurso | Qué era | Liberado |
| --- | --- | --- |
| worktree / ramas | — | — |
| pods / namespaces de k8s | — | — |
| contenedores / clusters kind | — | — |
| procesos auxiliares | — | — |

## Aprendido

- **El presupuesto global de entornos contaba contenedores ANTES del `await` que los crea**: dos
  altas simultáneas veían el host vacío y las dos tomaban el último slot. La reserva se toma
  antes de cualquier efecto y la suelta un guard RAII si la creación falla o se abandona.
- **La regla de los slots tiene un único dueño**: `hodei-mh-budget` (`admissible_creates`); el
  lifecycle server es el único que la aplica y el coordinador conserva los presupuestos de
  objetivo. No hay un segundo ledger.
- **Una reconciliación necesita un horizonte**: sin él, un entorno que se acaba de crear entre la
  observación y la reconciliación se borraba de la cuenta (y el presupuesto admitía de más). La
  autoridad numera los compromisos y la reconciliación solo olvida los anteriores al horizonte.
- **El execd de un sandbox recién creado todavía no acepta sesiones**: esperar de forma ACOTADA
  antes de usarlo, con un probe inofensivo para que el comando real corra EXACTAMENTE una vez.
- **El daemon fusiona el `ENV` de la IMAGEN con el del contenedor** → una imagen que declare
  `EXECD_*`/`OPENSANDBOX_*` escribe en el plano de ejecución: hay que INSPECCIONAR la imagen y
  rechazarla.
- **Comparar un prefijo sobre un `&str` con `entry[..n]` PANICA** si el corte cae dentro de un
  carácter UTF-8: la comparación de nombres tiene que ser sobre BYTES.
- **La edad de una red no dice nada de cuánto tarda su creación**: las creaciones en vuelo se
  REGISTRAN y el barrido nunca les quita la red.
- **Una prueba de carrera puede pasar sin detectar el defecto**: la prueba que tiene dientes
  FUERZA la superposición (un daemon simulado que acepta y no responde), y se comprueba
  restaurando el comportamiento viejo y viendo que la prueba falla.
- **El fallo de escritura se inyecta de VERDAD** (un trigger de SQLite desde otra conexión), no
  con un simulacro en el código.

## Candidatos siguientes

- TASK-28 (MH-A05) — Preparar workspaces y comandos sin transportar secretos del host (High).
- TASK-29 (MH-A11) — Extraer una ruta vertical de supervisión comprobable sin infraestructura.
- TASK-30 (MH-A07) — Acotar IPC y salida conservando eventos estructurados.
- TASK-31 (MH-A08) — Deadline único y cleanup reutilizable del transporte OpenSandbox.
- TASK-33 (MH-A12) — Reconciliar promoción Git y cierre de ticket con una intención durable.
- TASK-34 (MH-A06) — Conectar el supervisor de producto al bucle real de objetivos ACP.
- TASK-35 (MH-A13) — Reanudar objetivos y conversaciones sin conservar sandboxes innecesarios.
- TASK-36 (MH-A10) — Reutilizar cachés de compilación sin compartir estado de agentes.
- TASK-38 — El run-probe no entrega la credencial del execd y su pipe-attach falla con 401.
