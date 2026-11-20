---
id: TASK-57
title: >-
  Explicación novel-first de qué es y para qué sirve hodei-mh en
  docs/overview.md
status: Done
assignee: []
created_date: '2026-11-20T21:22:00Z'
updated_date: '2026-11-20T21:31:00Z'
labels: []
dependencies: []
priority: high
type: docs
ordinal: 57000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Los usuarios que llegan a la documentación se preguntan primero QUE es esto, PARA QUE sirve y COMO funciona, y las docs actuales empiezan por los comandos. Crear docs/overview.md en inglés con: la analogía del capataz con obra cerrada (agente IA en sandbox desechable, verificación antes de promoción, journal honesto), las tres garantías (aislamiento, verificación del candidato, memoria durable con budget y single-controller), una frase por pieza (lifecycle-server, supervisor, execd, doctor/perfiles), y la escalera de tres peldaños (rehearsal sin infraestructura, doctor+profile-run con una caja real, supervisor --live con tablero y managed branch). Sin inventar: cada afirmación anclada a quickstart/cli-reference/profiles/supervisor-protocol existentes. Enlazarlo desde docs/README.md y del README raíz como primer enlace de documentación, y añadir la respuesta a What is this? al inicio del README si aún no la tiene clara.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Un lector sin contexto puede explicar en dos frases qué hace el producto y qué problema evita después de leer solo overview.md
- [x] #2 La escalera de tres peldaños coincide con los comandos reales de quickstart.md sin contradecir USAGE
- [x] #3 Toda garantía citada tiene ancla a un doc o código existente; nada prometido que el producto no haga
- [x] #4 overview.md está enlazado desde docs/README.md y README.md
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 scripts/fast-check.sh --all pasa
- [x] #2 pipelinek validate pipeline.kts pasa
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T21:31:00Z

---

Evidencia (supervisor): rama @c0ee7ba (base 0ba57b9, worktree aislado), exactamente README.md + docs/README.md + docs/overview.md (188 líneas). AC1-4: analología capataz/obra cerrada con glosario, escalera enlazada a secciones reales de quickstart (slugs verificados por el supervisor: #1, #22, #23, #24, #3), garantías ancladas a runner.rs/promote/checkpoint.rs/profiles/configuration sin prometer nada no hecho, enlaces desde ambas portadas. Gates worker y post-merge: fast-check PASS, pipelinek validate SUCCESSFUL. Fix inline del descubrimiento reportado por el worker (quickstart marcaba configuration.md como pending obsoleto + enlace a overview) @commit docs(quickstart).

---
<!-- COMMENTS:END -->
