---
id: TASK-47
title: Referencia CLI de hodei-mh en docs/cli-reference.md
status: Done
assignee: []
created_date: '2026-11-20T17:39:00Z'
updated_date: '2026-11-20T17:50:00Z'
labels: []
dependencies: []
references:
  - crates/hodei-mh/src/main.rs
priority: high
type: docs
ordinal: 47000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Documentar cada subcomando product de hodei-mh (doctor, profile-run, supervisor [rehearsal/live], sup-status, sup-action, sup-events, attach-terminal, tui, package): propósito, flags, salida, códigos de exit y refusals. Fuente de verdad: USAGE en crates/hodei-mh/src/main.rs:57-98. No inventar flags.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Cubre todos los subcomandos product presentes en USAGE sin inventar ninguno
- [x] #2 Documenta flags, salida, códigos de exit y refusals por comando
- [x] #3 Enlaza a docs/supervisor-protocol.md para el wire contract
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 pipelinek validate pipeline.kts pasa
- [x] #2 scripts/fast-check.sh --all pasa
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T17:50:00Z

---

Evidencia (worker a4ad8a7f): rama task/task-47-cli-reference (base 2f40a89), HEAD 8c7e156, docs/cli-reference.md 554 líneas. Scope: SOLO docs/cli-reference.md (git diff 2f40a89..ramar solo ese fichero). fast-check --all PASS (exit 0). pipelinek validate pipeline.kts SUCCESSFUL exit 0. pipelinek run exit 0, RunFinished.outcome=success, run 94278168. git-control commit ALLOW exit 0. Verificación supervisor: doc describe el comportamiento implementado (bind+serve, refusar start con 'no runtime is wired'), no la paráfrasis de USAGE ('refuses to start'). Enlaces a supervisor-protocol.md y profiles.md correctos; configuration.md enlazada como pending (TASK-49). Merge --no-ff a main @ HEAD post-merge 3291677, fast-check PASS. AC/DoD verificados por el supervisor.

---
<!-- COMMENTS:END -->
