---
id: TASK-3
title: Leer trabajo elegible mediante el adaptador Backlog.md
status: Done
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-20T20:24:00Z'
labels:
  - gc-mvp
  - gc-03
  - hodei-mh
dependencies: []
ordinal: 3000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
WorkReader y WorkRef con instancia/proyecto/ID nativo y revisión observada. Consumir CLI oficial JSON v1 incluyendo watch de snapshots completos y refresh por IDs. Preservar estados/configuración, readiness y dependencias del tracker. El agente no tiene que copiar el backlog autoritativo a su sandbox.

Clave de incorporación: GC-03.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 La selección respeta isReady y configuración observados; estados desconocidos y dependencia ausente no se consideran listos.
- [x] #2 Watch procesa objetos JSON completos sucesivos incluyendo vacío; detecta EOF/error y hace resync con polling acotado.
- [x] #3 Un error de consulta se distingue de un snapshot vacío; no cancela o cierra tareas por ausencia de respuesta.
- [x] #4 Un FakeTracker con identidad y estados distintos supera los mismos contratos de lectura sin usar tipos Backlog en el core.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Probar contra una instalación Backlog fijada y fixtures con dependencias archivadas y ambiguas.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
EVIDENCIA (backlog real v1.55.3 de este repo + fixtures): tracker-probe 6/6 PASS. Core provider-agnóstico sin tipos Backlog: WorkItem/WorkStatus/select_eligible/compute_readiness. Selección respeta isReady observado y config observada (statuses 'To Do, In Progress, Done'); estados Unknown preservan texto del proveedor y jamás elegibles; dependencia ausente (TASK-ARCHIVED fixture) bloquea conservativamente. Snapshot real: 18 items, elegible=[TASK-4], In Progress excluidos (TASK-1, TASK-3). Error de consulta ≠ snapshot vacío: TrackerError::Query con CLI roto; el core nunca cancela por ausencia de respuesta. Watch: feed_stream procesa objetos JSON completos sucesivos INCLUIDO {} vacío, truncado queda en buffer, malformado = TrackerError::Stream; watch() hace resync con polling acotado (max_polls) tras EOF/error. FakeTracker con identidades/estados distintos supera los mismos contratos (verify_read_contracts + verify_error_contracts). 14 unit tests tracker (fixtures archivados/ambiguos incluidos), 64 tests workspace total, clippy -D warnings 0, fmt.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Adaptador WorkTracker para Backlog.md verificado contra instalación fijada y backlog real: selección conservativa (isReady + dependencias resueltas), watch con resync acotado, error ≠ vacío, FakeTracker compartiendo contratos. Core 100% libre de tipos Backlog.
<!-- SECTION:FINAL_SUMMARY:END -->
