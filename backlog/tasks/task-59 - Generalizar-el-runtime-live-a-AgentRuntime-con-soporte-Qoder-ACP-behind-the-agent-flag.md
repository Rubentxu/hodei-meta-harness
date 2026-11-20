---
id: TASK-59
title: >-
  Generalizar el runtime live a AgentRuntime con soporte Qoder ACP behind the
  --agent flag
status: Done
assignee: []
created_date: '2026-11-20T21:46:00Z'
updated_date: '2026-11-20T23:23:00Z'
labels: []
dependencies:
  - TASK-58
priority: high
type: feature
ordinal: 59000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Slice de código sobre crates/hodei-mh/src/live/ según las conclusiones del spike TASK-58: (1) ADT AgentRuntime { ClaudeCodeAcp, QoderAcp } en el núcleo del live con datos por variante (artefacto a instalar, ruta de ejecución, comando de agente, nombre de la variable de credencial); default ClaudeCodeAcp para no cambiar el comportamiento publicado. (2) Flag --agent claude-code|qoder en supervisor --live, parseado en el borde, valor inválido rechazado con mensaje específico. (3) El wrapper de credenciales se generaliza: exporta SOLO por entorno la variable que declare la variante (ANTHROPIC_AUTH_TOKEN o PAT de Qoder), mantiene ProcessEnvOnly, argv limpio. (4) Instalación del artefacto Qoder en la caja reutilizando el patrón ccacp (subida verificada, extracción, test -x) con la forma decidida en el spike. (5) Egreso: el profile/gate exige las variables de dominio que el live inyecta al proxy sin hardcodar dominios en el core. Tests offline con fakes ACP para ambas variantes (initialize/session/new/permission), sin proveedor ni Docker; un test negativo que un valor --agent inventado se rechaza. Actualizar docs/cli-reference.md, docs/quickstart.md (escalón live) y README troubleshooting solo lo necesario.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 qoder como agente live es seleccionable con --agent qoder sin tocar código de Claude Code; default intacto
- [x] #2 ProcessEnvOnly se mantiene para la credencial Qoder: prohibido argv, prohibido registro; test que lo afirma
- [x] #3 Los tests ACP corren offline con fakes para las dos variantes; ningún test duerme
- [x] #4 --agent valor-desconocido refusa con exit y mensaje específicos
- [x] #5 cli-reference y quickstart documentan la nueva flag con anclajes reales
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 scripts/fast-check.sh --all pasa
- [x] #2 cargo clippy --workspace --all-targets -- -D warnings pasa
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T23:23:00Z

---

Aceptado por el supervisor. Worker w59 READY_FOR_REVIEW @825a49d; diff --name-only 5f3f6a5..HEAD = superficie contratada exacta (live/runtime.rs nuevo, agent.rs, mod.rs, session.rs, main.rs USAGE, cli-reference, quickstart). Verificación independiente: ADT AgentRuntime{ClaudeCodeAcp,QoderAcp}+AgentArtifact con from_cli_value/expected_values; pines reales Qoder 1.1.94 (url+sha256+manifest, no inventados); ancla de cero-regresión claude_code_keeps_the_published_launch_command byte-a-byte vs v0.1.2; tests offline citados existen (runtime.rs:329, agent.rs:1261, mod.rs:443). Evidencia worker: fast-check --all PASS, clippy -D warnings limpio, 169+14 tests ok, refusals reales (--agent codex exit 1; qoder sin PAT refusal específico; default intacto). Merge --no-ff 450b321 a main; scripts/fast-check.sh --all post-merge exit 0 (fast-check: PASS). Descubierto registrado como TASK-61 (configuration.md staleness de anclajes pre-–agent). Pendiente honesto: layout interno del tarball qodercli no observado (test -x refuerza loudly) y prompt/streaming+PAT real = TASK-60.

---
<!-- COMMENTS:END -->
