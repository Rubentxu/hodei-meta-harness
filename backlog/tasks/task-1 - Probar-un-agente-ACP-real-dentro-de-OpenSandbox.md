---
id: TASK-1
title: Probar un agente ACP real dentro de OpenSandbox
status: In Progress
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-17T15:27:00Z'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
EVIDENCIA E2E leg transport (2026-11-17): pipeline completo contra Docker real con agente ACP simulacro (node) sobre pipe execd pty=0 — 12/12 checks, 3 ejecuciones consecutivas verdes ~1.7s (server hodei-mh-lifecycle-server :8081). Verificado: create sandbox -> endpoint execd resuelto -> staging agente -> sesión pipe (mode=pipe, frame connected con session_id real) -> handshake ACP initialize (protocolVersion=1) -> session/new -> session/prompt -> evento estructurado session/update -> respuesta stopReason=end_turn -> fichero escrito POR EL AGENTE dentro del sandbox verificado con cat (/workspace/hodei-probe.txt) -> session/cancel+SIGTERM -> exit frame code=0 (proceso liberado) -> separación stdout(424B ACP)/stderr(40B logs) -> DELETE sin huérfanos. Bytes preservados: stdout ACP NDJSON puro (todas las líneas JSON válidas). Hallazgos de transporte: (1) execd real NO devuelve subprotocolo aunque se ofrezca — tungstenite cliente lo rechaza; solución: clientes con header no ofrecen subprotocolo (como documenta execd), subprotocolo solo en path token. (2) Comandos one-shot pueden terminar antes del attach — completación via polling GET /pty/{id}/status + replay, no via frame exit. Matriz auth: backend Docker local SIN token execd (verificado en código referencia y runtime); agent auth requiere ANTHROPIC_AUTH_TOKEN dentro del sandbox (leg real claude-code-acp 0.16.2: NOT_RUN pendiente decisión de credencial del usuario).

CONDICIÓN DE REANUDACIÓN: leg real claude-code-acp pendiente de decisión de credencial del usuario (API key / OAuth suscripción / aplazar). Todo el trabajo de transporte está completo y verificado. Preguntar al usuario al reanudar.
<!-- SECTION:NOTES:END -->
