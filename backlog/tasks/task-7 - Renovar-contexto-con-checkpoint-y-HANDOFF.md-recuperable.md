---
id: TASK-7
title: Renovar contexto con checkpoint y HANDOFF.md recuperable
status: To Do
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-17T14:41:00Z'
labels:
  - gc-mvp
  - gc-07
  - hodei-mh
dependencies:
  - TASK-2
  - TASK-5
ordinal: 7000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Checkpoint estructurado durable en host y HANDOFF.md generado como proyección temporal; objetivo/ticket se releen. Guardar candidato Git recuperable y evidencia antes de nueva sesión ACP, sin resume ni transcripción previa. Renovar por tarea y en punto seguro cuando contexto fiable exceda umbral configurable. Handoff no concede autoridad.

Clave de incorporación: GC-07.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Cinco renovaciones consecutivas conservan GoalId y WorkRef pero cambian SessionId/epoch; no se usa session/load ni resume al renovar.
- [ ] #2 Un canario solo presente en la conversación previa no se adjunta a la nueva solicitud; se distingue memoria/instrucciones automáticas del harness.
- [ ] #3 Caída antes o después de solicitar nueva sesión no pierde el último checkpoint confirmado ni duplica el controlador.
- [ ] #4 Handoff limitado y redactado conserva siguiente acción decisiones referencias evidencias y estrategias fallidas; no incluye secretos ni transcripción completa.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Prueba canario de contexto real y fallos inyectados en cada frontera de persistencia.
<!-- DOD:END -->
