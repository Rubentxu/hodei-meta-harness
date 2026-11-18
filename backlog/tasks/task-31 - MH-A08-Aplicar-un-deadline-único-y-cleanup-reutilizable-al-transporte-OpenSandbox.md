---
id: TASK-31
title: >-
  [MH-A08] Aplicar un deadline único y cleanup reutilizable al transporte
  OpenSandbox
status: To Do
assignee: []
created_date: '2026-11-18T11:50:00Z'
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
- [ ] #1 Crear varias sesiones en sandbox listo no repite una prueba real por cada comando; tras pérdida se invalida readiness.
- [ ] #2 Una llamada bloqueada respeta el presupuesto temporal y no reejecuta un comando aceptado o de resultado incierto.
- [ ] #3 Cleanup fallido queda registrado para reconciliación también en cancelación; se elimina el camino al puerto 0.
- [ ] #4 Medir llamadas HTTP/WS y p50/p95 en fake local comparado con baseline; mantener smoke real de TASK-20.
<!-- AC:END -->
