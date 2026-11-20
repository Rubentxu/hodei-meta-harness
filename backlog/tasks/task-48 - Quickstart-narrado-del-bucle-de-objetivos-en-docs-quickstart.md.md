---
id: TASK-48
title: Quickstart narrado del bucle de objetivos en docs/quickstart.md
status: To Do
assignee: []
created_date: '2026-11-20T17:39:00Z'
labels: []
dependencies: []
references:
  - crates/hodei-mh/src/main.rs
  - docs/supervisor-protocol.md
priority: high
type: docs
ordinal: 48000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Walkthrough de un ciclo real: ruta rehearsal (sin Docker/red/modelo) con supervisor --rehearsal, sup-action start, sup-status, sup-events --jsonl; y ruta live con lifecycle-server, profile-run --profile rust y supervisor --live con sus flags (--server --image --project-dir --tracker-dir --managed-branch, --proxy-url, --permission). Credenciales solo por entorno. Fuente de verdad: USAGE y main.rs.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Explica la ruta rehearsal con comandos copiables y su salida esperada
- [ ] #2 Explica la ruta live con lifecycle-server y todas sus flags
- [ ] #3 Deja claro que las credenciales viajan solo por el entorno (ProcessEnvOnly)
- [ ] #4 Cita los anclajes reales de main.rs sin inventar flags
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 pipelinek validate pipeline.kts pasa
- [ ] #2 scripts/fast-check.sh --all pasa
<!-- DOD:END -->
