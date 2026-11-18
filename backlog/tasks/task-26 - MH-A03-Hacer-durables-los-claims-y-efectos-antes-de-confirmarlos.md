---
id: TASK-26
title: '[MH-A03] Hacer durables los claims y efectos antes de confirmarlos'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-18T11:49:00Z'
updated_date: '2026-11-18T22:17:00Z'
labels:
  - audit-v0.2
  - reliability
  - coordinator
  - p0
dependencies: []
references:
  - crates/hodei-mh-coordinator/src/actor.rs
  - crates/hodei-mh-coordinator/src/core.rs
  - crates/hodei-mh-coordinator/src/store.rs
  - crates/hodei-mh-promote/src/ledger.rs
priority: high
ordinal: 26000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0 · F03/F12 · TASK-4. actor.rs:88-141 ignora save, puede conceder claim tras rollback y ejecutar efectos no persistidos. core.rs:89-96 acepta epochs futuros y :136-139 permite reemitir estados no inciertos; store.rs:48-85 reescribe colecciones. Usar escritor único real con exclusión entre procesos y transacción antes de responder, estado por proyecto fuera del workspace, fencing exacto por intento. No añadir un segundo ledger de trabajo; Backlog sigue siendo autoridad del ticket.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Fallo inyectado de escritura no devuelve Granted, Released ni dispara efecto; el error es observable.
- [x] #2 Dos coordinadores sobre la misma identidad no conceden el mismo claim; tickets de igual ID en distintos proyectos no colisionan.
- [x] #3 Eventos de epoch viejo o futuro sin autorización y effect IDs duplicados/conflictivos no ejecutan ni alteran el intento actual.
- [x] #4 Reinicio recupera claims e intenciones pendientes como reconciliables; prueba SQLite temporal y fallo de persistencia sin Docker.
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Dominio puro (core.rs): el estado pertenece a UN proyecto (el id de ticket es unico dentro del proyecto, nunca global); el epoch es el token de fencing de la tentativa y solo el titular de la claim vigente puede aplicar un evento con ESE epoch (epoch viejo o futuro: inerte); el effect id es clave de idempotencia: un id ya registrado no se vuelve a ejecutar, un id conflictivo se rehusa sin alterar la tentativa, y un efecto confirmado (Applied) ya no se reemite.

2. Store (store.rs): la mutacion se ejecuta dentro de una transaccion BEGIN IMMEDIATE (el lock de escritura es la exclusion real entre procesos) y se persiste en DELTAS, no reescribiendo las colecciones; el archivo registra el proyecto al que pertenece y abrirlo como otro proyecto se rehusa; si la escritura falla no se confirma nada y el error es observable.

3. Actor (actor.rs): cada mutacion (claim, release, efecto) es una transaccion; un fallo de escritura NO devuelve Granted ni Released, NO dispara el efecto y el error llega a quien lo pidio; el efecto se construye solo cuando su intencion queda durable (un id duplicado nunca lo construye); el evento se evalua contra el estado confirmado.

4. Estado por proyecto FUERA del workspace: el estado durable vive en el directorio de estado del proyecto (<state-root>/hodei-mh/<project>/coordinator.db), nunca dentro del arbol de trabajo del proyecto.

5. Pruebas (SQLite temporal, sin Docker): fallo de escritura inyectado (trigger de SQLite) en claim/release/efecto; dos coordinadores sobre la misma identidad (dos conexiones y dos procesos) que no conceden la misma claim; el mismo id de ticket en dos proyectos que no colisiona; eventos con epoch viejo, futuro o sin autorizacion que no alteran la tentativa; effect id duplicado y conflictivo; reinicio que recupera claims e intenciones pendientes como reconciliables.

6. Verificar: cargo test del crate y de los crates que dependen de el, cargo fmt --check y cargo clippy -D warnings.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementado en `crates/hodei-mh-coordinator` (dominio puro, store transaccional, actor) y en `paths.rs` para el estado por proyecto.

