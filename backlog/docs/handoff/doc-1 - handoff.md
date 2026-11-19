---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-19T22:57:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea.

## Ahora

- **TASK-33 (MH-A12) está CERRADA.** 4/4 AC comprobados, PR #42 fusionado, `main` en
  `34ce234`. La promoción Git y el cierre del ticket se reconcilian desde una intención durable
  (`crates/hodei-mh-promote/{intent,flow,journal,writer}.rs` + payload en el store del
  coordinador), con la fase derivada de la observación de las dos autoridades y cada paso
  idempotente.
- **Aviso operativo (política `git-control`):** el mensaje del commit de merge de PR #42
  llevaba un trailer `Co-authored-by` que la política prohíbe. El hook `reference-transaction`
  juzga **commit por commit** los que entran en una rama, así que bloqueaba el fast-forward local
  y **todo push** (también de ramas nuevas que contuvieran ese commit). Remediado sin reescribir
  contenido: `git commit --amend --reset-author -F <mensaje>` (mismo árbol) y
  `git push --force-with-lease`. Si vuelve a salir un trailer, el mismo camino: **nunca**
  `--no-verify`. La rama `main` de GitHub no está protegida.
- **En curso, tres workers en worktrees aislados** (uno por tarea; no se tocan entre sí):
  - TASK-31 (MH-A08, deadline único y cleanup del transporte) → `../wt-task31`, rama
    `task/task-31-work`.
  - TASK-38 (la credencial del execd no viaja en el handshake de `run-probe`) → `../wt-task38`,
    rama `task/task-38-work`.
  - TASK-39 (las sondas dejan sockets de supervisor en `/tmp`) → `../wt-task39`, rama
    `task/task-39-work`.
- **La siguiente por prioridad es TASK-34 (MH-A06, HIGH)**: conectar el supervisor de producto al
  bucle real de objetivos ACP. Depende de TASK-33 (cerrada) y de TASK-31 (en curso), así que su
  encargo sale en cuanto TASK-34 quede cableado sobre el transporte unificado. Después:
  TASK-35 (MH-A13) y TASK-36 (MH-A10).
- Tablero: 33 tareas `Done`; en curso TASK-31, TASK-38, TASK-39; pendientes TASK-34, TASK-35,
  TASK-36.

## Última tarea cerrada

- **TASK-33 (MH-A12) — Reconciliar promoción Git y cierre de ticket con una intención durable.**
  - `PromotionIntent` guarda los hechos inmutables del intento (operación derivada de
    `work_ref:epoch:candidate`, proyecto, `work_ref`, `epoch`, rama gestionada, SHA base y
    candidato esperados, estado destino, nota marcada `[hodei-mh:op:{operation_id}]`). El
    coordinador lo conserva como payload opaco del intent de efecto: un solo store, una sola
    autoridad.
  - `decide()` es una función pura: la fase **no se persiste**, se deriva de observar las dos
    autoridades (la ref de Git y el JSON del ticket). Por eso una caída se reconcilia en vez de
    dejar un estado intermedio que nadie sabe leer.
  - Conflicto real (ref movida, epoch caducada, proyecto o SHA distintos, ticket tocado por una
    persona) se registra y **no** se adopta ni se sobrescribe nada; el cierre humano no se pisa.
  - Verificación: 34 unitarias, 7 de recuperación con repo Git temporal y tracker falso, 2 de
    contrato contra la CLI real sin LLM, gate local `scripts/fast-check.sh` en PASS y
    `workspace gate` de CI en SUCCESS.

## Aprendido

- **La política `git-control` juzga el historial completo, no solo el commit que escribes.** Un
  mensaje malo creado en GitHub (squash merge) deja la rama local sin poder avanzar y sin poder
  pushear nada, aunque el árbol sea correcto. Es el primer sitio donde mirar si `git` se niega.
- **Un `MERGE_HEAD` a medias es una operación viva**: estaban la fusión del PR #42 y su
  `MERGE_MSG` ya tenía el mensaje limpio que la política admitía. `git merge --abort` + rama
  temporal + `amend` resolvió la operación en vez de relanzarla.
- `backlog task edit` escribe las notas y el resumen final en el fichero de la tarea; el
  handoff se actualiza con `backlog doc update doc-1 --content ...`.
- Las sondas (`run-probe`, `multiproject-probe`) **no están** en `scripts/probes.sh`: por eso un
  fallo en ellas no lo detecta ninguna lane. TASK-38 y TASK-39 nacen de esa laguna.
