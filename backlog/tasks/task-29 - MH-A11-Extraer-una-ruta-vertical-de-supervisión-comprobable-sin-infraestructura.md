---
id: TASK-29
title: >-
  [MH-A11] Extraer una ruta vertical de supervisión comprobable sin
  infraestructura
status: To Do
assignee: []
created_date: '2026-11-18T11:50:00Z'
labels:
  - audit-v0.2
  - product
  - architecture
  - testing
  - p0
dependencies: []
references:
  - crates/hodei-mh/src/supervisor_cli.rs
  - crates/hodei-mh-supervisor/src/engine.rs
  - crates/hodei-mh/src/autonomous_probe.rs
  - crates/hodei-mh-goal/src/selection.rs
  - crates/hodei-mh-run/src/run.rs
priority: high
ordinal: 29000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0 · F01 · TASK-5/6/11/15. Extraer control de un GoalRunner de aplicación desde el flujo existente: seleccionar objetivo/trabajo y ejecutar mediante puertos ACP, lifecycle, tracker y workspace actuales. Primero modo de ensayo explícito sin efectos externos: mismo binario y mismo flujo, adaptadores fake deterministas, nunca afirmar que un fake es una ejecución real. Dos entregas: contrato ejecutable y posteriormente MH-A06 ACP real. No crear crates por cada trait ni duplicar RunState.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 CLI/TUI observan la misma máquina de estado; Start transita a Starting y Running únicamente por confirmación del runtime de ensayo o real identificado.
- [ ] #2 Secuencia Start, Pause, Resume, Stop y error queda demostrada por un único test de producto con fake ACP y tracker; sin invocar Docker, red ni LLM.
- [ ] #3 No hay nueva ruta de promoción/escritura en modo ensayo; estados de simulación están explícitos y no marcan tickets reales Done.
- [ ] #4 Se elimina duplicación de decisiones entre supervisor y el flujo extraído, conservando compatibilidad de los comandos públicos o migración documentada.
<!-- AC:END -->
