---
id: TASK-43
title: Refusal sin efecto no debe dejar el checkpoint Pending permanentemente
status: Done
assignee: []
created_date: '2026-11-20T14:45:00Z'
updated_date: '2026-11-20T15:56:00Z'
labels:
  - descubrimiento
dependencies: []
priority: high
ordinal: 43000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Cuando un turno termina como refusal y NO hubo efecto (sin commit, sin promoción, sin cierre), el registro de intento queda en Pending y cualquier Start nuevo se rechaza con "resolve the record explicitly" (crates/hodei-mh/src/live/checkpoint.rs:365-377). Esto contradice el invariante que TASK-35 fix 3 fijó de forma conservadora: Pending estaba pensado para cubrir un efecto realmente ambiguo (crash entre publish y confirm), no un refusal terminal sin candidato.
En la UAT run 5 el reintento Epoch 2 de A y D falló por esto antes de tocar el check (A2_events.jsonl, D2_events.jsonl).
Requisito observable: si el terminal es refusal con sha_before==sha_after y claims vacíos y ningún candidate.bundle escrito, el run debe liberar el Pending como "settled-no-effect" automáticamente y permitir un Start nuevo sin intervención humana. Mantener Pending bloqueante solo para Uncertain/PromotedUnconfirmed donde sí hubo efecto posible.
<!-- SECTION:DESCRIPTION:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Worker original agotó turnos con WIP heredado 9af2566 (adoptado y rebaseado 840c61a). Worker de continuación cerró con 3a5c32c. Merge 27670e4; fast-check --all PASS; cargo test --workspace 738/738.
Diseño entregado: LivePhase::SettledNoEffect + Recovery::SettledNoEffect en checkpoint.rs; AttemptRecordPort::recover mapea SettledNoEffect → Ok (checkpoint.rs:444); LiveCheckpoints::settle_no_effect escribe Pending→SettledNoEffect con handoff NO_EFFECT_REFUSAL reutilizando write_private 0600+sync_all, y rechaza si el Pending ya observó candidato (NotEffectless). Runner/live.rs añade la ruta refusal → settle_no_effect en AttemptEnd::AgentFailed con causa refusal sin efecto (publisher 0, claims liberado, sin candidate bundle); UncertainRelease y PromotedUnconfirmed siguen bloqueando (el invariante original preservado).
Pruebas nuevas en WIP+continuación (checkpoint.rs): a_settled_refusal_replaces_the_pending_phase_and_is_visible_in_record, a_settled_refusal_opens_the_next_attempt_and_rewrites_the_handoff (arreglada en 3a5c32c: la aserción del handoff se mueve al punto donde settle la escribe, antes del segundo begin_attempt que legítimamente la sobrescribe), unsettled_pending_refusal_still_recovers_as_uncertain, settle_refuses_a_record_that_observed_a_candidate. Runner/live.rs: a_refusal_without_effects_settles_the_record_and_the_next_start_is_accepted (2764), con RefusalAgent + PendingRecords offline, segundo runtime CountingAgent confirma sin intervención humana.
TASK-42 tests en agent.rs preservadas (a_reviewed_tool_is_refused_with_its_reason :1242, an_unclassified_tool_kind_runs_in_the_sandbox_unless_the_operator_denies :1257). Rebase sobre main 6e3af5a sin conflictos.
Métricas: +618/−45 en 6 ficheros; WIP heredado 90% del trabajo, continuación ~13 min y 59 tool uses; gate total en main ~2 min.
<!-- SECTION:NOTES:END -->
