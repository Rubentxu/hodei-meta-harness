---
id: TASK-35
title: >-
  [MH-A13] Reanudar objetivos y conversaciones sin conservar sandboxes
  innecesarios
status: To Do
assignee: []
created_date: '2026-11-18T11:50:00Z'
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
