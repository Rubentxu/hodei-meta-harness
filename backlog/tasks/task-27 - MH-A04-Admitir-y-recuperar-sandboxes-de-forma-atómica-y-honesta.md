---
id: TASK-27
title: '[MH-A04] Admitir y recuperar sandboxes de forma atómica y honesta'
status: To Do
assignee: []
created_date: '2026-11-18T11:49:00Z'
labels:
  - audit-v0.2
  - lifecycle
  - budget
  - p0
dependencies: []
references:
  - crates/lifecycle-server/src/service.rs
  - crates/lifecycle-server/src/compose.rs
  - crates/hodei-mh-budget/src/ledger.rs
  - crates/hodei-mh-release/src/reconcile.rs
priority: high
ordinal: 27000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0. service.rs:86-146 cuenta contenedores antes de await sin reservar; :243-255 devuelve IDs aunque DELETE falle. Reservar capacidad antes de efectos, validar límites finitos y TTL, idempotencia server-side por clave de creación y reconciliar fallos. No duplicar ledger de presupuesto: elegir un propietario de slots y presupuesto agregado. TASK-12/13/18. Contabilizar reservas pendientes más contenedores existentes, con CPU/memoria/slots efectivos. Mantener una autoridad de admisión en lifecycle, presupuestos de objetivos en coordinator. No prometer exactamente-una-vez en red: idempotencia por clave estable y reconciliación del resultado incierto.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Con capacidad 1 y dos altas simultáneas solo se admite una; recursos se liberan tras fallo y tras reinicio reconciliado.
- [ ] #2 Dos solicitudes con la misma clave y cuerpo no crean dos sandboxes; conflictos de cuerpo se rechazan.
- [ ] #3 Un fallo DELETE no aparece como liberado y se reintenta de forma acotada; no se eliminan entornos ajenos.
- [ ] #4 NaN, infinito, cero inválido y valores desbordados se rechazan; prueba concurrente determinista y un smoke real.
<!-- AC:END -->
