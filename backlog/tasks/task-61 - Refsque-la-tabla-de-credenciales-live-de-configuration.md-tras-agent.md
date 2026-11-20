---
id: TASK-61
title: Refsque la tabla de credenciales live de configuration.md tras --agent
status: Done
assignee: []
created_date: '2026-11-20T23:23:00Z'
updated_date: '2026-11-20T23:33:00Z'
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
- [x] #1 Los anclajes de la tabla de credenciales live apuntan a la línea que lee el entorno en el código actual (verificado con sed/grep real)
- [x] #2 QODER_PERSONAL_ACCESS_TOKEN figura con su canal ProcessEnvOnly y su refusal cuando está vacío
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 fmt+clippy+fast-check del crate afecto en verde
- [x] #2 Evidencia de anclajes reales registrada en la tarea
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T23:33:00Z

---

Aceptado por el supervisor. Worker w61 a594ae1: diff exacto README.md + docs/configuration.md (docs-only). Verificación independiente del supervisor sobre main post-merge (b8031f3): agent.rs:219-220 = Credentials::from_env leyendo runtime.credential_env(); runtime.rs:139 credential_env QODER_PERSONAL_ACCESS_TOKEN; runtime.rs:142 base_url_env None; main.rs:531 y supervisor_probe.rs:650 coinciden con los nuevos anclajes. El ancla vieja agent.rs:208-217 caía sobre el Debug impl (staleness confirmado antes de delegar). Fila QODER con ProcessEnvOnly + refusal MissingCredentials que nombra la variable; matriz de precedencia actualizada. scripts/fast-check.sh --all post-merge PASS.

---
<!-- COMMENTS:END -->
