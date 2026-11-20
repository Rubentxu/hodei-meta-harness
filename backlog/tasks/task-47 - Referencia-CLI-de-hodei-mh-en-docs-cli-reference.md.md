---
id: TASK-47
title: Referencia CLI de hodei-mh en docs/cli-reference.md
status: To Do
assignee: []
created_date: '2026-11-20T17:39:00Z'
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
- [ ] #1 Cubre todos los subcomandos product presentes en USAGE sin inventar ninguno
- [ ] #2 Documenta flags, salida, códigos de exit y refusals por comando
- [ ] #3 Enlaza a docs/supervisor-protocol.md para el wire contract
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 pipelinek validate pipeline.kts pasa
- [ ] #2 scripts/fast-check.sh --all pasa
<!-- DOD:END -->
