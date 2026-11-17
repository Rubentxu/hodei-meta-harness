---
id: TASK-15
title: Mostrar control de proyectos y sesiones en una TUI mínima
status: Done
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-17T22:50:00Z'
labels:
  - gc-mvp
  - gc-15
  - hodei-mh
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
- [x] #1 La TUI descubre el estado del supervisor y se reconecta sin arrancar un segundo controlador.
- [x] #2 Las acciones de usuario invocan el mismo contrato CLI y el estado refleja confirmación real; telemetría desconocida se muestra como tal.
- [x] #3 La terminal separada no pretende compartir la conversación ACP; su desconexión no altera el objetivo.
- [x] #4 Logs y decisión automática identifican procedencia sin mostrar credenciales; salida desconocida no se presenta como éxito.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Probar TUI con estado simulado y una sesión real desde terminal local.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
EVIDENCIA E2E (supervisor-probe, 8 checks ALL=True en 1.5s, procesos REALES del bin): AC1 el supervisor es UN proceso sirviendo UN socket Unix; sup-status descubre el estado (phase idle, telemetría UNKNOWN mostrada como tal); tras matar el supervisor el cliente falla honesto ('supervisor not reachable ... no second controller is started', exit 1) y tras relanzarlo sobre el MISMO path la reconexión funciona — ningún cliente arranca un segundo controlador (por construcción del protocolo; bind limpia sockets huérfanos). AC2 sup-action pause sin run=refused exit 1 ('NOT executed'), start=applied exit 0, snapshot posterior refleja el estado real (ticket TASK-9, revision deadbeef); segundo Start rechazado con 'not a second controller'; la TUI (ratatui) y el CLI usan el MISMO contrato de render y acciones. AC3 attach-terminal es viewer read-only de eventos estructurados (no puede enviar acciones por construcción); su salida no altera el objetivo (phase running intacta después). AC4 logs del supervisor con procedencia [human]/[auto boot], ANTHROPIC_AUTH_TOKEN del propio entorno del proceso NUNCA en el log (redacción pre-emisión); telemetría desconocida jamás presentada como éxito. DoD: TUI probada con estado simulado (etiquetado SIMULATED, nada se presenta como real) y con sesión real desde terminal local (--frames 2 contra el socket vivo, viewport fijo 110x34). Núcleo puro: crates/hodei-mh-supervisor (protocol/engine/provenance/render 16 tests + server/client adapters + 2 tests integración socket real: descubrimiento, exclusividad, replay por seq tras reconexión). Fixes de producto hallados por el probe: log del supervisor sin flush (buffering de bloque → logs de un servicio deben ser oportunos), viewport TUI 0x0 sin tty, redactor que no saltaba espacio tras 'KEY: valor'. Gates: fmt limpio, clippy workspace -D warnings limpio, cargo test workspace 179 passed 0 failed.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-15 completa: TUI Ratatui + CLI como clientes del supervisor socket para hodei-meta-harness. Núcleo puro nuevo crates/hodei-mh-supervisor: protocolo NDJSON (Request Snapshot/Events/Action, SupervisorAction Start/Pause/Resume/Stop/Renew, ActionOutcome con confirmación real), engine (SupervisorCore reusando la máquina de estados del crate run; Start exclusivo, Pause/Resume del bucle de prompts, Stop con reporte de irreversibles, Renew de TTL; eventos estructurados con seq), provenance (Human/Automatic{rule}/Telemetry{unknown} con redacción de credenciales pre-emisión), render (mismo contrato visual para TUI y CLI). Adapters: server Unix-socket NDJSON (bind limpia huérfanos), cliente compartido con NotReachable que nunca arranca un segundo controlador, attach-terminal read-only. Bin: supervisor/sup-status/sup-action/sup-events/attach-terminal/tui (--simulated y --frames para prueba real). Probe E2E con procesos reales: 8 checks ALL=True. 179 tests workspace, fmt/clippy -D warnings limpios.
<!-- SECTION:FINAL_SUMMARY:END -->
