---
id: TASK-7
title: Renovar contexto con checkpoint y HANDOFF.md recuperable
status: Done
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-17T16:11:00Z'
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
- [x] #1 Cinco renovaciones consecutivas conservan GoalId y WorkRef pero cambian SessionId/epoch; no se usa session/load ni resume al renovar.
- [x] #2 Un canario solo presente en la conversación previa no se adjunta a la nueva solicitud; se distingue memoria/instrucciones automáticas del harness.
- [x] #3 Caída antes o después de solicitar nueva sesión no pierde el último checkpoint confirmado ni duplica el controlador.
- [x] #4 Handoff limitado y redactado conserva siguiente acción decisiones referencias evidencias y estrategias fallidas; no incluye secretos ni transcripción completa.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Prueba canario de contexto real y fallos inyectados en cada frontera de persistencia.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Evidencia TASK-7 (rama task/task-7-rollover): crate hodei-mh-rollover con (1) checkpoint.rs — RolloverLedger FSM bifásica begin→record_session→confirm; recovery_action clasifica AdoptPending (crash tras pedir sesión: adopta LA MISMA sesión sin duplicar controlador) vs ReRequest (crash antes: re-pide seguro); next_step siempre RequestNewSession (session/load no existe en el camino); goal_id/work_ref invariantes, epoch/session_id cambian; test 5 rollovers consecutivos epochs 2..=6. (2) handoff.rs — render_handoff SOLO desde Checkpoint (sin parámetro transcript por construcción), contains_canary_risk rechaza marcadores de transcripción, redact_secrets nivel-línea ([REDACTED] para AKIA/ghp_/sk-/token/password/secret), MAX_HANDOFF_BYTES 8KiB con marcador [TRUNCATED]. 8/8 tests unitarios verdes. Probe E2E hodei-mh rollover-probe contra sandbox real (opensandbox/code-interpreter:v1.1.0) + mock agent ACP: 5 conversaciones ACP reales completas (initialize→session/new→prompt→cancel→exit), epochs [2,3,4,5,6], 5 session_ids únicos, crash-recovery con ledger persistido a disco (recarga JSON → AdoptPending, epoch 7), handoff limpio de canario y con todas las secciones, sandbox borrada sin huérfanos. 5/5 checks verdes, estable en 2 ejecuciones (3.8s y 2.6s). Gates: cargo fmt OK, clippy --workspace --all-targets -D warnings OK, cargo test --workspace OK (58+ tests).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-7 completa: renovación de contexto por rollover con checkpoint durable y HANDOFF.md recuperable. Crate hodei-mh-rollover: ledger bifásico crash-safe (begin→record_session→confirm) donde una caída tras pedir sesión ADOPTA la sesión pendiente sin duplicar el controlador y una caída antes re-pide de forma segura; el último checkpoint confirmado nunca se pierde. La renovación SIEMPRE crea sesión nueva (session/load/resume no existe en el diseño); goal_id y work_ref sobreviven, epoch y session_id cambian (5 rollovers consecutivos verificados E2E: epochs 2..6, 5 sesiones PTY reales). HANDOFF.md se renderiza únicamente desde el checkpoint — sin parámetro de transcripción, un canario presente solo en la conversación previa no tiene camino hacia la nueva solicitud (probado con canario E2E); acotado a 8KiB con truncado y redacción de secretos por línea. Verificación: 8 tests unitarios + probe E2E 5/5 checks ×2 ejecuciones contra sandbox real; gates fmt/clippy/tests workspace en verde.
<!-- SECTION:FINAL_SUMMARY:END -->
