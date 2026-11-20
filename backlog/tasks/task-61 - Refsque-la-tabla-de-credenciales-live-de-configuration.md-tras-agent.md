---
id: TASK-61
title: Refsque la tabla de credenciales live de configuration.md tras --agent
status: To Do
assignee: []
created_date: '2026-11-20T23:23:00Z'
labels:
  - descubrimiento
dependencies: []
priority: medium
type: docs
ordinal: 61000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
TASK-59 añadió AgentRuntime con --agent claude-code|qoder. docs/configuration.md:115 sigue citando crates/hodei-mh/src/live/agent.rs:208-217 (hoy cae sobre el Debug impl; la lectura real es Credentials::from_env vía runtime.credential_env()) y describe ANTHROPIC_AUTH_TOKEN como 'required for the live loop' sin mencionar QODER_PERSONAL_ACCESS_TOKEN para --agent qoder. Actualizar la fila y añadir la fila QODER_PERSONAL_ACCESS_TOKEN con anclaje verificado; revisar también el snippet de troubleshooting de README.md (exacto para el default, puede ganar una nota).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Los anclajes de la tabla de credenciales live apuntan a la línea que lee el entorno en el código actual (verificado con sed/grep real)
- [ ] #2 QODER_PERSONAL_ACCESS_TOKEN figura con su canal ProcessEnvOnly y su refusal cuando está vacío
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 fmt+clippy+fast-check del crate afecto en verde
- [ ] #2 Evidencia de anclajes reales registrada en la tarea
<!-- DOD:END -->
