---
id: TASK-33
title: >-
  [MH-A12] Reconciliar promoción Git y cierre de ticket con una intención
  durable
status: To Do
assignee: []
created_date: '2026-11-18T11:50:00Z'
labels:
  - audit-v0.2
  - reliability
  - promotion
  - tracker
  - p0
dependencies:
  - TASK-24
  - TASK-26
references:
  - crates/hodei-mh-promote/src/ledger.rs
  - crates/hodei-mh-promote/src/git.rs
  - crates/hodei-mh-promote/src/writer.rs
  - crates/hodei-mh-tracker/src/backlog.rs
  - crates/hodei-mh/src/autonomous_probe.rs
priority: high
ordinal: 33000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0 · F02/F03 · TASK-10. ledger.rs:103-111 solo serializa; el estado Planning no incluye candidato esperado. writer.rs:34-56 y :98-118 usa texto plain para releer cierre mientras tracker usa JSON versionado. Integrar intención durable en store existente, con SHA candidato y base esperados, work_ref y epoch. Si cae después de update-ref pero antes de persistir, releer ref; si cae después de modificar Backlog, releer estado/AC y evidencia sin duplicar nota. Mantener Git y Backlog como autoridades distintas, no una transacción distribuida ficticia.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Crash inyectado entre CAS y registro recupera exactamente el SHA esperado; ref ajeno no se adopta ni sobrescribe.
- [ ] #2 Crash entre actualización del tracker y registro se reconcilia leyendo JSON de Backlog; no duplica cierre ni notas identificadas por operación.
- [ ] #3 El proyecto, ticket, epoch y SHA verificados se contrastan antes de cerrar; conflicto real se registra y respeta cambios humanos.
- [ ] #4 Tests con repo Git temporal y fake tracker para todas las fronteras de caída, más un contrato contra CLI real sin LLM.
<!-- AC:END -->
