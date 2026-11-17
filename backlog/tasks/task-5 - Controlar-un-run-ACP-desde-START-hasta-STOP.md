---
id: TASK-5
title: Controlar un run ACP desde START hasta STOP
status: In Progress
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-17T15:54:00Z'
labels:
  - gc-mvp
  - gc-05
  - hodei-mh
dependencies:
  - TASK-1
  - TASK-4
ordinal: 5000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Implementar HarnessRuntime mínimo: negotiate/new/prompt/observe/cancel/reconcile. Start en modo YOLO explícito y scope de proyecto; control plane fuera del sandbox. ACP estable v1; goal nativo opcional y no requisito. Control socket local privado. Pausar conserva grant pero suspende; Stop revoca inmediatamente y evita nuevos efectos.

Clave de incorporación: GC-05.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Start crea un grant explícito asociado al proyecto y a los efectos autorizados; instalar o editar WORKFLOW no activa ni amplía permisos
- [ ] #2 hodei-mh abre y controla una sesión real exclusivamente; eventos llevan identidad de run/session/epoch
- [ ] #3 Stop invalida respuestas y efectos pendientes; cancela procesos controlados y muestra acciones ya iniciadas que no pueden deshacerse
- [ ] #4 Si el agente carece de goal nativo el mismo trabajo finito se ejecuta por session/prompt sin simular capacidades
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Prueba E2E real de start/pause/resume/stop y carrera Stop contra una respuesta pendiente.
<!-- DOD:END -->
