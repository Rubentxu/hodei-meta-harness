---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-20T20:13:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea. Sesión: 2026-11-20 (supervisor + workers delegados).

## Ahora

- **El tablero está cerrado: 54/54 tareas `Done`, ninguna elegible pendiente.** Y el CI de
  `main` está en verde de verdad: run `44808413572` con `workspace gate` SUCCESS y
  `probe lane` SUCCESS (las seis sondas). PRs fusionados en las últimas 48 h: 15; commits:
  136.
- **Repo**: `hodei-meta-harness` (`/home/rdcabrera/Proyectos/rust/goal-cycle-mh`), en `main`
  (`8d20acb`), sincronizado con `origin/main`, árbol limpio. Sin stashes, sin worktrees, sin
  ramas de tarea, sin PRs abiertos. Otra sesión trabaja en paralelo en este mismo repo
  (TASK-53/TASK-54, ambas cerradas), así que `main` puede avanzar sin que seas tú.
- **Residuo ajeno observado en el host** (no es de este ciclo, no lo he tocado): los
  contenedores `hodei-proxy` (un `sleep infinity` de otra sesión) y `hodei-uat-proxy4` siguen
  levantados. El proxy que usa el carril de sondas es el que levanta `scripts/probes.sh` o el
  que se arranca a mano con `HODEI_MH_EGRESS_PROXY_CONTAINER`; uno de los dos lo ocupa.

## Última tarea cerrada

- **TASK-52 — El carril de sondas rojo en `main`** (4/4 AC), mergeada en PR #49 (`8d20acb`).
  - El tablero decía "51/51 Done" y aun así el `probe lane` estaba rojo: las sondas afirmaban
    un run estable en fase `running` que el encadenamiento de objetivos de TASK-34 ya no
    sostiene (el runtime de ensayo confirma al instante y la época termina en milisegundos).
  - Las sondas se reapuntaron al invariante real: el journal (`sup-events --jsonl`) debe
    mostrar la época aplicada, confirmada por el runtime y cerrada por su veredicto; la CLI y
    la TUI deben llevar el mismo valor en los doce campos del contrato de snapshot; el viewer
    debe dejar el estado intacto antes y después; y un `Stop` solo es honesto si paró un run
    vivo o si el journal prueba que el veredicto de fin es anterior a la acción.
  - Ningún check se borró ni se volvió constante, y el `detail` de cada uno cuenta lo observado
    (uno de ellos afirmaba "the confirmation never happened" cuando el journal mostraba la
    confirmación).

## Aprendido

- **"Todo Done" no es lo mismo que "el CI pasa".** Audita el tablero Y los artefactos del run
  (`gh run download <run> -D <dir>`, desde dentro del repo: si no, `gh` no infiere el
  repositorio). El JSON de cada probe dice qué check falló y por qué.
- **Una regresión se acota comparando el run verde con el primero rojo**: el delta entre ambos
  commits es el culpable, y si no toca el fichero que falla, la culpa es del entorno del
  check, no del código que comprueba. Aquí el delta era exactamente el commit de encadenamiento.
- **Un `detail` de check puede mentir por construcción.** `attached-terminal-read-only…`
  imprimía "still running" sin observarlo. Los detalles se escriben a partir de lo medido.
- **Un check que no puede fallar es peor que un check rojo**: el del viewer era vacuo (no
  distinguía "inerte" de "fuga"). Reapuntarlo a comparar el estado antes y después sí lo
  distingue.
- **Revisión independiente con contexto nuevo antes de tocar la evidencia**: la revisión
  goldfish no refutó el diagnóstico y aportó dos correcciones (el detalle que mentía y la
  vacuidad del check). También dejó anotada la cobertura que el arreglo deja sin observar en CI y la
  dejó escrita como riesgo residual en la tarea.
- **La política `git-control` juzga el historial completo**, no solo el commit que escribes: un
  mensaje malo creado en GitHub bloquea el fast-forward local y *todo* push. Se corrige con
  `git commit --amend --reset-author -F <mensaje>` y `git push --force-with-lease`; nunca
  `--no-verify`.
- **Un `MERGE_HEAD` a medias es una operación viva**: `git merge --abort` + rama temporal +
  `amend` resolvió una fusión colgada sin relanzarla.
- **El carril de sondas exige un proxy de egreso propio** y no arranca sin él; la receta
  exacta está en `scripts/probes.sh` y en el CI (`.github/workflows`): contenedor con
  `scripts/egress-proxy.py`, puerto publicado, `HODEI_MH_EGRESS_PROXY_CONTAINER`,
  `HODEI_MH_PROXY_CHECK_URL` y `HODEI_MH_CONTROL_TOKEN`. Un proxy que se deja levantado
  ocupa el puerto y hace fallar la siguiente corrida por un motivo que no es del producto.
- **Los workers también dejan residuos**: un contenedor de proxy y una rama sin borrar. El
  cierre del supervisor tiene que mirarlos.
