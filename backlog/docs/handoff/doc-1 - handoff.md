---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-20T08:08:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea.

## Ahora

- **TASK-38 y TASK-39 están CERRADAS.** `main` lleva el código de ambas (TASK-38 en `e9829ed`,
  TASK-39 en `045fded`) y el cierre del tablero en `c536081`.
  - TASK-38: `run-probe` entrega la credencial del execd por la cabecera del handshake
    (`ExecdEndpoint::attach_options`) y un 401 aparece como check FALLIDO, no omitido.
  - TASK-39: la sonda multiproyecto registra los sockets de supervisor que nombra, los elimina
    tras matar a los supervisores y publica el paso `no-supervisor-socket-survives-the-probe`
    (cleanup completo Y recuento de `/tmp` igual antes/después). Evidencia: probe real 14/14,
    `all_passed=true`, 0 residuo, 4/4 sockets eliminados, 0 antes y 0 después.
- **TASK-31 (MH-A08) en corrección.** El worker entregó `a004c59` con gate `--all` PASS y smoke
  real 12/12, pero la revisión goldfish confirmó cuatro defectos que un corrector está
  arreglando en `../wt-task31`:
  1. AC2: `ensure_ready` devuelve `true` con la readiness cacheada aunque el deadline ya esté
     agotado; la llamada bloqueada debe ser `NotAccepted` sin ejecutar el comando.
  2. AC3: `reconcile_sandboxes` no tiene llamador productivo; el registro se pierde al salir o
     al cancelar, y las entradas de sesión nunca se liberan. Hace falta diario durable y
     reconciliación al entrar en los caminos que crean sandboxes.
  3. AC4: la medición imprime p50/p95 pero no compara contra un baseline explícito.
  4. Política: `expect()` en producción en `execd/client.rs` y `cleanup.rs`.
- **TASK-36 (MH-A10, LOW) en curso** en `../wt-task36`: caché de build opt-in y segura
  (módulo puro `cache.rs`, montaje/GC en `profile_run`, doc de perfiles), con benchmark
  frío/caliente y la regla de que una caché escribible no puede producir PASS.
- **La siguiente por prioridad es TASK-34 (MH-A06, HIGH)**: conectar el supervisor de producto
  al bucle real de objetivos ACP. Depende de TASK-31, así que su encargo sale en cuanto TASK-31
  se acepte y mergee. Después TASK-35 (MH-A13), que depende de TASK-34.
- Tablero: 34 tareas `Done`; en curso TASK-31 y TASK-36; pendientes TASK-34 y TASK-35.

## Aprendido

- **Un registro sin consumidor no es reconciliación.** `CleanupRegistry` con `Drop` cumple la
  parte de "no se pierde la intención", pero si nadie llama a `reconcile_sandboxes` y el diario
  vive solo en memoria, el residuo se pierde igual. La evidencia tiene que ser un llamador
  productivo y un diario durable, no una API y su test.
- **La sonda multiproyecto necesita el entorno completo del lane**: contenedor de egreso con
  puerto publicado (`-p 127.0.0.1:18888:8888`, `scripts/egress-proxy.py` en `rust:1-bookworm`),
  `HODEI_MH_EGRESS_PROXY_CONTAINER`, `HODEI_MH_PROXY_CHECK_URL` y `PORT=8082` si el servidor
  adoptado ocupa 8081. Sin eso fallan pasos que no tienen que ver con el cambio.
- **El gate de workspace puede fallar por flake del propio test**: `supervisor_cli` agotó sus
  20 s esperando `phase: idle` en CI y pasó 3/3 en local y en la reejecución. Se reejecuta el job
  y se registra; no se toca el test sin causa demostrada.
- **La política `git-control` juzga commit por commit al actualizar una ref.** Un trailer
  `Co-authored-by` en un squash de GitHub bloquea el fast-forward y todo push de ramas que lo
  contengan. Remedio: mensaje limpio con el mismo árbol y `--force-with-lease`; nunca
  `--no-verify`.
- Las sondas siguen fuera de `scripts/probes.sh`; TASK-38 y TASK-39 nacieron de esa laguna y
  ahora se ejecutan a mano con el entorno del lane.
