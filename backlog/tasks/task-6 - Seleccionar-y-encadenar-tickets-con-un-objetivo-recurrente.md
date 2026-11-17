---
id: TASK-6
title: Seleccionar y encadenar tickets con un objetivo recurrente
status: To Do
assignee: []
created_date: '2026-11-17T14:20:00Z'
labels:
  - goalcycle
  - gc-mvp
  - gc-06
dependencies:
  - TASK-3
  - TASK-5
ordinal: 6000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Definir Infinite Goal como intención activa durante idle; la unidad finita es el WorkItem. Seleccionar de forma determinista por prioridad/orden del tracker e identidad estable; no usar LLM para decidir el siguiente ticket. Una tarea por flujo por defecto; WIP informativo no gate. Prohibición de inventar trabajo y de cerrar por actividad textual.

Clave de incorporación: GC-06.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Un conjunto de tareas independientes y dependientes produce una selección reproducible con explicación de no elegibilidad.
- [ ] #2 Una tarea bloqueada por dependencia técnica permite avanzar otra; respetar asignación y claim existentes.
- [ ] #3 Con backlog vacío el supervisor entra en Idle y no hace llamadas al modelo; una nueva tarea elegible lo despierta.
- [ ] #4 Un work item externamente completado o cambiado dispara refresh y revalidación sin confundir una pérdida de visibilidad con eliminación.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Pruebas con FakeTracker y Backlog real de reapertura y alta durante Idle.
<!-- DOD:END -->
