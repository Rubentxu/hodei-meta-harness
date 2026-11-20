---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-20T10:54:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea. Sesión: 2026-11-20 (supervisor + workers delegados).

## Ahora

- **`main` = `c82f907`** (último commit de tablero; el último código integrado es `f3dcec5`,
  merge PR #47: cableado vivo del supervisor con stops honestos +
  fixtures de test con identidad git propia). Tablero: 37 `Done` (contado con `backlog task
  list`), en curso **TASK-34** (2/4 AC marcadas) y
  **TASK-35** (0/4), en To Do **TASK-40** y **TASK-41** (3 AC cada una, añadidas por el
  supervisor en esta sesión).
- **TASK-34 (HIGH)**: **AC1 y AC2 marcadas con evidencia viva**. Run 3 con binarios de `f3dcec5`
  (`hodei-mh` sha256 `3230d360…`, servidor `961241f7…`, evidencia `/tmp/uat-task34/evidence3`):
  dos tickets dependientes ejecutados por ACP real solo con la CLI de producto —
  `edd22ab → a0e3c3e (TASK-1) → f7a6650 (TASK-2)`, `merge-base --is-ancestor` rc=0, tablero
  TASK-1 Done 2/2 y TASK-2 Done 1/1; Stop tras promocionar `6c1695a` con
  `uncertain=false`; `Renew` → `Unsupported` explícito; check insatisfacible sin promoción ni
  cierre; tablero vacío sin sandbox ni tokens. **AC3 NO marcada**: con `--permission deny` no se
  promociona ni se cierra, pero **no existe ningún evento de denegación** — `agent.rs` responde
  `cancelled` sin emitir nada y el run muere como timeout de 180 s indistinguible de un modelo
  lento (defecto registrado en TASK-41). **AC4 no marcada** hasta repetir la UAT contra el
  binario corregido. Queda el hueco de encadenamiento: el segundo ticket exigió un Stop+Start
  manual porque tras `Confirmed` el run sigue `running` y `Start` se rechaza.
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
  **Estado**: ese worker agotó su límite de turnos con 15 ficheros y ~1289 inserciones **sin
  commitear**; se respaldó el diff (`/tmp/uat-task34/task34-fixes.partial.patch`, sha256
  `cac2743c…`) y un worker nuevo lo **adoptó** para terminar las dos pruebas de encadenamiento,
  correr `cargo fmt --all && scripts/fast-check.sh --all` y commitear en la rama (sin push, sin
  PR: los hace el supervisor). El diff ya introduce `MAX_CHAINED_ATTEMPTS = 32` y un `serve_line`
  que aplica el follow-up aunque fallar el write.
- **TASK-35 entregado por worker y RECHAZADO por la revisión independiente** (`618521d` en
  `task/task-35-resume`, worktree `../wt-task35`; gate del worker `--all` PASS): goldfish-verdict
  **DO_NOT_SHIP** con 8 defectos confirmados y verificados por el supervisor contra el código —
  el objetivo personalizado declara `checks` vacío así que `require_evidence` lo rechaza y nunca
  se confirma (el test aprobaba con un publisher fake); el claim sobrevive a un crash sin dueño;
  `recover()` no puerta la ejecución; un `confirm` que falla degrada una promoción real; el
  handoff se anuncia en el boot pero se escribe después del turno; el JSON de estado queda 0644
  con la instrucción cruda; `--objective` descarta `--ticket` en silencio; el rehearsal ignora
  `request.objective`. Las decisiones de corrección están en la nota de TASK-35 (incluida la
  reutilización de `TerminationCriterion`/`advance_custom`, que ya existe en el dominio).

## Siguiente corte exacto

1. Verificar el diff real de `task/task-34-fixes` (no el informe del worker), correr
   `scripts/fast-check.sh --all`, PR con asunto convencional, merge tras el gate de workspace.
2. Con `task/task-34-fixes` ya en `main`, **rebasear `task/task-35-resume` y relanzar un worker
   de corrección** con las 8 decisiones de la nota de TASK-35 (un solo escritor por zona de
   código: publisher/agent/server solapan con TASK-34). Después gate `--all`, segunda ronda de
   revisión y PR/merge.
3. **UAT final con el binario corregido** (requisito AC4: el propio binario + SHA): escenario A
   con dos tickets dependientes en un solo `Start` (el segundo debe recibir `a0e3c3e...` y
   promover su propio commit sin `Stop` intermedio), B confirmado-después-de-stop, C check
   insatisfacible sin promoción/cierre, D permiso denegado sin promoción/cierre, y la UAT de
   recuperación de TASK-35 (matar el supervisor con claim vivo y reiniciar).
4. Solo con evidencia: marcar AC en Backlog.md vía CLI y cerrar TASK-34; luego TASK-35.
5. Después: TASK-40 (diseño B: wrapper que lee una línea de stdin y hace `exec`; fallback A:
   `CreateSandboxRequest.env`, ver mapa de hechos en la nota de TASK-40) y TASK-41
   (incluye el `text_tail` espejado y el `AGENT_TIMEOUT` no configurable).

## Operaciones vivas

- **No queda ninguna operación viva**: el run 3 de la UAT liberó supervisores, lifecycle server y
  `hodei-uat-proxy3`; verificado 0 contenedores `sandbox-*`, 0 redes con
  `label=hodei.io/network-owner=lifecycle-server`, 0 procesos `hodei-mh supervisor` /
  `hodei-mh-lifecycle-server`. `hodei-proxy` (12 h) es del usuario y no se toca.
- Receta para relanzar la UAT final (con el binario corregido): `scripts/egress-proxy.py` en un
  contenedor con `--allow-domain api.minimax.io`, `XDG_STATE_HOME` y `PORT` propios,
  `HODEI_MH_CONTROL_TOKEN` **solo en el entorno** del shell que arranca el servidor (nunca
  impreso), y `hodei-mh supervisor --live` con `--server/--image/--proxy-url/--project-dir/
  --tracker-dir/--managed-branch hodei-mh/main/--permission/--socket/--project`. Todo el detalle
  replicable está en `/tmp/uat-task34/evidence/commands.txt` y `/tmp/uat-task34/evidence3/`.
- Pruebas y perfiles: `cuenta-1` (`api.minimax.io`) funciona; `cuenta-6` tiene token vacío;
  `cuenta-3/4` (z.ai) se recuperan 2026-11-21 23:07 local.

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
- **Un test que enchufa un fake en el punto exacto del defecto aprueba el fake, no el producto.**
  TASK-35 pasó su gate `--all` y aun así el objetivo personalizado no podía confirmarse:
  `require_evidence` rechazaba el `EvidenceSet` vacío y la prueba usaba un publisher fake. Regla:
  cuando una AC habla de promover/cerrar, el oráculo debe recorrer el publisher real (o un fake
  que aplique la misma política de evidencia).
- **Un worker que agota sus turnos no es trabajo perdido**: antes de relanzar nada, respaldar el
  árbol sucio como patch con sha256 y lanzar el relevo con la orden explícita de **adoptar**
  (leer el diff, no reiniciar, no reset/stash/checkout). Así el encargo nuevo es "terminar +
  verificar + commitear", no "volver a diseñar".
- **El squash de GitHub inyecta `Co-authored-by` y `git-control` lo rechaza**; el asunto de merge
  por defecto también se rechaza (`GCTL-CONV-002`) → merge manual con asunto `chore:` y nunca
  `--no-verify`.
