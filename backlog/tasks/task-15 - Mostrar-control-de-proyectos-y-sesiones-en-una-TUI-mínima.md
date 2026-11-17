---
id: TASK-15
title: Mostrar control de proyectos y sesiones en una TUI mínima
status: To Do
assignee: []
created_date: '2026-11-17T14:21:00Z'
labels:
  - goalcycle
  - gc-mvp
  - gc-15
dependencies:
  - TASK-1
  - TASK-4
  - TASK-5
ordinal: 15000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Ratatui + CLI como clientes del supervisor socket. Listado proyecto/estado/ticket/revisión/recursos/contexto disponible, eventos ACP estructurados, motivos de espera y acciones Start Pause Resume Stop Renew. Abrir terminal real mediante osb terminal o cliente PTY separado, no emulador multiplexer.

Clave de incorporación: GC-15.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 La TUI descubre el estado del supervisor y se reconecta sin arrancar un segundo controlador.
- [ ] #2 Las acciones de usuario invocan el mismo contrato CLI y el estado refleja confirmación real; telemetría desconocida se muestra como tal.
- [ ] #3 La terminal separada no pretende compartir la conversación ACP; su desconexión no altera el objetivo.
- [ ] #4 Logs y decisión automática identifican procedencia sin mostrar credenciales; salida desconocida no se presenta como éxito.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Probar TUI con estado simulado y una sesión real desde terminal local.
<!-- DOD:END -->
