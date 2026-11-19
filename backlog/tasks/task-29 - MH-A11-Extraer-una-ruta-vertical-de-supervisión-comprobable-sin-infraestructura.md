---
id: TASK-29
title: >-
  [MH-A11] Extraer una ruta vertical de supervisión comprobable sin
  infraestructura
status: In Progress
assignee:
  - '@rdcabrera'
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-19T01:04:00Z'
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

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. run: Phase::Starting + RuntimeIdentity{kind,id} + RunState::starting/confirm + puerto ObjectiveRuntime (el RunState sigue siendo UNO).
2. supervisor: apply(Start) -> Starting (no Running sin confirmacion); engine::confirm_runtime/refuse_runtime (transiciones del engine, NO acciones de cliente); Snapshot gana runtime y mode (Rehearsal|Live) para que CLI y TUI muestren el estado de simulacion; server::serve recibe el runtime y lanza la ejecucion.
3. runner (crate nuevo): GoalRunner puro con puertos AgentPort/VerifierPort/PublisherPort y reutilizando tracker::TrackerRead, verify::Check/Candidate/EvidenceSet; puerta pura 'sin evidencia no se publica'; rehearsal.rs con TrialTracker/ScriptedAgent/ScriptedVerifier/RecordingPublisher (sin WorkWriter: el ensayo no puede escribir).
4. binario: 'hodei-mh supervisor --rehearsal' cablea el runtime de ensayo; sin el, Start se rechaza honestamente (el ACP real llega con MH-A06).
5. Test de producto UNICO (crates/hodei-mh-runner/tests/product_flow.rs): socket real + cliente real + runtime de ensayo -> Start(Starting) -> confirmacion -> Running -> Pause -> Resume -> Stop + caso de error; el ensayo no marca ningun ticket Done.
6. Gates: scripts/fast-check.sh (afectados + dependientes), PR y merge a main; el gate de workspace lo corre la CI.
<!-- SECTION:PLAN:END -->
