---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-20T10:24:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea. Sesión: 2026-11-20 (supervisor + workers delegados).

## Ahora

- **`main` = `f3dcec5`** (merge PR #47: cableado vivo del supervisor con stops honestos +
  fixtures de test con identidad git propia). Tablero: 37 `Done` (contado con `backlog task
  list`), en curso **TASK-34** (0/4) y
  **TASK-35** (0/4), en To Do **TASK-40** y **TASK-41** (3 AC cada una, añadidas por el
  supervisor en esta sesión).
- **TASK-34 (HIGH)**: la promoción viva ya está probada con ACP real. Epoch 1 de `uat3`:
  TASK-1 ejecutado por el agente en sandbox, verificado, **promovido `a0e3c3ea2b92` a
  `hodei-mh/main` y cerrado** (re-read confirma `Done`). Evidencia AC existente: AC2(a)
  pause/resume PASS, AC2(d) `Renew` → `Unsupported` explícito PASS, AC3(iii) tablero vacío sin
  sandbox ni red PASS, AC4 parcial con SHA de binarios y escaneo de secretos 0 coincidencias.
- **Dos huecos reales detectados en vivo** (no es culpa del proveedor):
  1. **Sin encadenamiento tras confirmar**: `engine.rs:560-620` mantiene el run vivo tras
     `Confirmed` ("the objective held") pero `server.rs:616-680` no vuelve a seleccionar; el
     `phase` queda `running` sin sandbox y un segundo `Start` se rechaza con "a run is already
     in flight". El único avance era un `Stop`, semánticamente falso después de un éxito.
  2. **Turno ACP cortado por `AGENT_TIMEOUT` fijo de 180 s** (`live/agent.rs:50`): la epoch 2
     (TASK-2) confirmó runtime, el agente estaba produciendo, y el turno murió en `timeout`;
     el mensaje imprime la cola **espejada** (`agent.rs:563-565`, anotado en TASK-41).
- **Rama `task/task-34-fixes` (worktree `../wt-task34b`) en curso** con worker vivo: corrige el
  encadenamiento y los hallazgos verificados (a)-(e) de la revisión independiente sobre
  `f3dcec5`: (a) `server.rs:528` `write_answer(...)?` corta antes del `FollowUp` (stop
  registrado sin `cancel`); (b) `live/agent.rs:240-282` staging del adaptador en `/tmp`
  predecible confiado por `exists()`; (c) `publisher.rs:83` + `requirements.rs` indices de AC
  por conteo (puede declarar ACs no verificadas); (d) `publisher.rs:74-123` construye
  `PromotionIntent` y lo descarta en vez de `promote::flow::execute` + `CoordinatorJournal`;
  (e) `supervisor_cli.rs:145,189-193` y `mod.rs` enmascaran fallos no-EPIPE/git.
- **TASK-35 entregado por worker, sin aceptar aún**: `618521d` en `task/task-35-resume`
  (worktree `../wt-task35`), gate del worker `scripts/fast-check.sh --all` **PASS**, 1425+/164-,
  checkpoint durable en producción (`live/checkpoint.rs`), permisos ACP vía `interact::resolve`,
  `-32601` para interacciones no soportadas, objetivo personalizado sin ticket
  (`Closes::Nothing`, `StartObjective`). Revisión goldfish independiente en vuelo.

## Siguiente corte exacto

1. Verificar el diff real de `task/task-34-fixes` (no el informe del worker), correr
   `scripts/fast-check.sh --all`, PR con asunto convencional, merge tras el gate de workspace.
2. Rebasear `task/task-35-resume` sobre el `main` nuevo, reconciliar con la revisión goldfish,
   PR y merge; después reconstruir `--release` y **liberar** los recursos vivos de esta sesión.
3. **UAT final con el binario corregido** (requisito AC4: el propio binario + SHA): escenario A
   con dos tickets dependientes en un solo `Start` (el segundo debe recibir `a0e3c3e...` y
   promover su propio commit sin `Stop` intermedio), B confirmado-después-de-stop, C check
   insatisfacible sin promoción/cierre, D permiso denegado sin promoción/cierre, y la UAT de
   recuperación de TASK-35 (matar el supervisor con claim vivo y reiniciar).
4. Solo con evidencia: marcar AC en Backlog.md vía CLI y cerrar TASK-34; luego TASK-35.
5. Después: TASK-40 (diseño B: wrapper que lee una línea de stdin y hace `exec`; fallback A:
   `CreateSandboxRequest.env`, ver mapa de hechos en la nota de TASK-40) y TASK-41
   (incluye el `text_tail` espejado y el `AGENT_TIMEOUT` no configurable).

## Operaciones vivas (adoptar antes de relanzar; liberar al terminar)

- lifecycle server `pid 3234327` en `PORT=18082`, con `HODEI_MH_CONTROL_TOKEN` solo en el
  entorno del shell que lo lanzó; pid files en `/tmp/uat-task34/state3/` (`server.pid`,
  `sup3-{a,b,c,d}.pid`), `XDG_STATE_HOME=/tmp/uat-task34/state3`.
- 4 supervisores vivos: `pid 3234344..3234347` → sockets `/tmp/uat-task34/sup3-{a,b,c,d}.sock`,
  proyectos `uat3`/`uat3b`/`uat3c`/`uat3d`, repos `/tmp/uat-task34/repos3/repo-{a,b,c,d}`.
- contenedor proxy `hodei-uat-proxy3` (`scripts/egress-proxy.py --allow-domain api.minimax.io`).
  **No tocar** `hodei-proxy` (11 h, es del usuario). Sandboxes `sandbox-*` con
  `purpose=hodei-mh-live` y `expires-at` a 1 h: se liberan solas o con el `Stop` cooperativo.
- Perfil de proveedor que funciona ahora: `cuenta-1` (`api.minimax.io`); `cuenta-6` tiene token
  vacío; `cuenta-3/4` (z.ai) se recuperan 2026-11-21 23:07 local. Receta completa:
  `/tmp/uat-task34/evidence/commands.txt`; evidencia de esta sesión:
  `/tmp/uat-task34/evidence3/` (`A_poll.txt`, `A_stall.txt`, `sup3-*.log`, `server.log`).

## Aprendido en esta sesión

- **Un fixture que solo pasa con la identidad git del host es un test mudo en CI.**
  `init_fixture_repo` neutraliza hooks pero no pone identidad; `commit_index` la inyecta por
  entorno. Arreglo: identidad local en el fixture y oracle `GIT_CONFIG_GLOBAL=/dev/null
  GIT_CONFIG_SYSTEM=/dev/null`. Regla: todo fixture vivo debe ser huérfano del host.
- **`gh pr merge --delete-branch` no borra una rama usada por un worktree**: quitar el worktree
  antes; la rama remota se borra igual.
- **CLI Backlog.md**: `-p` es *parent* (usar `--priority`), y `--add-label` no existe (usar
  `-l`). Nunca editar el markdown a mano.
- **Confirmar ≠ terminar**: el motor mantiene el run vivo tras `Confirmed`, así que la
  verificación de "¿avanzó?" debe mirar si hay una *nueva* selección, no solo el `phase`.
- **Los timeouts fijos de producto matan objetivos lentos**: 180 s por turno ACP no es
  configurable ni se reporta como decisión del producto; hay que dimensionarlo o exponerlo.
- **El squash de GitHub inyecta `Co-authored-by` y `git-control` lo rechaza**; el asunto de merge
  por defecto también se rechaza (`GCTL-CONV-002`) → merge manual con asunto `chore:` y nunca
  `--no-verify`.
