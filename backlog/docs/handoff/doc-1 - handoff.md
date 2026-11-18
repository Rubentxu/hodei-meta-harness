---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-18T23:27:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea.

## Ahora

- Ninguna tarea en curso. TASK-27 está cerrada y mergeada; `main` está en `2bcb547`.
- Repo: `hodei-meta-harness` (`/home/rdcabrera/Proyectos/rust/goal-cycle-mh`), en `main`, árbol limpio, sin ramas locales ni remotas de tarea y sin worktrees adicionales.

## Última tarea cerrada

- **TASK-27 (MH-A04) — Admitir y recuperar sandboxes de forma atómica y honesta.** El lifecycle
  server es la única autoridad de admisión: reserva CPU/memoria/slots antes de cualquier efecto
  (entornos existentes + reservas pendientes), suelta la reserva tras fallo o abandono y
  reconstruye desde el daemon tras un reinicio sin borrar entornos recién creados. Idempotencia
  por `creationKey` + huella sha256 (labels que sobreviven al reinicio; `409 CREATION_CONFLICT`
  si cambia el cuerpo). Un DELETE fallido no libera, se reintenta de forma acotada y solo se
  borran entornos con la identidad propia. Límites NaN/infinito/cero/desbordados rechazados.
- Evidencia: PR #37 (merge `2bcb547`; commits `239b8d6`, `ae78f99`, `ad8189e`, `1ebf81c`).
  17 pruebas offline deterministas (`crates/lifecycle-server/src/service/admission_tests.rs`) +
  7 de compose; 3 mutantes manuales detectados; `cargo fmt`, `clippy -D warnings` y
  `scripts/fast-check.sh` PASS; smoke real `multiproject-probe` PASS (5 creates simultáneos
  contra 2 slots: 2 admitidos y 3 `429`; 4 con una clave: 1 sandbox; 0 residuos).

## Qué queda a medias

- Nada de TASK-27. Salvedades conscientes:
  - Solo se ejecutó la sonda `multiproject-probe` (la que lleva el paso nuevo), no las seis de `scripts/probes.sh`.
  - No se probó el paso nuevo contra el código VIEJO para ver que falla; los dientes están demostrados con mutación en las pruebas offline.
  - La idempotencia no promete exactly-once en red: solo reconciliación del resultado incierto (decisión de la tarea).

## Recursos liberados

| Recurso | Qué era | Liberado |
| --- | --- | --- |
| rama local y remota | `task/task-27-atomic-admission` | sí (PR #37 mergeado con `--delete-branch`; `git branch -a` solo muestra main) |
| contenedor | `hodei-smoke-proxy-t27` (proxy de egreso de la sonda) | sí (`docker rm -f`; sin diferencias contra el estado previo) |
| sandboxes / redes de la sonda | creados por `multiproject-probe` | sí (la pista informó `sandboxes=0 containers=0 networks=0`) |
| procesos auxiliares | servidor de presupuesto en 8099 y lifecycle en 8081 | sí (la pista los cierra) |
| temporales | `/tmp/*.bak`, `/tmp/t27-*` | desechables, fuera del repo |

## Aprendido

- **Un registro de claves necesita un guard RAII**: un ticket de creación abandonado dejaba la
  clave `InFlight` para siempre y los que esperaban colgados. `CreationTicket` con `Drop`
  publica el resultado y borra la clave bajo UN solo lock (si no, hay carrera publicar/borrar).
- **Una prueba de carrera solo tiene dientes si FUERZA el solape**: un daemon Docker simulado
  (socket unix + axum) que acepta y no responde; y se comprueba con mutación manual (quitar la
  reserva, dar por liberado un DELETE fallido, borrar sin mirar la identidad).
- **bollard a veces omite el prefijo `/v1.xx`** en las rutas: el simulador debe normalizar el
  path quitando solo un primer segmento `v<dígito>…`.
- **La clave de creación debe normalizarse igual en el registro y en el label** (recortada), o
  tras el reinicio no se reencuentra.
- **No usar `pkill -f <patrón>` si el patrón aparece en el propio comando**: mató mi shell
  (exit 144). Lo mismo con `pgrep -f scripts/probes.sh` dentro de un Monitor: se encuentra a sí
  mismo y nunca termina; para esperar un proceso propio, lanzarlo con `run_in_background`.
- **`scripts/probes.sh` exige `HODEI_MH_CONTROL_TOKEN`, `HODEI_MH_EGRESS_PROXY_CONTAINER` y
  `HODEI_MH_PROXY_CHECK_URL`**: sin proxy configurado se puede levantar uno efímero con
  `docker run -d -v scripts/egress-proxy.py:/p/egress-proxy.py:ro -p 127.0.0.1:8888:8888
  python:3.12-slim python /p/egress-proxy.py --bind 0.0.0.0 --port 8888` y borrarlo al terminar.
- **El presupuesto global de entornos contaba contenedores ANTES del `await` que los crea**; la
  regla de slots sigue teniendo un único dueño (`hodei-mh-budget::admissible_creates`) y no hay
  segundo ledger; la reconciliación necesita un horizonte de secuencia.
- **El execd de un sandbox recién creado aún no acepta sesiones**: esperar de forma acotada.
- **El daemon fusiona el `ENV` de la imagen con el del contenedor**: inspeccionar la imagen y
  rechazar `EXECD_*`/`OPENSANDBOX_*`.
- **Comparar prefijos con `entry[..n]` sobre `&str` panica** en cortes UTF-8: comparar BYTES.
- **La edad de una red no dice cuánto tarda su creación**: las creaciones en vuelo se registran.

## Candidatos siguientes

- TASK-28 (MH-A05) — Preparar workspaces y comandos sin transportar secretos del host (High).
- TASK-29 (MH-A11) — Extraer una ruta vertical de supervisión comprobable sin infraestructura.
- TASK-30 (MH-A07) — Acotar IPC y salida conservando eventos estructurados.
- TASK-31 (MH-A08) — Deadline único y cleanup reutilizable del transporte OpenSandbox (desbloqueada por TASK-27).
- TASK-33 (MH-A12) — Reconciliar promoción Git y cierre de ticket con una intención durable.
- TASK-34 (MH-A06) — Conectar el supervisor de producto al bucle real de objetivos ACP.
- TASK-35 (MH-A13) — Reanudar objetivos y conversaciones sin conservar sandboxes innecesarios.
- TASK-36 (MH-A10) — Reutilizar cachés de compilación sin compartir estado de agentes.
- TASK-38 — El run-probe no entrega la credencial del execd y su pipe-attach falla con 401.
