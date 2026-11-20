---
id: TASK-35
title: >-
  [MH-A13] Reanudar objetivos y conversaciones sin conservar sandboxes
  innecesarios
status: In Progress
assignee: []
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-20T10:26:00Z'
labels:
  - audit-v0.2
  - product
  - rollover
  - interaction
  - p1
dependencies:
  - TASK-34
references:
  - crates/hodei-mh-rollover/src/checkpoint.rs
  - crates/hodei-mh-rollover/src/handoff.rs
  - crates/hodei-mh-interact/src/resolver.rs
  - crates/hodei-mh-goal/src/source.rs
  - crates/hodei-mh-release/src/idle.rs
priority: medium
ordinal: 35000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P1 · integración posterior · TASK-7/8/13/14. Conectar las capacidades ya implementadas de checkpoint, HANDOFF, interacción y objetivo personalizado a la ruta productiva. Distinguir Goal persistente, Run, sesión y lease. Un handoff transfiere referencias y contexto mínimo, no permisos. ACP-first, plugin solo para una capacidad realmente ausente; no incorporar memoria nueva ni tracker alternativo.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Checkpoint durable conserva identidad/estado del objetivo y candidato; reinicio crea nueva sesión sin duplicar claims ni efectos confirmados.
- [ ] #2 Solicitud ACP de permiso o formulario usa política autorizada y no hereda permiso por handoff; interacción no soportada queda pendiente o denegada explícitamente.
- [ ] #3 Objetivo recurrente sin trabajo espera sin sandbox; objetivo personalizado termina sin mutar Backlog si no tiene ticket.
- [ ] #4 Test determinista de pérdida de contexto y espera, más una UAT de recuperación real cuando este flujo cambie; no repetir toda la matriz de probes.
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Dependencia operativa resuelta: el cableado productivo de TASK-34 está integrado en main (merge f3dcec5, workspace gate PASS). La evidencia viva pendiente de TASK-34 está en curso contra un proveedor ya recuperado (cuenta-1). TASK-35 se implementa sobre main en un worktree aislado; su alcance (checkpoint durable, política de interacción, objetivo personalizado/recurrente en la ruta productiva) no depende de esa evidencia. Hechos contrastados: el checkpoint/HANDOFF no se usa en producción (solo rollover_probe); el permiso ACP binario ignora interact::resolve y las peticiones no soportadas se estancan hasta el timeout de 180 s; GoalSource no se selecciona en producción y Start solo acepta --ticket.

Entrega del worker en rama task/task-35-resume, commit 618521d (base f3dcec5): checkpoint durable en vivo (LiveRecord Pending/Confirmed sobre el state del proyecto, recover/classify, rollover con RolloverLedger, HANDOFF renderizado y redactado), permiso ACP resuelto con interact::resolve y peticiones no soportadas respondidas con -32601 en lugar de esperar el timeout, objetivo personalizado sin ticket (Closes::Nothing, tracker intacto) y StartObjective en el supervisor. Gate del worker: scripts/fast-check.sh --all PASS. Pruebas deterministas nuevas: checkpoint (reinicio abre sesión nueva y conserva identidad; sesión igual rechazada; confirmado sin rehacer; pendiente incierto sin reejecutar; handoff redacta secretos), claims (reinicio con claim vivo no lo duplica), agente (lectura permitida por política; escritura enrutada al sandbox; red/desconocido denegados con causa; grant sin allow_once rechazado; sin herencia de decisión previa), runner (dos start en idle sin coste; objetivo personalizado sin claim ni lectura del tracker, tracker byte-idéntico; objetivo+ticket rechazado), supervisor (goal scope sin ticket). Métricas: 874 s, 109 tool uses, ~19,2 M tokens de subagente. Pendiente para aceptar: rebase sobre el merge de task/task-34-fixes, revisión independiente del diff, UAT real de recuperación (AC4) y merge con gate de workspace. Incierto registrado: un crash con claim vivo deja el claim en supervisor-run; recover reporta el checkpoint pendiente pero no roba el claim ajeno (no hay lease owner todavía).

Revisión goldfish independiente (contexto nuevo, solo lectura) sobre 618521d: veredicto DO_NOT_SHIP, 8 defectos confirmados. Verificados por el supervisor contra el código y decididos: (1) live.rs:463-471 declares checks vacíos para objetivo personalizado; verify/runner.rs:223-232 produce un EvidenceSet vacío y promote/evidence.rs:54-75 rechaza entries vacíos y política sin checks, así que VerifiedPublisher nunca confirma y la rama Closes::Nothing es inalcanzable; el test pasa solo porque enchufa un RegistryPublisher fake. Decisión: --objective exige un criterio de terminación del operador (--accept <command> [--expect-exit N]) que se convierte en el Check del objetivo, reutilizando TerminationCriterion/advance_custom ya implementados (goal/source.rs:26-33, custom_goal_probe.rs:46-56); sin criterio ni ticket, rechazo explícito; la prueba debe recorrer el publisher real o un fake que aplique require_evidence. (2) claims.rs:136-160 codifica el claim permanente tras un crash: el claim se toma en live.rs:392 y el checkpoint se escribe después (agent.rs:161-172), recover() solo imprime (live/mod.rs:168-185); decisión: escribir el checkpoint ANTES del claim y que la recuperación libere el claim del run muerto del que tiene evidencia Pending, reportándolo (la visibilidad del fallo de liberación queda en TASK-41). (3) checkpoint.rs:206-212 begin pisa el registro anterior sin consultar recover: la recuperación es decorativa; decisión: begin debe negarse sobre Pending/Confirmed existentes y el runtime debe preguntar a recover antes de ejecutar. (4) publisher.rs:148-160 + live.rs:551: un confirm que falla degrada una promoción real a PromotionFailed e invita a re-promover; decisión: reportar promovido con checkpoint sin confirmar, nunca reintentar el efecto. (5) agent.rs:184 escribe el handoff después del turno mientras mod.rs:175,183 lo anuncia en el boot: en la ventana incierta no existe; decisión: escribir el handoff junto al Pending y no anunciar rutas inexistentes. (6) checkpoint.rs:92 guarda la instrucción cruda y persist usa create_dir_all/fs::write sin modo (checkpoint.rs:270-286): el JSON de estado queda 0644 con el texto tal cual mientras HANDOFF redacta (rollover/handoff.rs:74); decisión: 0700/0600 y el mismo redactor en el registro. (7) supervisor_cli.rs:218-230 descarta --ticket en silencio cuando hay --objective, saltándose el rechazo 'not both' de live.rs:350-356: decisión: rechazo explícito. (8) rehearsal.rs:312 ignora request.objective y ejecuta trabajo del tracker: decisión: honrarlo o rechazarlo. Plausibles a cerrar si caben: fsync antes del rename y nombre .tmp fijo; from_tool_name casa la subcadena 'ls' y auto-permite títulos como 'Pulse' (interact/request.rs:22-27); PromotionIntent del personalizado nombra un ticket inexistente (publisher.rs:78-89); todos los personalizados comparten goal_id 'custom-objective'. No se integra nada hasta corregir (1)-(8) y pasar la revisión de segunda ronda.
<!-- SECTION:NOTES:END -->
