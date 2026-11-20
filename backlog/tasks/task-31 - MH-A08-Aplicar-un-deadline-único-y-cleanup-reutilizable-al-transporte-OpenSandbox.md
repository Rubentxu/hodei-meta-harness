---
id: TASK-31
title: >-
  [MH-A08] Aplicar un deadline único y cleanup reutilizable al transporte
  OpenSandbox
status: Done
assignee:
  - '@rubentxu'
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-20T08:29:00Z'
labels:
  - audit-v0.2
  - performance
  - reliability
  - p1
dependencies:
  - TASK-27
references:
  - crates/opensandbox/src/execd/oneshot.rs
  - crates/opensandbox/src/execd/api.rs
  - crates/hodei-mh-verify/src/runner.rs
  - crates/hodei-mh/src/main.rs
priority: medium
ordinal: 31000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P1. oneshot.rs:73-113 y :191-301 repiten readiness y tienen plazos separados de create/attach/replay/cleanup; main.rs:309-323 intenta limpiar puerto 0 con tarea no esperada. Preservar attach-first de TASK-20, cachear readiness por lease y compartir lifecycle client, cleanup y deadline de extremo a extremo. TASK-13/20. Unificar LifecycleClient y lease de entorno existente sin introducir otro daemon. Distinguir NoAceptado de ResultadoIncierto; propagar pérdida de sesión y deadline monotónico desde el consumidor. Reusar readiness solo ligado a generación de sandbox.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Crear varias sesiones en sandbox listo no repite una prueba real por cada comando; tras pérdida se invalida readiness.
- [x] #2 Una llamada bloqueada respeta el presupuesto temporal y no reejecuta un comando aceptado o de resultado incierto.
- [x] #3 Cleanup fallido queda registrado para reconciliación también en cancelación; se elimina el camino al puerto 0.
- [x] #4 Medir llamadas HTTP/WS y p50/p95 en fake local comparado con baseline; mantener smoke real de TASK-20.
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Aceptada por el supervisor tras corregir los cuatro hallazgos goldfish. AC1: readiness cacheada por generación e invalidada al perderla (tests de oneshot). AC2: ensure_ready rechaza un presupuesto agotado aunque la readiness esté cacheada; test con Deadline::in_budget(ZERO) y recuento del fake: NotAccepted sin nueva sesión ni comando. AC3: diario durable atomico en $XDG_STATE_HOME/hodei-mh/cleanup-journal.json, con flush en register/reconcile/drop; hodei-mh-verify (runner.rs:123, reconciliación antes del POST, orden de peticiones asertado) y el probe acp (main.rs:304) reconcilian antes de crear; lo que no se libera queda journalizado y se reporta; las sesiones execd dejan de ser objetivo de reconciliación y sobra el SessionRefused. AC4: BASELINE explícito probes=20 creates=40 deletes=40 frente a medido probes=1 creates=21 deletes=21, p50=505.63ms p95=521.62ms, con p50/p95 conservados. Política: sin expect/unwrap de producción en los caminos tocados; locks envenenados recuperados y cliente total. Evidencia: gate --all PASS, CI run 44702550552 SUCCESS, smoke real post-merge 12/12 all_passed en 4.33 s (/tmp/runprobe-task31-merged.json). Integración: merge de origin/main (ba9b878) y adaptación de las llamadas de la caché de TASK-36 a la firma nueva (14288bd); PR #46 mergeado en 67fc6ef. Flake observada bajo carga: socket_integration::a_client_that_does_not_read_is_dropped, pasa aislada 3/3 y en la reejecución.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Un único deadline monótono con readiness cacheada por generación, cleanup durable reconciliado antes de crear sandboxes y medición con baseline, sin expect/unwrap en producción.
<!-- SECTION:FINAL_SUMMARY:END -->
