---
id: TASK-14
title: Ejecutar un objetivo personalizado sin issue tracker
status: Done
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-17T20:14:00Z'
labels:
  - gc-mvp
  - gc-14
  - hodei-mh
dependencies:
  - TASK-5
  - TASK-9
ordinal: 14000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
GoalSource distingue TrackerScope y CustomObjective. Objetivo personalizado explícito con criterio de cierre verificable; por defecto tracker backlog. No crear tareas fantasma; objetivo finito completado queda Idle mientras supervisor continúa habilitado. No inventar trabajo recurrente sin fuente de cambios.

Clave de incorporación: GC-14.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Usuario inicia objetivo sin tracker con condición de terminación verificable y obtiene evidencia sin escribir tickets.
- [x] #2 Completar ese objetivo no crea otro automáticamente; nueva edición autorizada puede reactivarlo.
- [x] #3 Un fallo de configuración del tracker no convierte el objetivo por defecto en objetivo personalizado silenciosamente.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Prueba end-to-end con objetivo personalizado y sin CLI Backlog instalada.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Evidencia TASK-14 (rama task/task-14-custom-goal): módulo source.rs en hodei-mh-goal (sin duplicar: reutiliza verify::result para clasificar el registro del criterio) con (1) GoalSource enum TrackerScope{provider} | CustomObjective{edition, TerminationCriterion} — el objetivo custom lleva criterio verificable (comando + exit esperado) y EDICIÓN explícita. (2) advance_custom FSM puro: Active+criterio cumplido→Fulfilled SIN crear nada nuevo (objetivo finito, sin recurrencia inventada); Active+no cumplido→Continue; Fulfilled sin nueva edición autorizada→StayFulfilled; Fulfilled+edición nueva estrictamente mayor autorizada→Reactivated (AC2: nada reactiva automáticamente, la reactivación es edición explícita del usuario). (3) resolve_scope: default+tracker sano→Tracker; default+tracker FALLO→TrackerFailure VISIBLE (AC3: nunca convierte el objetivo por defecto en custom silencioso); custom explícito ignora salud del tracker (el usuario lo pidió). 5 tests nuevos (12 totales en goal). Probe hodei-mh custom-goal-probe (DoD: E2E sin CLI Backlog — el proceso no invoca backlog en ningún punto, cero tickets): criterio real grep sobre deliverable en /tmp → Continue con exit≠0 → aparece el deliverable real → Fulfilled con evidencia (comando+exit+output capturados en ExecutionRecord) sin escribir tickets → StayFulfilled tras completar (no crea otro objetivo) → Reactivated solo con edición autorizada 2 → TrackerFailure visible para scope default con tracker roto. 5/5 checks ×2. Gates globales (10ª tarea completada): cargo fmt --all OK, cargo clippy --workspace --all-targets -D warnings OK, cargo test --workspace 131 passed / 0 failed.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-14 completa: objetivos personalizados sin issue tracker. GoalSource distingue TrackerScope (por defecto) de CustomObjective con criterio de terminación verificable (comando + exit esperado) y ediciones explícitas; el objetivo custom se cumple con evidencia real (ExecutionRecord del criterio) SIN escribir tickets ni invocar CLI de backlog; completar un objetivo finito no crea otro automáticamente — solo una nueva edición autorizada lo reactiva; un fallo de configuración del tracker produce TrackerFailure VISIBLE y jamás reescribe silenciosamente el objetivo por defecto en uno personalizado. Verificación: 5 tests nuevos (12 en goal) + probe E2E 5/5 ×2 sin CLI Backlog en todo el proceso (DoD); gates globales de las 10 tareas completadas: fmt, clippy -D warnings y test --workspace (131 tests) en verde.
<!-- SECTION:FINAL_SUMMARY:END -->
