---
id: TASK-1
title: Probar un agente ACP real dentro de OpenSandbox
status: To Do
assignee: []
created_date: '2026-11-17T14:20:00Z'
labels:
  - goalcycle
  - gc-mvp
  - gc-01
dependencies: []
ordinal: 1000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Primer corte vertical de producto. Fijar versiones concretas de OpenSandbox/execd y claude-agent-acp/Claude Code con evidencia de autenticación. Probar pipe WebSocket sin PTY como primera opción; relay stdio mínimo si no cumple. No asumir que Podman rootless ni una suscripción Claude funcionan. El usuario puede abrir una terminal separada.

Clave de incorporación: GC-01.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Una sesión ACP v1 real ejecuta un prompt que modifica un fichero dentro del sandbox y devuelve eventos estructurados.
- [ ] #2 El transporte preserva bytes y separa stdout de stderr incluso con fragmentación y desconexión; nunca reenvía ciegamente un prompt incierto.
- [ ] #3 Cancelación y cierre liberan el proceso; la terminal PTY separada funciona sin reclamar la conversación ACP.
- [ ] #4 La autenticación disponible y el modo de permisos se comprueban sobre Bazzite o un host de referencia; se registra la matriz real y el fallback admitido sin lanzar nada en el host silenciosamente.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Registrar comandos versiones exactas y resultado real de cada caso en esta tarea.
<!-- DOD:END -->
