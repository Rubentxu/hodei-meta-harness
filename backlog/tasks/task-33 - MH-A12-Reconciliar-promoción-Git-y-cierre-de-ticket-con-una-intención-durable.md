---
id: TASK-33
title: >-
  [MH-A12] Reconciliar promoción Git y cierre de ticket con una intención
  durable
status: Done
assignee:
  - '@rubentxu'
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-19T22:53:00Z'
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
- [x] #1 Crash inyectado entre CAS y registro recupera exactamente el SHA esperado; ref ajeno no se adopta ni sobrescribe.
- [x] #2 Crash entre actualización del tracker y registro se reconcilia leyendo JSON de Backlog; no duplica cierre ni notas identificadas por operación.
- [x] #3 El proyecto, ticket, epoch y SHA verificados se contrastan antes de cerrar; conflicto real se registra y respeta cambios humanos.
- [x] #4 Tests con repo Git temporal y fake tracker para todas las fronteras de caída, más un contrato contra CLI real sin LLM.
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Intencion durable (`crates/hodei-mh-promote/src/intent.rs`): `PromotionIntent` con los hechos inmutables de un intento de promocion: `operation_id` (derivado: `promote:{work_ref}:{epoch}:{candidate}`), `project`, `work_ref`, `epoch`, `managed_branch`, `expected_base` (SHA), `expected_candidate` (SHA), `target_status` y `update` (nota con la marca `[hodei-mh:op:{operation_id}]`, ACs, resumen). Los SHA se parsean con `verify::git::GitRevision` (parse, don't validate).
2. Store existente (`crates/hodei-mh-coordinator`): el intent de efecto gana una columna `payload` (JSON opaco), `EffectIntent.payload: Option<String>`, con `record_effect_with_payload`, `effect_payload` y `update_effect_payload`, y migracion `ALTER TABLE` para las bases ya existentes. La promocion se registra como intent de efecto del claim del ticket: `effect_id` = `operation_id`, `ticket_id` = `work_ref`, `epoch` = `epoch`, `payload` = JSON de la intencion. Un solo store, una sola autoridad; el store ya se niega a abrirse como otro proyecto.
3. Observacion por JSON versionado (`crates/hodei-mh-promote/src/writer.rs`): `read_ticket` usa `backlog task view <id> --json` y devuelve un `TicketSnapshot` con `id`, `status`, `checked_ac`, `notes` y `final_summary`; el parseo de texto plano se borra (era la mitad "texto plano" del contrato). `apply` sigue anadiendo la nota, marcando ACs y fijando estado y resumen; `append_note` escribe SOLO la nota (evidencia) y se usa cuando el ticket ya esta cerrado por una persona: su resumen humano no se pisa.
4. Decision pura (`crates/hodei-mh-promote/src/intent.rs`): `decide(intent, ref_observada, ticket_observado, epoch_actual)` devuelve `Promote` | `AlreadyPromoted` | `Close` | `AlreadyClosed` | `Conflict(RefMoved | EpochStale | ProjectMismatch | ShaMismatch)`. La fase NO se persiste: se deriva de la observacion de las dos autoridades (la ref de git y el JSON del ticket), que es lo que exige reconciliar tras una caida.
5. Flujo (`crates/hodei-mh-promote/src/flow.rs`): write-ahead de la intencion, observacion, decision, efecto (CAS o cierre), re-observacion, verificacion y registro del desenlace. Cada paso es idempotente: una caida en cualquier punto se reanuda sin segundo commit y sin nota duplicada.
6. Verificacion: unitarios de la tabla de decision, del parser JSON y de la serializacion de la intencion; integracion con repo Git temporal y tracker falso para las fronteras de caida (caida tras el CAS, caida tras el cierre, ref ajena, epoch caduca); contrato contra la CLI real sin LLM (proyecto temporal con `backlog init` y tarea real) que se salta ruidosamente si la CLI no esta instalada (el carril rapido de CI no la instala).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementado en `crates/hodei-mh-promote` (`intent.rs`, `flow.rs`, `journal.rs`, `writer.rs`) y `crates/hodei-mh-coordinator` (`store.rs`, `core.rs`).

- `PromotionIntent` (intent.rs) guarda los hechos inmutables del intento: `operation_id` derivada de `{work_ref}:{epoch}:{candidate}`, proyecto, `work_ref`, `epoch`, rama gestionada, SHA base y candidato esperados, estado destino y la nota con la marca `[hodei-mh:op:{operation_id}]`. Los SHA se parsean con `verify::git::GitRevision`.
- El coordinador lo guarda como payload opaco del intent de efecto (`EffectIntent.payload`, columna `payload` con migracion `ALTER TABLE`): un solo store, una sola autoridad.
- `decide()` es una funcion pura que deriva la fase de la observacion de las dos autoridades (ref de git y JSON del ticket) en vez de una fase persistida: `Promote | AlreadyPromoted | Close | AlreadyClosed | Conflict(RefMoved|EpochStale|ProjectMismatch|ShaMismatch)`.
- `read_ticket` usa `backlog task view --json` (`TicketSnapshot`); el parseo de texto plano desaparece. `append_note` anade solo evidencia cuando el ticket ya esta cerrado por una persona.
- `flow.rs` es idempotente con write-ahead (`journal.rs`): write-ahead, observacion, decision, efecto, re-observacion, verificacion y registro.

Evidencia de verificacion: 34 unitarias (tabla de decision, parser JSON, serializacion de la intencion), `tests/promotion_recovery.rs` con repo Git temporal y tracker falso (7: caida tras la promocion recupera el mismo commit, caida tras el cierre no duplica cierre ni nota, ref movida no se adopta ni se sobrescribe, una promocion aplicada no se deshace por un refusal posterior), `tests/backlog_cli_contract.rs` contra la CLI real sin LLM (2). Gate local `scripts/fast-check.sh` sobre los crates tocados: PASS. `workspace gate` de CI en PR #42: SUCCESS.

Nota operativa: el mensaje del commit de merge llevaba un trailer `Co-authored-by` prohibido por la politica `git-control`, que bloqueaba el fast-forward local y todo push. Se enmendo el mensaje (mismo arbol, autor reasignado por la politica) y se forzo el push de `main`, que queda en `de5b44a`.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
La promocion Git y el cierre del ticket se reconcilian desde una intencion durable. El coordinador conserva los hechos inmutables del intento (proyecto, work_ref, epoch, rama, SHA base y candidato esperados) y cada paso del flujo es idempotente: una caida tras el CAS recupera exactamente el SHA esperado y una caida tras el cierre relee el JSON de Backlog.md sin duplicar cierre ni nota. Un conflicto real (ref movida, epoch caducada, proyecto o SHA distintos, ticket tocado por una persona) se registra y no se adopta ni se sobrescribe nada, y el cierre humano no se pisa. Git y Backlog.md siguen siendo autoridades distintas, no una transaccion distribuida.

Verificacion: 34 unitarias, 7 de recuperacion con repo Git temporal y tracker falso, 2 de contrato contra la CLI real sin LLM, gate local `scripts/fast-check.sh` en PASS y `workspace gate` de CI en SUCCESS. Merge: PR #42 (`de5b44a`).
<!-- SECTION:FINAL_SUMMARY:END -->