**Qué cambió**

- `core.rs`: el estado es de UN proyecto (`CoordinatorState::new(project)`): el id de ticket es único DENTRO del proyecto. `accepts_event` solo acepta el evento del titular vigente y con ESE epoch (epoch viejo, epoch futuro o otro titular: inerte, y la tentativa no se altera). `record_effect` exige el epoch de la claim vigente (rehusa `NotClaimed` o `StaleEpoch`), trata el id repetido con la misma intencion como duplicado (`may_run = false`, la intencion registrada no se altera) y rehusa el id conflictivo (`EffectConflict`) sin tocar la tentativa. `can_reemit` solo es cierto para un efecto observado NO aplicado: un efecto confirmado ya no se reemite. Se anadio `NotApplied` al estado del efecto para distinguir "nunca corrio" de "todavia no se sabe".
- `store.rs`: cada mutacion es una transaccion `BEGIN IMMEDIATE` — el lock de escritura es la exclusion real entre procesos (con `busy_timeout` de 10 s) —, la lectura que decide ocurre DENTRO de la transaccion y los cambios se persisten en DELTAS (las colecciones no se reescriben nunca). El archivo registra el proyecto al que pertenece y abrirlo como otro se rehusa (`IdentityMismatch`). `recover_pending_effects` convierte cada intencion PENDING en UNCERTAIN al reiniciar: nadie puede afirmar si su efecto corrio.
- `actor.rs`: claim, release, efecto y evento son transacciones; un fallo de escritura devuelve el error (nunca Granted, nunca Released, nunca dispara el efecto). El efecto se CONSTRUYE (factory) solo cuando su intencion es durable y puede correr: un id duplicado nunca lo construye, asi que el efecto ni existe. `Query` lee el estado confirmado y `join` devuelve el fallo de persistencia que no tenia a quien reportarlo.
- `paths.rs`: el estado durable de un proyecto vive en `<state-root>/hodei-mh/<project>/coordinator.db` (`$XDG_STATE_HOME`, o `~/.local/state`), FUERA del workspace; el nombre del proyecto es un unico segmento y no puede escapar de su directorio.
- `multiproject_probe.rs`: una coordiandora por proyecto. El MISMO id de ticket se reclama en los cuatro proyectos y se concede en los cuatro; un segundo solicitante del mismo id en el mismo proyecto se rechaza.

**Dientes de las pruebas (comprobado, no supuesto)**

- Con la lectura FUERA de la transaccion (el comportamiento anterior) FALLA `the_second_coordinator_waits_for_the_decision_of_the_first`: los dos coordinadores conceden la misma claim.
- Con el comportamiento anterior de "responder igual" (la escritura fallida se ignoraba) FALLAN `a_write_failure_does_not_grant_a_claim`, `a_write_failure_does_not_release_a_claim`, `a_write_failure_does_not_run_an_effect` y `a_conflicting_effect_id_is_refused`.
- El fallo de escritura inyectado es REAL: un trigger de SQLite (`RAISE(ABORT)`) creado desde otra conexion, no un simulacro del codigo.

**Verificacion**

- `cargo test --workspace` en verde (29 pruebas del coordinador: 11 de dominio, 6 de store, 10 de actor y 2 de paths).
- `cargo fmt --all --check` y `cargo clippy --all-targets` limpios; `scripts/fast-check.sh` PASS.
- Pista de sondas contra Docker 29.1.3 real: `scripts/probes.sh --probe multiproject-probe` PASS, sin residuo (sandboxes=0 containers=0 networks=0). La sonda midio que el mismo id en cuatro proyectos se concede en los cuatro.
- `hodei-mh run-probe` (fuera de la pista): sus checks de coordinador PASS; su check `pipe-attach` falla con 401 porque la sonda no entrega la credencial del execd cerrada en TASK-25. Es un fallo PREEXISTENTE, reproducido tambien sobre el codigo base, y queda registrado como TASK-38.

