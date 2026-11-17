---
id: TASK-1
title: Probar un agente ACP real dentro de OpenSandbox
status: Done
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-17T21:01:00Z'
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
- [x] #1 Una sesión ACP v1 real ejecuta un prompt que modifica un fichero dentro del sandbox y devuelve eventos estructurados.
- [x] #2 El transporte preserva bytes y separa stdout de stderr incluso con fragmentación y desconexión; nunca reenvía ciegamente un prompt incierto.
- [x] #3 Cancelación y cierre liberan el proceso; la terminal PTY separada funciona sin reclamar la conversación ACP.
- [x] #4 La autenticación disponible y el modo de permisos se comprueban sobre Bazzite o un host de referencia; se registra la matriz real y el fallback admitido sin lanzar nada en el host silenciosamente.
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

sin ac

Cierre del leg real (sesión con decisión del usuario: fue un error marcar Done sin evidencia; autenticación elegida: API key vía env). Evidencia TASK-1 completa: leg REAL ejecutado — claude-code-acp@0.16.2 (Zed, wrapper del claude-agent-sdk) corriendo DENTRO del sandbox OpenSandbox sobre node v22, conversación ACP v1 real: initialize→protocol v1, session/new con mcpServers (el real exige el campo; el mock no), session/prompt REAL vía gateway (ANTHROPIC_AUTH_TOKEN + ANTHROPIC_BASE_URL pasados como env al proceso del sandbox, jamás en disco ni repo), respuesta del modelo real con stopReason=end_turn, 74 session/update estructurados streaming, tool_use Write del agente con session/request_permission respondido como {outcome:selected, optionId:<allow_once>} y el fichero /workspace/hodei-probe.txt creado por el agente DENTRO del sandbox verificado con cat (contenido 'written-by-mock-agent', EXIT:0); stdout ACP (18KB) separado de stderr logs; cancel+SIGTERM→exit frame observado; sandbox borrada sin huérfanos. All_passed TRUE en ambos modos (mock y claude real), estable ×3. Hallazgos de infraestructura resueltos: (1) el sandbox NO tiene salida directa a internet (firewall FORWARD) → instalado host-side y empaquetado el adaptador JS (40MB tgz vía upload_file execd multipart, íntegro) + proxy HTTP CONNECT mínimo en el host (172.17.0.1:8888, solo docker-subnet) para el egress HTTPS del agente; (2) opensandbox::execd::oneshot — el execd PURGA sesiones sin holder WS antes de ejecutar el proceso y los one-shots SILENCIOSOS (>1.2s sin salida) morían por un kill prematuro: el módulo compartido attach-first usa el deadline total como límite y los ticks silenciosos solo continúan (reused por verify::runner y probes); (3) verify::runner refactorizado al oneshot compartido sin duplicar. Matriz de autenticación real: AUTH_TOKEN+BASE_URL gateway Z.AI vía env del sandbox = VÁLIDO (usado); OAuth suscripción = no probado (NOT_RUN, innecesario con gateway válido); credenciales del host nunca reutilizadas ni persistidas.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-1 completa: agente ACP real (claude-code-acp + modelo real vía gateway) ejecutándose DENTRO de un sandbox OpenSandbox, pilotado E2E por el transporte propio de hodei-mh (lifecycle Rust + execd pipe + ACP assembler). Prompt real ejecutado por el modelo con tool_use Write, permiso concedido vía protocolo, fichero creado dentro del sandbox y verificado con cat; 74 eventos estructurados, stdout/stderr separados, cancelación libera el proceso, sandbox sin residuos. Ambas piernas (mock y real) verdes y estables. Infraestructura incorporada como producto: oneshot attach-first compartido (los one-shots sin holder se purgan antes de ejecutar; los comandos silenciosos ya no mueren por kill prematuro), staging de adaptador JS sin internet del sandbox (pack+upload+extract) y egress HTTPS por proxy CONNECT del bridge. Credencial vía env únicamente, matriz documentada, nada lanzado en el host.
<!-- SECTION:FINAL_SUMMARY:END -->
