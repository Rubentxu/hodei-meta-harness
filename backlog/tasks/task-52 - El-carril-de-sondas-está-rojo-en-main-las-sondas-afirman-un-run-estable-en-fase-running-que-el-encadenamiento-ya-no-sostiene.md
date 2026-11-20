---
id: TASK-52
title: >-
  El carril de sondas está rojo en main: las sondas afirman un run estable en
  fase running que el encadenamiento ya no sostiene
status: Done
assignee:
  - '@rubentxu'
created_date: '2026-11-20T19:30:00Z'
updated_date: '2026-11-20T20:13:00Z'
labels:
  - descubrimiento
  - bug
  - ci
  - probes
dependencies: []
references:
  - crates/hodei-mh/src/supervisor_probe.rs
  - crates/hodei-mh/src/multiproject_probe.rs
  - crates/hodei-mh-supervisor/src/engine.rs
  - scripts/probes.sh
priority: high
ordinal: 52000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Descubierto al auditar el cierre del ciclo: el tablero dice 51/51 Done, pero el carril de sondas de CI está ROJO desde 2026-11-20T11:13Z. Run 44799959858 (main): 'workspace gate' SUCCESS, 'probe lane' FAILURE. Artefactos del run: probe-output/supervisor-probe-1.json (4 checks FAIL) y probe-output/multiproject-probe-1.json (1 step FAIL).

Checks que fallan en supervisor-probe:
- actions-drive-the-same-contract-confirmation-is-real: 'the run reached running only AFTER the runtime confirmed it - IT NEVER REACHED running'.
- tui-real-session-renders-live-supervisor-state: el TUI no pinta phase:running/ticket:TASK-9.
- the-cli-and-the-tui-observe-the-same-state-machine: CLI: phase=false, ticket=false, runtime=false.
- attached-terminal-read-only-and-disconnect-is-inert: despues del viewer el objetivo ya no esta 'running'.
Step que falla en multiproject-probe:
- stop-idle-release-new-task-cycle: 'stop applied=1 (refused: no running run to stop)'; el resto del invariante se cumplio (release permitido, DELETE 200, gone, tarea nueva creada, reclaimed, re-admitted, build del proyecto actualizado).

Causa probable: tras el encadenamiento de objetivos (TASK-34, commits 7d86143 y ed44515) el runtime de ensayo confirma al instante y la epoca 1 termina, de modo que un run ya no se sostiene en fase 'running'. El propio comentario del probe (supervisor_probe.rs:337-348, escrito en TASK-45) dice que un ensayo nunca se queda estable en 'running': las sondas affirmation obsolete de una fase transitoria. El carril de sondas es la evidencia del producto: no se puede debilitar ni borrar un check para ponerlo verde; hay que reapuntarlo al invariante real.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 El supervisor-probe deja de exigir una fase transitoria: sus cuatro checks se reapuntan al invariante real (secuencia del journal: epoca aplicada, confirmada y cerrada; y acuerdo entre CLI y TUI sobre el MISMO snapshot), sin borrar checks ni volverlos vacios, y el detalle de cada check cuenta la verdad sobre lo observado.
- [x] #2 El step stop-idle-release-new-task-cycle acepta que el stop se rechace porque no hay run vivo (fin honesto de la epoca) y sigue afirmando el invariante completo: release permitido por la puerta durable, DELETE 200, sandbox desaparecido, tarea nueva creada, reclamada, re-admitida y proyecto actualizado compilado en sandbox nuevo.
- [x] #3 El carril de sondas queda verde en local (supervisor-probe y multiproject-probe, una ronda, con los prerrequisitos que exige scripts/probes.sh) y el carril completo de seis sondas queda verde en CI sobre main.
- [x] #4 Ninguna asercion nueva depende de una carrera de timing: no se espera a una fase que el ensayo no sostiene, y no se duerme para que algo pase.
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Diagnóstico (supervisor): el carril de sondas estaba rojo en `main` aunque el tablero decía 51/51 Done. Run 44799959858: `workspace gate` SUCCESS, `probe lane` FAILURE. Los artefactos del run se descargan con `gh run download <run> -D <dir>` (desde dentro del repo) y ahí está el JSON de cada probe con el check que falló. `supervisor-probe` fallaba en 4 checks y `multiproject-probe` en 1 step; reproducido en local con la misma firma.

Causa: el encadenamiento de objetivos de TASK-34 (`7d86143`) hace que el runtime de ensayo confirme al instante y la época termine en milisegundos, así que un run ya NO se sostiene en fase `running`. Las sondas afirmaban esa fase transitoria (supervisor_probe.rs:314, :422, :440, :489 antes del arreglo) y el step del multiproject exigía un `stop` aplicado sobre un run que ya había terminado (multiproject_probe.rs:893 antes del arreglo). Además el `detail` mentía por construcción: afirmaba 'the confirmation never happened' cuando el journal del propio CI mostraba la confirmación.

