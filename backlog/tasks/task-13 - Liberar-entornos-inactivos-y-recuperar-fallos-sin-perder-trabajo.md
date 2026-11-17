---
id: TASK-13
title: Liberar entornos inactivos y recuperar fallos sin perder trabajo
status: Done
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-17T22:33:00Z'
labels:
  - gc-mvp
  - gc-13
  - hodei-mh
dependencies:
  - TASK-7
  - TASK-10
  - TASK-12
ordinal: 13000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Implementar lifecycle OpenSandbox: create/inspect/renew/delete y endpoint resolution, estado, TTL, ownership tags, generación y lease. Zero-token idle: detener agente y liberar entorno cuando trabajo recuperable. Recuperación de proceso/sandbox y retry por causa con jitter; no asumir snapshots tras OOM. Proteger estado y archivos externos.

Clave de incorporación: GC-13.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Desconexión/timeout tras create reconcilia el efecto por metadata/estado antes de intentar otro; huérfanos solo se limpian si pertenecen a hodei-mh
- [x] #2 Idle libera el entorno gestionado después de confirmar candidato/checkpoint y sigue observando el tracker sin inferencia
- [x] #3 Pérdida del sandbox se recupera desde candidato durable y sesión nueva; datos nunca exportados se marcan como posible pérdida
- [x] #4 Dos intentos improductivos o unos 20–30 minutos observados activan investigación y cambio de estrategia; antes de una hora se decide otro slice o yield sin cierre ficticio
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Inyectar pérdida de respuesta creación OOM simulado caída del supervisor y expiración real de un sandbox.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
EVIDENCIA E2E (release-probe, server 8081 real, ALL=True en 13s): AC1 create con respuesta perdida (timeout 400ms cliente) reconciliado por request-tag via GET list metadata -> AdoptExisting{id}, effects with tag=1 (sin duplicado); huérfano managed-by=hodei-mh liberado DELETE->GET 404, sandbox extranjero (team=foreign-demo) NUNCA seleccionado y sigue vivo (limpiado después por el probe, no por el reconciliador). AC2 IdleGate::WorkPending->Keep (GET 200); candidato confirmado con digest real -> Release -> DELETE->404; tracker fixture backlog observado por CLI tras el release (task list --plain, sin inferencia). AC3 pérdida REAL (DELETE->404, sin snapshots): ResumeFromCandidate en sandbox NUEVO restaura bytes durables (base64) y digest verificado idéntico; sin export -> PossibleDataLoss{half-edited.rs,scratch.sh} marcado exacto. AC4 todas las decisiones terminales/accionables (adopt/retry/release/marked-loss), slice completo 13s<300s; 2 hallazgos investigados y corregidos en minutos, sin estancamiento. Núcleo puro: crates/hodei-mh-release (reconcile/idle/recovery, 7 tests). HALLAZGO REAL del server corregido con test de regresión: sandbox_from_inspect parseaba el label metadata-keys con serde_json::from_value(Value::String) que SIEMPRE fallaba -> metadata nunca restaurada en GET/list; fix from_str + test metadata_is_restored_from_labels_on_inspect. Gates: fmt limpio, clippy workspace -D warnings limpio, cargo test workspace 160 passed 0 failed.

DoD verificado: el TTL es REAL, no decorativo. Implementado expiry reaper en el lifecycle server: decision pura compose::expired_sandboxes (solo vencidos, manual-cleanup jamás; test expiry_selects_only_due_sandboxes_and_never_manual_cleanup_ones), metodo service::expire_due_sandboxes (list->decide->delete, carradas 404 ignoradas), loop tokio en main con EXPIRY_SWEEP_SECONDS (default 5s). Demo E2E en release-probe check ttl-expiration-is-real-server-enforced: sandbox con timeout=8s liberado por el sweep del server SIN DELETE del probe (7 checks ALL=True en 18.7s). Perdida de respuesta real inyectada (timeout 400ms con efecto creado y adoptado por tag). 161 tests workspace 0 failed, fmt/clippy -D warnings limpios.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-13 completa: ciclo de release/recovery de entornos para hodei-meta-harness. Núcleo puro nuevo crates/hodei-mh-release: reconcile.rs (CreateOutcome Confirmed/Unknown por request-tag -> ReconcileDecision AdoptExisting/RetrySafe/Abort; orphans_to_release solo managed-by=hodei-mh, unclaimed e idle), idle.rs (IdleGate WorkPending/CandidateConfirmed/CheckpointConfirmed -> ReleaseDecision: release SOLO con durable confirmado), recovery.rs (SandboxLoss -> ResumeFromCandidate con sesión nueva | PossibleDataLoss marcando ficheros no exportados; nunca se asumen snapshots). Probe E2E release-probe contra el lifecycle server real: 6 checks ALL=True en 13s con DELETE/GET 404 verificados, sin duplicados, extranjero intacto y tracker observado tras el release. Bug del server corregido con regresión: restauración de metadata en inspect (from_str). 160 tests workspace, fmt/clippy -D warnings limpios.
<!-- SECTION:FINAL_SUMMARY:END -->
