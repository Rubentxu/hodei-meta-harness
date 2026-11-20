---
id: TASK-50
title: Sección Troubleshooting en README.md
status: Done
assignee: []
created_date: '2026-11-20T17:39:00Z'
updated_date: '2026-11-20T18:20:00Z'
labels: []
dependencies: []
references:
  - README.md
priority: high
type: docs
ordinal: 50000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Recetas de fallo común con señal, causa y fix: gate del perfil negado sin proxy de egreso, imagen del perfil ausente en el daemon (doctor no hace fallback), 429 SANDBOX::BUDGET_EXHAUSTED, credencial que no llega al execd (ProcessEnvOnly, argv nunca), y package que rechaza un binario con dynamic loader. Enlazar a docs/cli-reference.md, docs/configuration.md.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Cubre los cinco fallos con señal, causa y fix accionable
- [x] #2 Enlaza a cli-reference.md y configuration.md
- [x] #3 No inventa flags ni rutas; cita anclajes reales
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 pipelinek validate pipeline.kts pasa
- [x] #2 scripts/fast-check.sh --all pasa
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T18:20:00Z

---

Evidencia (supervisor): rama task/task-50-readme-troubleshooting @ecc4020 (base ffb9b40), commit solo README.md (+134 líneas). Gate worker: fast-check PASS, pipelinek validate SUCCESSFUL. Merge --no-ff @b29c404 (absorbe además el commit de tablero 1995f84 que por error del supervisor cayó en esta rama; historia lineal en main, sin cherry-pick pendiente). Post-merge: fast-check PASS, pipelinek validate SUCCESSFUL, pipelinek run exit 0 con Finished: success (run 95025058, events en ~/.local/state/pipelinek/goal-cycle-mh/events-final.ndjson). Spot-check supervisor: 5 recetas con Signal/Cause/Fix exactos (líneas 182/210/234/257/282), enlaces a cli-reference/configuration/quickstart resuelven a ficheros existentes, sin secretos en claro. Nota honesta del worker: el mensaje legible del budget vive en admission.rs:137-144, model.rs:149 es solo la constante de código; la receta documenta el mensaje real compuesto.

---
<!-- COMMENTS:END -->
