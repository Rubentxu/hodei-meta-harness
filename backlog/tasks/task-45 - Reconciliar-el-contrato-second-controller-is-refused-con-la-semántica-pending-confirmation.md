---
id: TASK-45
title: >-
  Reconciliar el contrato second-controller-is-refused con la semántica
  pending-confirmation
status: To Do
assignee: []
created_date: '2026-11-20T16:31:00Z'
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