Revisión independiente (goldfish, contexto nuevo, solo lectura): NO REFUTADA. P1 sí (el ensayo no sostiene `running`), P2 sí (el delta que rompe el carril es exactamente el commit de encadenamiento y no toca las sondas), P4 no (el viewer NO tiene poder de escritura: `Request::Events` -> `engine::events_since`, `follow_up: Nothing`; el check era vacuo), P5 el fin honesto de la época. Aportó dos correcciones que el arreglo incorpora: el detalle que mentía y la vacuidad del check del viewer.

Arreglo (PR #49, `8d20acb`; solo dos ficheros de sonda, ni el motor ni el runtime de ensayo):
- `supervisor_probe.rs`: `JournalSummary` gana `first_run_confirmed`, `first_run_ended_with` y `second_run_ended`; `first_epoch_is_settled` exige la confirmación del runtime (provenance automática) Y el veredicto que terminó la época. El check de las acciones ya no espera una fase: afirma la secuencia real y que el snapshot nombra el ticket y la revisión del Start aplicado. La TUI debe pintar el MISMO valor que la CLI en los 12 campos del contrato (`SHARED_SNAPSHOT_FIELDS`) y no declarar nada simulado. El check del viewer compara el snapshot ANTES y DESPUÉS (idénticos), lo que sí distingue 'inerte' de 'fuga'.
- `multiproject_probe.rs`: `StopEvidence` lee el journal y solo acepta un Stop honesto: o paró un run vivo (snapshot `phase: stopped` y `applied=true`), o se rechazo con su motivo y el veredicto de fin es ANTERIOR en secuencia al Stop. El resto del invariante del ciclo (release durable, DELETE 200, sandbox desaparecido, tarea nueva, reclamo, re-admision, build del proyecto actualizado) queda intacto.

Verificacion (supervisor, sobre el `main` fusionado): `cargo test -p hodei-mh` 158+14 verdes; `cargo fmt --check` y `cargo clippy --all-targets -- -D warnings` verdes; `scripts/fast-check.sh` PASS; carril completo `scripts/probes.sh --rounds 1` con las 6 sondas en PASS y 0 residuos (sandboxes=0, containers=0, networks=0); CI en `main` (run 44808413572) con `workspace gate` SUCCESS y `probe lane` SUCCESS. Ningun check se borro ni se volvio constante, y las pruebas nuevas discriminan: `an_epoch_the_runtime_never_confirmed_is_not_settled`, `an_epoch_that_never_ended_is_not_settled`, `a_surface_that_shows_another_value_does_not_agree`, `a_field_that_a_surface_does_not_render_is_never_agreement`, `a_stop_that_came_after_the_verdict_is_honest`, `a_stop_that_came_before_the_verdict_stopped_a_live_run`.

Riesgo residual y cobertura: tras el reapuntado, ninguna sonda de CI observa una fase `running` sostenida ni un `stop` aplicado sobre el socket, porque el ensayo no las sostiene. Esa transicion sigue cubierta offline en la ruta productiva (`crates/hodei-mh-runner/tests/product_flow.rs:106-173`: Starting->Running y Stop aplicado). La alternativa (un ensayo sostenido via `TrialRuntime::gated()`) es la decision de arquitectura que TASK-45 dejo anotada; no se toma aqui.

Correccion de higiene: el worker dejo el contenedor de proxy `hodei-verify-t52-proxy` ocupando el puerto 18888; retirado antes de la verificacion del supervisor.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
El carril de sondas de CI vuelve a estar verde en `main`. La causa no era el producto: tras el encadenamiento de objetivos de TASK-34 el runtime de ensayo confirma al instante y la epoca termina en milisegundos, de modo que un run ya no se sostiene en fase `running`, y las sondas afirmaban esa fase transitoria (ademas de mentir en su `detail` y de dejar vacuo el check del viewer).

Las sondas se reapuntan al invariante que el contrato real sostiene: el journal (`sup-events --jsonl`) debe mostrar la epoca aplicada, confirmada por el runtime con provenance automatica y cerrada por su veredicto; la CLI y la TUI deben llevar el mismo valor en los doce campos del contrato de snapshot; el viewer debe dejar el estado intacto antes y despues; y el `Stop` solo es honesto si paro un run vivo o si el journal prueba que el veredicto de fin es anterior a la accion. Ningun check se borro, ninguno se volvio constante y no se introduced ninguna espera por una fase que no se sostiene.

Verificacion: `cargo test -p hodei-mh` 158+14, gate local `scripts/fast-check.sh` PASS, carril completo de 6 sondas PASS en local sin dejar residuos, y en CI (`main`, run 44808413572) `workspace gate` y `probe lane` en SUCCESS. Merge: PR #49 (`8d20acb`).
<!-- SECTION:FINAL_SUMMARY:END -->
