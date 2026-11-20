---
id: TASK-48
title: Quickstart narrado del bucle de objetivos en docs/quickstart.md
status: Done
assignee: []
created_date: '2026-11-20T17:39:00Z'
updated_date: '2026-11-20T18:13:00Z'
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
- [x] #1 Explica la ruta rehearsal con comandos copiables y su salida esperada
- [x] #2 Explica la ruta live con lifecycle-server y todas sus flags
- [x] #3 Deja claro que las credenciales viajan solo por el entorno (ProcessEnvOnly)
- [x] #4 Cita los anclajes reales de main.rs sin inventar flags
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 pipelinek validate pipeline.kts pasa
- [x] #2 scripts/fast-check.sh --all pasa
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T18:13:00Z

---

Evidencia (supervisor): rama task/task-48-quickstart @0a2de1f (base 1014a4b), solo docs/quickstart.md (411 líneas). Gate worker: fast-check --all PASS, pipelinek validate SUCCESSFUL. Merge --no-ff @71a3bdb; post-merge fast-check PASS + validate SUCCESSFUL. Supervisor spot-check: 7 flags --live verificadas contra USAGE main.rs:73-79, ProcessEnvOnly citado, sin secretos en claro, salidas rehearsal capturadas de ejecuciones reales. Discrepancias USAGE-vs-código (runtime sin flags, --project requerido en profile-run) documentadas honestamente, no enmascaradas. Nota: worker compartió árbol con TASK-49; commit desviado fue corregido por el propio worker y el estado final fue verificado por el supervisor antes del merge.

---
<!-- COMMENTS:END -->
