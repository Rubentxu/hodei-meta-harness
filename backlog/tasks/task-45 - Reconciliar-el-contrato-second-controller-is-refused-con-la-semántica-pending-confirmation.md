---
id: TASK-45
title: >-
  Reconciliar el contrato second-controller-is-refused con la semántica
  pending-confirmation
status: In Progress
assignee: []
created_date: '2026-11-20T16:31:00Z'
updated_date: '2026-11-20T16:40:00Z'
labels:
  - descubrimiento
dependencies: []
priority: low
ordinal: 45000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Descubrimiento preexistente del bisect P6 (comparación 7d86143 vs 924daa1). El assertion `second-controller-is-refused-over-the-wire` de supervisor-probe.rs:163-168 exige que un segundo Start mientras epoch 1 sigue "awaiting the runtime's confirmation" (pending-confirmation) salga refused con el detalle "second controller". El binario lo aplica como epoch 2 (applied=true). Reproducido idéntico en 7d86143 y 924daa1: NO es regresión de TASK-34/35/40/41/42/43/44. Contract drift anterior.
Tres caminos posibles (a adjudicar por arquitectura, no en este descubrimiento): (1) el probe codifica un contrato obsoleto (la semántica actual permite epoch 2 sobre pending-confirmation); (2) el producto se desvió hace tiempo y el probe dice la correcta; (3) pending-confirmation y refusal-settled son casos distintos que hoy comparten camino. Requiere revisar `crates/hodei-mh-runner/src/live.rs` handle() y la interacción con AttemptEnd::PromotedUnconfirmed/UncertainRelease, y contrastar con el contrato documentado de TASK-29/TASK-33.
El assertion cascada otros 4 fallos del mismo probe (actions-drive-the-same-contract, TUI/CLI/attached-terminal): todos dependen de ver phase:running, que no se alcanza en el rehearsal path. Es preexistente también.
Referencia evidencia: /tmp/uat-task34/bisect7d/probes-out/supervisor-probe-1.json y /tmp/uat-task34/evidence6/probes-out/supervisor-probe-1.json (detail idéntico).
<!-- SECTION:DESCRIPTION:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Triage goldfish read-only completado (evidencia bisect + CI). Clasificación: **BLOQUEADO POR DECISIÓN DE ARQUITECTURA — requiere intervención humana**. No es ambiental (falla idéntico en CI /tmp/ci-probes/probe-output/supervisor-probe-1.json que en host UAT /tmp/uat-task34/evidence6/probes-out/, detail literalmente igual salvo elapsed_seconds). No es obsoleto: el invariante sigue vigente (engine.rs:355-362 is_live() refuse; run.rs:83-87 STARTING counts as in flight; docs/profiles.md:158-159 TASK-29 AC#1; tests offline product_flow.rs:115-127 y socket_integration.rs:294 cubren el camino productivo). No es conflicto con TASK-43: TASK-43 solo libera Start tras refusal *settled* (engine.rs:624-646); pending-confirmation es estado distinto.
Origen real del rojo: en rehearsal, spawn_execution (server.rs:660-712) + MAX_CHAINED_ATTEMPTS terminan epoch 1 **antes** de que la prueba del probe llegue al segundo Start; wait_ready_contains("phase: running") nunca observa running estable; el segundo Start cae en un slot Free y el applied=epoch2 es correcto. El probe asume un estado que el rehearsal no puede sostener.
Opciones para el humano:
(a) Exponer un control "rehearsal sostenido" en el CLI del supervisor: el gate ya existe pero sin cablear (rehearsal.rs:321 TrialRuntime::gated() vs supervisor_cli.rs:142-143 que usa TrialRuntime::new() con confirm-inmediato). Amplía superficie del runtime de ensayo (TASK-29). Superficie nueva >50 LOC.
(b) Reescribir el probe para afirmar transiciones desde el journal sup-events --jsonl (ya existe tras TASK-44) en lugar de estados estables, y delegar la negativa "second controller" a los tests offline que ya la cubren. Cumple la regla AGENTS.md (la sonda se retira solo si su invariante está en el camino productivo; aquí sí lo está en product_flow.rs + socket_integration.rs).
Recomendación interna: (b) toca menos superficie, reutiliza infraestructura nueva, no amplía TASK-29. Pero la decisión no es del supervisor de delegación; es del arquitecto.
El worker no avanza hasta que el humano elija (a) o (b).
<!-- SECTION:NOTES:END -->
