---
id: TASK-3
title: Leer trabajo elegible mediante el adaptador Backlog.md
status: To Do
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-17T14:41:00Z'
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
- [ ] #1 La selección respeta isReady y configuración observados; estados desconocidos y dependencia ausente no se consideran listos.
- [ ] #2 Watch procesa objetos JSON completos sucesivos incluyendo vacío; detecta EOF/error y hace resync con polling acotado.
- [ ] #3 Un error de consulta se distingue de un snapshot vacío; no cancela o cierra tareas por ausencia de respuesta.
- [ ] #4 Un FakeTracker con identidad y estados distintos supera los mismos contratos de lectura sin usar tipos Backlog en el core.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Probar contra una instalación Backlog fijada y fixtures con dependencias archivadas y ambiguas.
<!-- DOD:END -->
