---
id: TASK-11
title: Completar tareas consecutivas de un backlog real sin intervención
status: To Do
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-17T14:41:00Z'
labels:
  - gc-mvp
  - gc-11
  - hodei-mh
dependencies:
  - TASK-6
  - TASK-7
  - TASK-8
  - TASK-10
ordinal: 11000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Integrar la unidad finita de trabajo con el ciclo infinito, usando verificación/promoción y renovación. Un único camino de ejecución de producción: ACP + OpenSandbox. Automatización por defecto en un entorno de prueba con tareas configuradas. No añadir orquestación de equipos.

Clave de incorporación: GC-11.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Un backlog fixture con cinco tareas y dependencias se completa en orden válido sin repetir tickets ni intervención ordinaria.
- [ ] #2 Cada tarea inicia una conversación nueva y la siguiente usa la rama promovida; los cambios anteriores siguen disponibles.
- [ ] #3 Un objetivo nativo que se declara complete sin evidencia válida no cierra el ticket.
- [ ] #4 La adición de una tarea después de Idle reactiva el flujo; Stop impide reactivación por cambios del tracker.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 E2E en Claude Code real con logs redactados e IDs observados.
<!-- DOD:END -->
