---
id: TASK-1
title: Probar un agente ACP real dentro de OpenSandbox
status: In Progress
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-17T14:41:00Z'
labels:
  - gc-mvp
  - gc-01
  - hodei-mh
dependencies:
  - TASK-18
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

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Pipeline: lifecycle REST (contrato /v1 OpenAPI, server Python FastAPI solo dev) → execd WS pipe (pty=0, frames binarios 0x00 stdin/0x01 stdout/0x02 stderr/0x03 replay-offset-BE; JSON control stdin|signal|resize|ping; subprotocolo opensandbox-pty; señales SIGINT/SIGTERM/SIGKILL/SIGQUIT/SIGHUP al process group) → ACP v1 NDJSON (crate oficial agent-client-protocol 5.3.0). Agente: claude-code-acp 0.16.2 dentro de sandbox code-interpreter v1.1.0; probar Bun primero, Node fallback documentado. TDD: módulo frames puro → transporte pipe con mock → framing ACP → E2E real con evidencia. Decision: decision-1.
<!-- SECTION:PLAN:END -->
