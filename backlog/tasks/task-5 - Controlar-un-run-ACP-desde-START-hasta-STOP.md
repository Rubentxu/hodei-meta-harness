---
id: TASK-5
title: Controlar un run ACP desde START hasta STOP
status: Done
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-20T20:24:00Z'
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
- [x] #1 Start crea un grant explícito asociado al proyecto y a los efectos autorizados; instalar o editar WORKFLOW no activa ni amplía permisos
- [x] #2 hodei-mh abre y controla una sesión real exclusivamente; eventos llevan identidad de run/session/epoch
- [x] #3 Stop invalida respuestas y efectos pendientes; cancela procesos controlados y muestra acciones ya iniciadas que no pueden deshacerse
- [x] #4 Si el agente carece de goal nativo el mismo trabajo finito se ejecuta por session/prompt sin simular capacidades
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Prueba E2E real de start/pause/resume/stop y carrera Stop contra una respuesta pendiente.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
EVIDENCIA (Docker real, 3 ejecuciones verdes ~4s): run-probe 6/6. START: grant explícito por proyecto/epoch con efectos enumerados (AgentPromptSession concedido, WorkspaceGitCommit denegado; paths de gestión nunca en scope, probado en unit tests) + claim del coordinador con segundo pretendiente DENIED (sesión exclusiva). Eventos: cada mensaje del wire etiquetado run/session/epoch; eventos foráneos rechazados por la máquina de estados (3 variantes: run/session/epoch distintos). AC4: agente sin goal nativo -> RunPlan::FinitePrompts -> turnos session/prompt literales (1/1 completado, stopReason=end_turn), sin simulación de goal. STOP: pending invalidado, proceso controlado cancelado vía SIGTERM con frame exit observado (exit=-1 tras SIGTERM), acción irreversible iniciada REPORTADA honestamente (no fingida cancelable). Cleanup: sandbox borrado + claim liberado. 79 tests workspace, clippy -D warnings 0, fmt. Limitación documentada: leg con claude-code-acp real sigue pendiente de credencial (misma condición que TASK-1).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Control START/STOP de runs ACP verificado E2E: grants explícitos no ampliables, sesión exclusiva vía coordinator, eventos con identidad run/session/epoch, STOP que invalida/cancela/reporta, work finito por session/prompt para agentes sin goal. Leg real pendiente de credencial compartido con TASK-1.
<!-- SECTION:FINAL_SUMMARY:END -->
