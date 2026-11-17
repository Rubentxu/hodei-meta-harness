---
id: TASK-14
title: Ejecutar un objetivo personalizado sin issue tracker
status: To Do
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-17T14:41:00Z'
labels:
  - gc-mvp
  - gc-14
  - hodei-mh
dependencies:
  - TASK-5
  - TASK-9
ordinal: 14000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
GoalSource distingue TrackerScope y CustomObjective. Objetivo personalizado explícito con criterio de cierre verificable; por defecto tracker backlog. No crear tareas fantasma; objetivo finito completado queda Idle mientras supervisor continúa habilitado. No inventar trabajo recurrente sin fuente de cambios.

Clave de incorporación: GC-14.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Usuario inicia objetivo sin tracker con condición de terminación verificable y obtiene evidencia sin escribir tickets.
- [ ] #2 Completar ese objetivo no crea otro automáticamente; nueva edición autorizada puede reactivarlo.
- [ ] #3 Un fallo de configuración del tracker no convierte el objetivo por defecto en objetivo personalizado silenciosamente.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Prueba end-to-end con objetivo personalizado y sin CLI Backlog instalada.
<!-- DOD:END -->
