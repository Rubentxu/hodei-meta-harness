---
id: TASK-42
title: Permitir tool kinds desconocidos en allow-once sin humano (bucle vivo)
status: To Do
assignee: []
created_date: '2026-11-20T14:45:00Z'
labels:
  - descubrimiento
dependencies: []
priority: high
ordinal: 42000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
El agente ACP real emite tool_calls con kinds que acp_tool_kind no clasifica y cae en ToolKind::Unknown. sandbox_first lo manda a requires_human; sin humano disponible el turno termina como refusal terminal. En la UAT run 5 (main 6972e45) mató TASK-2 del encadenamiento AC1 y TASK-1 del escenario D AC3(i). El requisito observable: allow-once debe permitir un comando de lectura/escritura dentro del sandbox aunque el tool kind no esté en la taxonomía; la denegación solo debe activarse cuando la política lo exija, no por ausencia de clasificación.
Evidencia: /tmp/uat-task34/evidence5/A_chaining.txt (interactions: permission granted by the interaction policy; permission refused: tool kind Unknown requires explicit review), A_events.jsonl seq 5, D_events.jsonl y D2_events.jsonl.
Código: crates/hodei-mh/src/live/agent.rs:860-869 (acp_tool_kind → ToolKind::Unknown) y crates/hodei-mh-interact/src/policy.rs:26-32 (sandbox_first → requires_human).
<!-- SECTION:DESCRIPTION:END -->
