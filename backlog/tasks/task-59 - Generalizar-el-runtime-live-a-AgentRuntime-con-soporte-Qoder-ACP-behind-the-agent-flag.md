---
id: TASK-59
title: >-
  Generalizar el runtime live a AgentRuntime con soporte Qoder ACP behind the
  --agent flag
status: To Do
assignee: []
created_date: '2026-11-20T21:46:00Z'
updated_date: '2026-11-20T21:46:00Z'
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
- [ ] #1 qoder como agente live es seleccionable con --agent qoder sin tocar código de Claude Code; default intacto
- [ ] #2 ProcessEnvOnly se mantiene para la credencial Qoder: prohibido argv, prohibido registro; test que lo afirma
- [ ] #3 Los tests ACP corren offline con fakes para las dos variantes; ningún test duerme
- [ ] #4 --agent valor-desconocido refusa con exit y mensaje específicos
- [ ] #5 cli-reference y quickstart documentan la nueva flag con anclajes reales
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 scripts/fast-check.sh --all pasa
- [ ] #2 cargo clippy --workspace --all-targets -- -D warnings pasa
<!-- DOD:END -->
