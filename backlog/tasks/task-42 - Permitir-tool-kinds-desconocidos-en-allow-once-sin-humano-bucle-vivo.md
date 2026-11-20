---
id: TASK-42
title: Permitir tool kinds desconocidos en allow-once sin humano (bucle vivo)
status: Done
assignee: []
created_date: '2026-11-20T14:45:00Z'
updated_date: '2026-11-20T14:57:00Z'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Worker entregó en 20a9a5b sobre 730297b. Merge a924663; fast-check --all PASS.
Cambios: crates/hodei-mh-interact/src/policy.rs:22-43 — ToolKind::Unknown se mueve de requires_human a sandbox_routed; requires_human queda vec![Network]. Doc-comment actualizado con el invariante host-caps-vacío.
Colateral mínimo: crates/hodei-mh-interact/src/resolver.rs:110-118 renombró el test de resolución para afirmar RoutedToSandbox(Unknown) + HumanRequired(Network) y no mentir en el gate.
Pruebas: default_policy_never_auto_allows_writes_or_unknown ajustada; unknown_tools_go_to_the_sandbox_and_never_the_host (policy.rs:86-127) afirma clientCapabilities={} y la taxonomía; an_unclassified_tool_kind_runs_in_the_sandbox_unless_the_operator_denies (agent.rs:1249-1272) camino completo con fake ACP: toolCall.kind="other" bajo AllowOnce responde optionId allow_once con detalle "routed to the sandbox" y refusal.is_none(), bajo Deny sale cancelled con agent.permission_refused. a_reviewed_or_unknown_tool_is_refused_with_its_reason se redujo a sólo Network (evita duplicar).
TDD aplicado, clippy forzó quitar un bucle de un solo elemento. Métricas ~8 min, 18 tool uses.
<!-- SECTION:NOTES:END -->
