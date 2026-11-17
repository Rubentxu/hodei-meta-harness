---
id: TASK-10
title: Promover resultados Git y cerrar el ticket sin doble verdad
status: Done
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-17T20:10:00Z'
labels:
  - gc-mvp
  - gc-10
  - hodei-mh
dependencies:
  - TASK-4
  - TASK-9
ordinal: 10000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
WorkWriter host-side mediante CLI oficial; no editar Markdown directamente. Promover a rama local gestionada por proyecto por CAS de ref base y verificar integración. Conservar checkout usuario intacto. Releer ticket antes de cerrar; conservar notas/AC previos. Separar promoción de cierre con outbox y reconciliación sin duplicación. No push/merge/deploy sin grant específico.

Clave de incorporación: GC-10.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Un candidato validado se integra a la rama gestionada y solo entonces se actualizan AC DoD y final-summary mediante CLI; Done usa estado configurado.
- [x] #2 Un cambio de rama destino o ticket desde la verificación exige revalidar; no pierde cambios humanos ni pisa notas.
- [x] #3 Crash tras promover antes de actualizar tracker recupera el mismo commit y no crea otro ni declara Done sin evidencia.
- [x] #4 Error de tracker conserva trabajo técnico y actualizaciones pendientes; no se modifica directamente el Markdown ni se archiva o borra automáticamente.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Prueba de caída entre Git y CLI con relectura real del ticket y branch.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Evidencia TASK-10 (rama task/task-10-promote): crate hodei-mh-promote con (1) ledger.rs — PromotionLedger FSM bifásica Planning→Promoted→TrackerUpdated→Confirmed como OUTBOX write-ahead (persistir antes de cada efecto); recovery clasifica PromoteFromBase / ReconcileTracker{EL MISMO commit} / VerifyClose / AlreadyDone idempotente (AC3: crash tras promover recupera el mismo commit, nunca un segundo commit ni Done sin evidencia). (2) ticket.rs — TicketUpdate validado: nota no vacía, summary no vacío, target_status DEBE coincidir con el estado terminal configurado leído del tracker (AC1). (3) writer.rs — trait WorkWriter síncrono con BacklogWorkWriter (CLI oficial backlog task edit --append-notes/--check-ac/--status/--final-summary; NUNCA Markdown directo) y FakeWorkWriter con inyección de fallos; TicketSnapshot::parse relee status/ACs/notas del texto plano real (AC2: no pisa notas, append-only). (4) git.rs — promoción PLUMBING (rev-parse/fetch a namespace propio refs/hodei-mh//update-ref) que no toca el checkout del usuario; CAS atómico nativo git update-ref <ref> <new> <old>; ff-only con detección BaseMoved (AC2: rama movida tras verificación exige revalidar, cambios humanos intactos) y NotFastForward. 12 tests unitarios. Probe E2E hodei-mh promote-probe sobre proyecto de ejemplo REAL (repo git + backlog init real + ticket real via CLI + bundle candidato): CAS-promote ✓, crash entre git y CLI recargando outbox del disco → ReconcileTracker con el mismo commit ✓, fallo de tracker conservando trabajo técnico y ledger pendiente ✓ (AC4), reconcile via CLI oficial con ACs marcadas y estado Done configurado ✓, relectura real del ticket (status/AC/descripción previa conservada) ✓, relectura de rama al commit persistido + AlreadyDone ✓, BaseMoved rechaza promoción obsoleta sin perder el commit humano ni las notas ✓ (AC2). 9/9 checks ×2 ejecuciones. Gates: fmt/clippy -D warnings/test --workspace verdes (126 tests). Hallazgos: el CLI backlog es all-or-nothing (un --check-ac inválido descarta TODO el edit — los tickets deben nacer con ACs); patrón let-guard temporal sobre MutexGuard indexado puede colgar futex en esta plataforma — usar scopes explícitos + clone.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-10 completa: promoción de resultados Git y cierre de tickets sin doble verdad. Crate hodei-mh-promote: outbox write-ahead bifásico que separa promoción (git) de cierre (tracker) con recovery idempotente — un crash tras promover recupera EL MISMO commit (nunca un segundo commit ni Done sin evidencia); promoción por plumbing git (fetch a namespace propio + update-ref CAS atómico sobre la ref base verificada) que nunca toca el checkout del usuario y sin push/merge sin grant; rama destino movida tras la verificación → BaseMoved exige revalidar conservando cambios humanos; cierre SOLO vía CLI oficial Backlog.md (append-only: notas acumuladas, ACs marcadas individualmente, estado terminal configurado del tracker, no Markdown directo); fallo de tracker conserva el trabajo técnico y las actualizaciones pendientes sin archivar ni borrar nada. Verificación: 12 tests unitarios + probe E2E 9/9 checks ×2 sobre proyecto de ejemplo real (repo git + backlog real + ticket vía CLI) incluyendo la prueba de caída entre Git y CLI con relectura real de ticket y rama (DoD); gates workspace en verde (126 tests).
<!-- SECTION:FINAL_SUMMARY:END -->
