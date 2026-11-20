---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-20T08:29:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea.

## Ahora

- **TASK-31, TASK-36, TASK-38 y TASK-39 están cerradas.** `main` lleva TASK-31 en `67fc6ef`
  (merge de PR #46) y el cierre del tablero en `1a2b7ec`.
  - TASK-31: deadline único, readiness cacheada por generación, diario durable en
    `$XDG_STATE_HOME/hodei-mh/cleanup-journal.json` reconciliado **antes** de crear sandbox
    (`hodei-mh-verify` y probe acp), baseline explícito en la medición y sin `expect`/`unwrap`
    de producción. Evidencia: gate `--all`, CI 44702550552 SUCCESS, smoke real 12/12
    (`/tmp/runprobe-task31-merged.json`).
  - TASK-36: caché de build opt-in con clave profile/imagen/toolchain/`Cargo.lock`/proyecto/scope,
    digest verificado antes de restaurar, cuota con evicción y poda de huérfanos, y verificador
    que nunca la consume. Frío 10.18 s → caliente 8.00 s (descargas); `--cache-target` 8.92 s →
    0.95 s; archivo corrupto rechazado y descartado.
- **TASK-34 (MH-A06, HIGH) en curso** en `../wt-task34` (rama `task/task-34-live-core`):
  - Rebanada 1, `cc18f30`: `live.rs` puro (`AttemptEnd`/`outcome_of`, `LiveRuntime` con claims y
    puertos), `select_in`, `RenewOutcome` y `renew` por defecto en el contrato, y en el motor
    `apply_renewal` más la cancelación incierta de `Stop`; 20 tests; gate `--all` PASS.
  - Rebanada 2 **en curso**: adaptadores `crates/hodei-mh/src/live/*` sobre el transporte de
    TASK-31 (un `ExecdClient`, un `Deadline`, `UncertainResult` sin reintento, registro durable al
    crear), flags `--live` con rechazos explícitos, Renew contestado por el runtime fuera del lock
    y liberación del claim segura ante abort.
  - Rebanada 3 **pendiente**: UAT con el binario real (repo externo con Backlog.md y dos tickets
    dependientes por ACP real, permiso denegado sin promoción ni cierre, tablero vacío sin sandbox
    ni tokens, Stop con evidencia de cancelación y liberación) y decisión de paridad antes de
    sustituir `autonomous-loop`.
- **TASK-35 (MH-A13) pendiente**, depende de TASK-34.
- Tablero: 36 tareas `Done`; en curso TASK-34; pendiente TASK-35.
- **Operaciones vivas**: el lifecycle server de 8081 se detuvo (era el binario de TASK-38, previo
  a TASK-31). El contenedor `hodei-proxy` sigue sin listener: la UAT necesita un proxy de egreso
  real (`scripts/egress-proxy.py` en `rust:1-bookworm`, con puerto publicado) y exportar
  `HODEI_MH_EGRESS_PROXY_CONTAINER` y `HODEI_MH_PROXY_CHECK_URL`.

## Aprendido

- **El gate local de una rama no valida un cambio de API compartida.** TASK-31 compilaba sola,
  pero el código de TASK-36 ya en `main` llamaba a la firma antigua de `oneshot::run`: 5 `E0308`
  y 1 `E0382` que solo aparecieron al evaluar el merge. Remedio: merge de `origin/main` en la
  rama y adaptación de las llamadas (`14288bd`). Regla: cuando `main` se mueve, mirar el CI del
  PR contra la base actual, no solo el gate local.
- **El squash de GitHub inyecta `Co-authored-by`** cuando los commits de la rama llevan autor y
  `git-control` bloquea la ref. Remedios usados: commit con el mismo árbol y mensaje limpio más
  `--force-with-lease` (`be7d880`) y merge commit para PR #46.
- **La política también rechaza el asunto de merge por defecto** (`GCTL-CONV-002`): un merge
  manual debe llevar asunto `chore:`; nunca `--no-verify`.
- **Un registro sin consumidor no es reconciliación**: hace falta un llamador productivo y un
  diario durable; las sesiones execd no son residuo del host porque mueren con su sandbox.
- **Flakes bajo carga**: `supervisor_cli` (phase idle) y
  `socket_integration::a_client_that_does_not_read_is_dropped`. Pasan aislados y en la
  reejecución; se reejecuta el job y se registra, no se toca el test sin causa demostrada.
- Las sondas siguen fuera de `scripts/probes.sh`; el lane exige Backlog CLI, Docker, proxy de
  egreso con listener e imágenes ya presentes en el daemon.
