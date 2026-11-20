---
id: TASK-50
title: Sección Troubleshooting en README.md
status: To Do
assignee: []
created_date: '2026-11-20T17:39:00Z'
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
- [ ] #1 Cubre los cinco fallos con señal, causa y fix accionable
- [ ] #2 Enlaza a cli-reference.md y configuration.md
- [ ] #3 No inventa flags ni rutas; cita anclajes reales
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 pipelinek validate pipeline.kts pasa
- [ ] #2 scripts/fast-check.sh --all pasa
<!-- DOD:END -->
