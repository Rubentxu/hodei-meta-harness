---
id: TASK-34
title: '[MH-A06] Conectar el supervisor de producto al bucle real de objetivos ACP'
status: To Do
assignee: []
created_date: '2026-11-18T11:50:00Z'
labels:
  - audit-v0.2
  - product
  - architecture
  - p0
dependencies:
  - TASK-29
  - TASK-33
  - TASK-25
  - TASK-27
  - TASK-28
  - TASK-30
  - TASK-31
references:
  - crates/hodei-mh/src/supervisor_cli.rs
  - crates/hodei-mh-supervisor/src/engine.rs
  - crates/hodei-mh/src/autonomous_probe.rs
  - crates/hodei-mh/src/multiproject_probe.rs
priority: high
ordinal: 34000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0 producto · F01 · TASK-5/6/11/15/17. Completar el slice iniciado en MH-A11: conectar ACP real y lifecycle al GoalRunner, reutilizando selección, claims, workspaces, verificación, promoción y cierre existentes. autonomous_probe.rs crea fixture interno y main.rs expone caminos separados. Mantener un agente por run y un flujo por objetivo; no añadir scheduler distribuido, plugins nuevos ni otro tracker. Registrar límites de backend (por ejemplo Renew no soportado) sin presentar una transición simulada como éxito.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Un repo externo con Backlog.md y dos tickets dependientes se ejecuta mediante CLI de producto y ACP real; el segundo recibe el commit promovido del primero.
- [ ] #2 Start/Pause/Resume/Stop corresponden a efectos confirmados u objetivos explícitamente pendientes/inciertos; Renew solo confirma si backend lo confirma, si no Unsupported.
- [ ] #3 Candidato fallido o permiso negado no promociona ni cierra; objetivo sin trabajo no consume sandbox ni tokens.
- [ ] #4 La UAT usa el propio binario, conserva evidencia de SHA, cierre, cancelación y liberación; los probes equivalentes se sustituyen solo tras comprobar paridad.
<!-- AC:END -->
