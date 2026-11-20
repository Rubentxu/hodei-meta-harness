---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-20T19:31:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea. Sesión: 2026-11-20 (supervisor + workers delegados).

## Ahora

- **El tablero está completo (51/51 `Done`) pero el CI de `main` tiene el carril de sondas
  ROJO.** No lo cierres como si todo estuviera verde: el `workspace gate` (fmt, clippy, suite
  completa) pasa, pero la `probe lane` falla desde 2026-11-20T11:13Z. Run 44799959858:
  `workspace gate` SUCCESS, `probe lane` FAILURE.
- **TASK-52** (creada en esta sesión, `In Progress`) recoge el defecto: `supervisor-probe` tiene
  4 checks en rojo y `multiproject-probe` 1 step. Los checks afirman un run estable en fase
  `running`; tras el encadenamiento de objetivos (TASK-34) el runtime de ensayo confirma al
  instante y la época 1 termina, así que esa fase es transitoria. Worker en
  `../wt-task52` (rama `task/task-52-probe-lane`) y revisión independiente en paralelo.
  Reproducción local (mismos fallos): hay que arrancar el proxy de egreso, porque
  `scripts/probes.sh` no corre sin él:
  ```
  docker run -d --name hodei-local-egress --network bridge -p 127.0.0.1:18888:8888 \
    -v "$PWD/scripts/egress-proxy.py:/opt/egress-proxy.py:ro" \
    --entrypoint python3 rust:1-bookworm /opt/egress-proxy.py \
    --bind 0.0.0.0 --port 8888 --allow-prefix 172.16.0.0/12
  HODEI_MH_EGRESS_PROXY_CONTAINER=hodei-local-egress \
  HODEI_MH_PROXY_CHECK_URL=http://127.0.0.1:18888 \
  HODEI_MH_CONTROL_TOKEN="$(openssl rand -hex 32)" \
  scripts/probes.sh --rounds 1 --probe "supervisor-probe multiproject-probe"
  ```
- **Repo**: `hodei-meta-harness` (`/home/rdcabrera/Proyectos/rust/goal-cycle-mh`), en `main`
  (`8b3ab72`), sincronizado con `origin/main`. Árbol limpio salvo `.qoder/` (ajeno, sin
  versionar). Sin stashes, sin worktrees sueltos, sin ramas de tarea pendientes.
- **Tarea abierta**: solo TASK-52. Todo lo demás está `Done` (33 → 51 durante el día, con
  TASK-34, TASK-35, TASK-36, TASK-40…TASK-46 y la tanda de documentación TASK-47…TASK-51).

## Última tarea cerrada

- **TASK-51 — Índice de documentación de usuario en `docs/README.md`** (3/3 AC), mergeada y con
  la evidencia en el tablero. Precedida por TASK-47…TASK-50 (referencia CLI, quickstart,
  configuración, troubleshooting) y por TASK-34…TASK-46 (bucle vivo, credenciales, visibilidad
  de fallos, JSONL de eventos y correcciones de sondas).

## Aprendido

- **El tablero puede estar completo y el CI rojo a la vez.** "51/51 Done" no es evidencia de
  que el carril de integración pase: audita los artefactos del run, no solo el tablero.
- **Los artefactos del carril se descargan**: `gh run download <run-id> -D <dir>` (desde dentro
  del repo, si no `gh` no infiere el repositorio) y ahí está el JSON de cada probe con el check
  que falló y su `detail`. Es la evidencia primaria del fallo.
- **El `detail` de un check puede mentir**: `attached-terminal-read-only-and-disconnect-is-inert`
  imprimía "still running" por construcción, mientras observaba otra cosa. Los detalles deben
  contar lo observado.
- **La política `git-control` juzga el historial completo**, no solo el commit que escribes: un
  mensaje malo creado en GitHub bloquea el fast-forward local y *todo* push. Se corrige con
  `git commit --amend --reset-author -F <mensaje>` y `git push --force-with-lease`; nunca
  `--no-verify`.
- **Un `MERGE_HEAD` a medias es una operación viva**: `git merge --abort` + rama temporal +
  `amend` resolvió una fusión colgada sin relanzarla.
- Las sondas (`run-probe`, `multiproject-probe`, `supervisor-probe`) **no son el mismo carril**:
  `scripts/probes.sh` corre `release|supervisor|profile|multiproject|verify|trust-probe`, y lo
  que no está en esa lista no lo detecta ninguna lane (de ahí nacieron TASK-38 y TASK-39).
