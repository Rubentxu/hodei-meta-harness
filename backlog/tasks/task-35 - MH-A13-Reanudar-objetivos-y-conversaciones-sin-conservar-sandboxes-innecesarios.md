---
id: TASK-35
title: >-
  [MH-A13] Reanudar objetivos y conversaciones sin conservar sandboxes
  innecesarios
status: In Progress
assignee: []
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-20T10:21:00Z'
labels:
  - audit-v0.2
  - product
  - rollover
  - interaction
  - p1
dependencies:
  - TASK-34
references:
  - crates/hodei-mh-rollover/src/checkpoint.rs
  - crates/hodei-mh-rollover/src/handoff.rs
  - crates/hodei-mh-interact/src/resolver.rs
  - crates/hodei-mh-goal/src/source.rs
  - crates/hodei-mh-release/src/idle.rs
priority: medium
ordinal: 35000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P1 · integración posterior · TASK-7/8/13/14. Conectar las capacidades ya implementadas de checkpoint, HANDOFF, interacción y objetivo personalizado a la ruta productiva. Distinguir Goal persistente, Run, sesión y lease. Un handoff transfiere referencias y contexto mínimo, no permisos. ACP-first, plugin solo para una capacidad realmente ausente; no incorporar memoria nueva ni tracker alternativo.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Checkpoint durable conserva identidad/estado del objetivo y candidato; reinicio crea nueva sesión sin duplicar claims ni efectos confirmados.
- [ ] #2 Solicitud ACP de permiso o formulario usa política autorizada y no hereda permiso por handoff; interacción no soportada queda pendiente o denegada explícitamente.
- [ ] #3 Objetivo recurrente sin trabajo espera sin sandbox; objetivo personalizado termina sin mutar Backlog si no tiene ticket.
- [ ] #4 Test determinista de pérdida de contexto y espera, más una UAT de recuperación real cuando este flujo cambie; no repetir toda la matriz de probes.
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Dependencia operativa resuelta: el cableado productivo de TASK-34 está integrado en main (merge f3dcec5, workspace gate PASS). La evidencia viva pendiente de TASK-34 está en curso contra un proveedor ya recuperado (cuenta-1). TASK-35 se implementa sobre main en un worktree aislado; su alcance (checkpoint durable, política de interacción, objetivo personalizado/recurrente en la ruta productiva) no depende de esa evidencia. Hechos contrastados: el checkpoint/HANDOFF no se usa en producción (solo rollover_probe); el permiso ACP binario ignora interact::resolve y las peticiones no soportadas se estancan hasta el timeout de 180 s; GoalSource no se selecciona en producción y Start solo acepta --ticket.

Entrega del worker en rama task/task-35-resume, commit 618521d (base f3dcec5): checkpoint durable en vivo (LiveRecord Pending/Confirmed sobre el state del proyecto, recover/classify, rollover con RolloverLedger, HANDOFF renderizado y redactado), permiso ACP resuelto con interact::resolve y peticiones no soportadas respondidas con -32601 en lugar de esperar el timeout, objetivo personalizado sin ticket (Closes::Nothing, tracker intacto) y StartObjective en el supervisor. Gate del worker: scripts/fast-check.sh --all PASS. Pruebas deterministas nuevas: checkpoint (reinicio abre sesión nueva y conserva identidad; sesión igual rechazada; confirmado sin rehacer; pendiente incierto sin reejecutar; handoff redacta secretos), claims (reinicio con claim vivo no lo duplica), agente (lectura permitida por política; escritura enrutada al sandbox; red/desconocido denegados con causa; grant sin allow_once rechazado; sin herencia de decisión previa), runner (dos start en idle sin coste; objetivo personalizado sin claim ni lectura del tracker, tracker byte-idéntico; objetivo+ticket rechazado), supervisor (goal scope sin ticket). Métricas: 874 s, 109 tool uses, ~19,2 M tokens de subagente. Pendiente para aceptar: rebase sobre el merge de task/task-34-fixes, revisión independiente del diff, UAT real de recuperación (AC4) y merge con gate de workspace. Incierto registrado: un crash con claim vivo deja el claim en supervisor-run; recover reporta el checkpoint pendiente pero no roba el claim ajeno (no hay lease owner todavía).
<!-- SECTION:NOTES:END -->