**Limites (deliberados, con su destino)**

- La exclusion es el lock de escritura de SQLite: serializa a los coordinadores que comparten el archivo. Cada proyecto tiene el suyo, y un archivo compartido por dos proyectos se rehusa.
- El estado durable es por proyecto y por archivo. El actor puede arrancar sobre un archivo cualquiera (`start(project, path)`), asi que la ubicacion canonica fuera del workspace la ofrecen `start_in_state_dir` y `paths`; el camino de producto que la usara es TASK-33/TASK-34.
- Un effect id se emite como mucho una vez: un id ya registrado no se vuelve a ejecutar. Solo un efecto reconciliado como NO aplicado puede volver a emitirse, y entonces vuelve a PENDING.
- `join` devuelve error si la persistencia del desenlace de un efecto fallo; el estado final se puede leer del store.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
El coordinador delega: nada se responde antes de ser durable.

**Como funciona ahora.** Cada mutacion (claim, release, intencion de efecto, evento) es una transaccion `BEGIN IMMEDIATE`: la lectura que decide ocurre dentro de la transaccion y los cambios se persisten en deltas antes de que el commit responda. El lock de escritura de SQLite ES la exclusion entre procesos (con 10 s de espera), no una convencion: dos coordinadores sobre el mismo archivo se serializan, y el segundo ve la decision del primero. El estado pertenece a un proyecto: el id de ticket es unico dentro del proyecto, el archivo recuerda el proyecto al que pertenece y se niega a abrirse como otro, y el estado durable vive fuera del workspace, en el directorio de estado del proyecto (`$XDG_STATE_HOME/hodei-mh/<project>/` o `~/.local/state/hodei-mh/<project>/`).

**El fencing.** El epoch es el token de la tentativa: solo el titular de la claim vigente puede aplicar sus eventos y solo con ESE epoch, de modo que un evento viejo (duplicado tardio), uno futuro (que no nos corresponde conceder) y uno de otro titular son inertes y no alteran la tentativa en vuelo. Un efecto se registra con el epoch de su claim y con un id que es clave de idempotencia: un id repetido con la misma intencion NO vuelve a ejecutar el efecto, un id conflictivo se rehusa sin tocar la tentativa, y solo un efecto observado NO aplicado puede volver a emitirse — un efecto confirmado no se reemite nunca.

**El reinicio.** Las claims se recuperan y cada intencion que estaba PENDING vuelve como UNCERTAIN, porque nadie puede afirmar si su efecto corrio: son reconciliables contra el estado observado, no reemitibles a ciegas.

**Verificacion.** `cargo test --workspace` en verde (29 pruebas del coordinador), `cargo fmt --all --check` y `cargo clippy --all-targets` limpios, `scripts/fast-check.sh` PASS. Las pruebas tienen dientes, y se comprobo: contra la lectura fuera de la transaccion falla la exclusion entre coordinadores, y contra el "responder igual" de antes fallan los tres caminos de fallo de escritura. El fallo inyectado es real (un trigger de SQLite creado desde otra conexion). La pista de sondas contra Docker 29.1.3 real pasa (`multiproject-probe`, sin residuo) y mide que el mismo id de ticket en cuatro proyectos se concede en los cuatro.

**Limites.** La exclusion serializa a quienes comparten el archivo; cada proyecto tiene el suyo. El actor admite un archivo cualquiera (`start(project, path)`), y la ubicacion canonica fuera del workspace la ofrecen `start_in_state_dir` y `paths`: el camino de producto que la usara es TASK-33/TASK-34. `join` devuelve el fallo de persistencia que no tenia a quien reportarlo. No se anade un segundo ledger de trabajo: Backlog sigue siendo la autoridad del ticket.
<!-- SECTION:FINAL_SUMMARY:END -->
