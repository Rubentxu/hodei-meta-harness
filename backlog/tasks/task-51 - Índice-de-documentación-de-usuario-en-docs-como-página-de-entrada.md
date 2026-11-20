---
id: TASK-51
title: Índice de documentación de usuario en docs/ como página de entrada
status: Done
assignee: []
created_date: '2026-11-20T19:18:00Z'
updated_date: '2026-11-20T19:22:00Z'
labels: []
dependencies: []
priority: high
type: docs
ordinal: 51000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Crear docs/README.md como índice navegable de la documentación de usuario (quickstart, cli-reference, configuration, profiles, supervisor-protocol) con descripción de una línea por documento y a quién sirve cada uno, más enlaces al README principal. Objetivo: que la navegación de GitHub en docs/ tenga una portada coherente sin duplicar contenido (una sola fuente de verdad: los docs del repo, no la wiki).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Lista los cinco documentos con ruta relativa funcional y una línea de propósito
- [x] #2 Enlaza de vuelta al README.md y no duplica secciones del README
- [x] #3 No inventa comportamiento: cada descripción coincide con el contenido real del doc
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 scripts/fast-check.sh --all pasa
- [x] #2 pipelinek validate pipeline.kts pasa
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T19:22:00Z

---

Evidencia (supervisor): rama task/task-51-docs-index @0f13a72 (base 040a7c8, worktree aislado /tmp/wt51 — patrón corregido del ciclo), solo docs/README.md (27 líneas). AC verificadas: 5 enlaces relativos resuelven a ficheros existentes en main, back-link a ../README.md en líneas 5 y 27, descripciones cotejadas contra el contenido real de cada doc. Gates: fast-check --all PASS, pipelinek validate SUCCESSFUL (worker y post-merge). Merge --no-ff, HEAD post-merge verificado.

---
<!-- COMMENTS:END -->
