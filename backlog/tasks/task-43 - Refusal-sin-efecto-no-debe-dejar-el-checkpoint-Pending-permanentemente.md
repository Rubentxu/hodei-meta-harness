---
id: TASK-43
title: Refusal sin efecto no debe dejar el checkpoint Pending permanentemente
status: To Do
assignee: []
created_date: '2026-11-20T14:45:00Z'
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
