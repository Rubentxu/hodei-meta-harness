---
id: TASK-26
title: '[MH-A03] Hacer durables los claims y efectos antes de confirmarlos'
status: To Do
assignee: []
created_date: '2026-11-18T11:49:00Z'
labels:
  - audit-v0.2
  - reliability
  - coordinator
  - p0
dependencies: []
references:
  - crates/hodei-mh-coordinator/src/actor.rs
  - crates/hodei-mh-coordinator/src/core.rs
  - crates/hodei-mh-coordinator/src/store.rs
  - crates/hodei-mh-promote/src/ledger.rs
priority: high
ordinal: 26000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0 · F03/F12 · TASK-4. actor.rs:88-141 ignora save, puede conceder claim tras rollback y ejecutar efectos no persistidos. core.rs:89-96 acepta epochs futuros y :136-139 permite reemitir estados no inciertos; store.rs:48-85 reescribe colecciones. Usar escritor único real con exclusión entre procesos y transacción antes de responder, estado por proyecto fuera del workspace, fencing exacto por intento. No añadir un segundo ledger de trabajo; Backlog sigue siendo autoridad del ticket.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Fallo inyectado de escritura no devuelve Granted, Released ni dispara efecto; el error es observable.
- [ ] #2 Dos coordinadores sobre la misma identidad no conceden el mismo claim; tickets de igual ID en distintos proyectos no colisionan.
- [ ] #3 Eventos de epoch viejo o futuro sin autorización y effect IDs duplicados/conflictivos no ejecutan ni alteran el intento actual.
- [ ] #4 Reinicio recupera claims e intenciones pendientes como reconciliables; prueba SQLite temporal y fallo de persistencia sin Docker.
<!-- AC:END -->
