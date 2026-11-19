---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-19T01:56:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea.

## Ahora

- **TASK-29 (MH-A11) está EN CURSO y NO está cerrada.** La sesión que la empezó se quedó a medias
  (terminó su turno sin cerrar nada) y el bucle la midió **FAIL**. **Continúa sobre lo que hay, no
  desde cero: el trabajo está casi entero.**
- Repo: `hodei-meta-harness` (`/home/rdcabrera/Proyectos/rust/goal-cycle-mh`), rama
  `task/task-29-supervision-vertical-path`, commit `45420e6` con la primera parte (19 ficheros,
  +969 líneas, y el crate nuevo `crates/hodei-mh-runner/`). `main` sigue en `8ebcecc`.
- **El árbol NO COMPILA.** `cargo check --workspace --all-targets` → `rc=101`:
  `error[E0277]: ObjectiveHandle doesn't implement Debug` en
  `crates/hodei-mh-runner/src/rehearsal.rs:369:61` (una aserción de test formatea `{started:?}`).
  **Arranca por aquí.**
- Sesiones: perfil `cuenta-1` (MiniMax-M3.1-Flash). Lee "Aprendido": este perfil puede emitir el
  markup de las herramientas **como texto**; si te pasa, reintenta la invocación.

## Última tarea cerrada

- **TASK-28 (MH-A05) — Preparar workspaces y comandos sin transportar secretos del host.**
  La exportación del workspace decide qué sale (denylist de salida: metadata de git, caches,
  familia `.env`, claves y credenciales), la redacción ocurre **antes** de truncar o persistir y
  conserva la estructura de la salida, y el fixture local con secretos sintéticos comprueba el
  artefacto y los logs sin proveedor LLM. Los comandos del host no ejecutan URLs/proyectos con
  caracteres especiales y ninguna credencial llega a `argv` ni a los logs.
- Evidencia: PRs #38 y #39 (merge `f154e5b`), commit de cierre `70106c0`; `cargo fmt`,
  `clippy -D warnings` y `scripts/fast-check.sh` PASS.

## Qué queda a medias

- **Rama `task/task-29-supervision-vertical-path`, commit `45420e6`.**
  - Crate nuevo `crates/hodei-mh-runner/` (`core.rs`, `ports.rs`, `rehearsal.rs`, `lib.rs` + tests):
    `GoalRunner` puro con puertos `AgentPort`/`VerifierPort`/`PublisherPort`, la puerta pura
    "sin evidencia no se publica" y el ensayo (`TrialTracker`, `ScriptedAgent`, `ScriptedVerifier`,
    `RecordingPublisher`) **sin `WorkWriter`: el ensayo no puede escribir**.
  - `crates/hodei-mh-run/`: `Phase::Starting` + `RuntimeIdentity{kind,id}` + `RunState::starting/confirm`
    + puerto `ObjectiveRuntime` (el `RunState` sigue siendo UNO).
  - `crates/hodei-mh-supervisor/`: `apply(Start) -> Starting` (nunca `Running` sin confirmación),
    transiciones de `engine` (`confirm_runtime`/`refuse_runtime`), `Snapshot` con runtime y modo
    (`Rehearsal|Live`), `server::serve` recibe el runtime.
  - `crates/hodei-mh/`: `supervisor --rehearsal` cablea el runtime de ensayo; sin él, `Start` se
    rechaza honestamente. Sondas y CLI apuntando al camino nuevo. `docs/profiles.md` actualizado.
- **Lo que falta para cerrar TASK-29** (AC #1..#4):
  1. Que el árbol compile (el error de arriba) y las pruebas de lo ya escrito.
  2. El **test de producto único** `crates/hodei-mh-runner/tests/product_flow.rs`: socket real +
     cliente real + runtime de ensayo → `Start` (Starting) → confirmación → `Running` → `Pause` →
     `Resume` → `Stop` + el caso de error, con fake ACP y tracker, **sin Docker, red ni LLM**
     (AC #2), y que el ensayo **no marque ningún ticket Done** (AC #3).
  3. La prueba de que CLI y TUI observan la **misma** máquina de estado y que `Running` solo se
     alcanza por confirmación del runtime de ensayo o real identificado (AC #1).
  4. La revisión de que **no hay duplicación de decisiones** entre el supervisor y el flujo
     extraído, con los comandos públicos compatibles o la migración documentada (AC #4).
  5. Gates (`cargo fmt`, `clippy -D warnings`, `scripts/fast-check.sh`), PR con merge a `main` y
     cierre de la tarea con su evidencia.
- El plan completo (6 pasos, con lo ya hecho marcado) está en el tablero, en TASK-29
  (Implementation Plan): léelo, no lo rederives.

## Recursos liberados

| Recurso | Qué era | Liberado |
| --- | --- | --- |
| ramas local y remota | `task/task-27-atomic-admission`, `task/task-28-workspace-secretless-export`, `task/task-28-fixture-cleanup` | sí (PRs #37, #38 y #39 mergeados con `--delete-branch`) |
| contenedores y sandboxes | los de las sondas | sí (la pista informó `sandboxes=0 containers=0 networks=0`) |
| procesos auxiliares | servidor de presupuesto en 8099 y lifecycle en 8081 | sí (la pista los cierra) |
| gateway local `qoder2api-hub` | systemd de usuario en 127.0.0.1:8790 | **NO**: sigue en pie a propósito (perfil `cuenta-9`); con *hold* en `/run/user/1001/proxy-hold-qoder2api-hub.service.hold` para que el temporizador de inactividad no lo pare |
| rama `task/task-29-supervision-vertical-path` | trabajo a medias de TASK-29 | **NO**: es la rama de la tarea en curso; se libera al cerrar la tarea |

## Aprendido

- **Un modelo que corta el turno a mitad de tarea se mide FAIL, y el trabajo se pierde**: la
  sesión terminó con `is_error=false` y `stop_reason=end_turn` después de 90 turnos y 38 min, con el
  último mensaje anunciando lo que iba a hacer a continuación. Por eso el trabajo a medias se
  commitea en la rama de la tarea y se documenta aquí: sin eso, la siguiente sesión repite 38
  minutos de diseño.
- **Un perfil puede emitir el markup de las herramientas COMO TEXTO**: con `cuenta-1` (MiniMax)
  el transcript muestra 92 reenvíos del mismo prompt y respuestas del tipo
  `<invoke name="Edit">…<file_path>…` escritos en el contenido del assistant en vez de bloques
  `tool_use`: el cliente no los ejecuta, repite, y la sesión muere (`end_turn`) sin cerrar nada.
  Cuando veas ese markup en tu propia salida, **no lo copies al fichero**: invoca la herramienta.
- **El bucle se cura entre iteraciones**: una sesión que se queda a medias deja su trabajo
  commiteado y documentado, y la siguiente sesión lo continúa y cierra la tarea (TASK-28 se cerró
  así: la primera sesión se quedó a medias y la segunda la terminó).
