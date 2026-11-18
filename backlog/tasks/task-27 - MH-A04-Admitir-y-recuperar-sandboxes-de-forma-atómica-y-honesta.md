---
id: TASK-27
title: '[MH-A04] Admitir y recuperar sandboxes de forma atómica y honesta'
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-18T11:49:00Z'
updated_date: '2026-11-18T22:56:00Z'
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

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
6. Verificar: pruebas offline (admision concurrente determinista con un daemon simulado, conflicto de clave, liberacion tras fallo y tras reinicio reconciliado, fallo de DELETE), cargo fmt, cargo clippy -D warnings, scripts/fast-check.sh y un smoke real en la pista de sondas (creates concurrentes contra el presupuesto real y la misma clave de creacion).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Núcleo implementado y compilando en la rama task/task-27-atomic-admission (commit 239b8d6, pusheada). QUEDA: (1) pruebas offline con dientes — admision concurrente determinista (un daemon simulado que acepta y no responde: la primera creacion queda en vuelo con su slot reservado y la segunda recibe 429 BUDGET_EXHAUSTED; al abandonar la primera, la reserva se suelta); conflicto de clave (misma clave + cuerpo distinto -> 409 SANDBOX::CREATION_CONFLICT) y misma clave + mismo cuerpo -> un solo entorno; liberacion de la reserva tras fallo y tras reinicio reconciliado; fallo de DELETE (reintento acotado, el fallo NO aparece como liberado). (2) pruebas de compose: huella de creacion (misma intencion -> misma huella; intencion distinta -> huella distinta), labels de la clave, parsers que rechazan NaN/infinito/cero/desbordamiento, TTL de 0 rechazado. (3) smoke real en la pista de sondas: creates CONCURRENTES contra el presupuesto real (con el codigo viejo se admiten todos; con el nuevo, uno y 429 para el resto) y la misma clave de creacion (un solo sandbox). (4) documentacion (docs/profiles.md + README) y el cliente (profile_run y el runner de verify mandan creationKey = su marca unica). (5) gates: fmt, clippy -D warnings, scripts/fast-check.sh y la pista de sondas; luego PR y cierre.

Decisiones ya tomadas (no volver a derivarlas): el lifecycle server es el UNICO propietario de los slots y del presupuesto agregado; la REGLA de los slots sigue en hodei-mh-budget (admissible_creates) y el coordinador conserva los presupuestos de objetivo — no hay un segundo ledger. La autoridad cuenta entornos EXISTENTES + reservas PENDIENTES con CPU/memoria/slots EFECTIVOS (leidos del contenedor). La reserva se toma ANTES de cualquier efecto y la suelta un guard RAII si la creacion falla o se abandona; la reconciliacion (arranque y barrido) reconstruye el estado desde el daemon y usa un horizonte de secuencia para no borrar un entorno que se acaba de crear. La idempotencia es por clave de creacion en el cuerpo (creationKey) con la huella sha256 de la intencion (sin la credencial de la imagen); la clave y la huella van como labels (hodei.io/creation-key, hodei.io/creation-fingerprint) para que sobrevivan al reinicio.
<!-- SECTION:NOTES:END -->
