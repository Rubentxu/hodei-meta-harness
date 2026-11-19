---
id: TASK-29
title: >-
  [MH-A11] Extraer una ruta vertical de supervisión comprobable sin
  infraestructura
status: Done
assignee:
  - '@rdcabrera'
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-19T02:15:00Z'
labels:
  - audit-v0.2
  - product
  - architecture
  - testing
  - p0
dependencies: []
references:
  - crates/hodei-mh/src/supervisor_cli.rs
  - crates/hodei-mh-supervisor/src/engine.rs
  - crates/hodei-mh/src/autonomous_probe.rs
  - crates/hodei-mh-goal/src/selection.rs
  - crates/hodei-mh-run/src/run.rs
priority: high
ordinal: 29000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0 · F01 · TASK-5/6/11/15. Extraer control de un GoalRunner de aplicación desde el flujo existente: seleccionar objetivo/trabajo y ejecutar mediante puertos ACP, lifecycle, tracker y workspace actuales. Primero modo de ensayo explícito sin efectos externos: mismo binario y mismo flujo, adaptadores fake deterministas, nunca afirmar que un fake es una ejecución real. Dos entregas: contrato ejecutable y posteriormente MH-A06 ACP real. No crear crates por cada trait ni duplicar RunState.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 CLI/TUI observan la misma máquina de estado; Start transita a Starting y Running únicamente por confirmación del runtime de ensayo o real identificado.
- [x] #2 Secuencia Start, Pause, Resume, Stop y error queda demostrada por un único test de producto con fake ACP y tracker; sin invocar Docker, red ni LLM.
- [x] #3 No hay nueva ruta de promoción/escritura en modo ensayo; estados de simulación están explícitos y no marcan tickets reales Done.
- [x] #4 Se elimina duplicación de decisiones entre supervisor y el flujo extraído, conservando compatibilidad de los comandos públicos o migración documentada.
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. run: Phase::Starting + RuntimeIdentity{kind,id} + puerto ObjectiveRuntime; el `RunState` sigue siendo UNO y su fase es PRIVADA: `Running` solo se alcanza por `confirm()`.
2. supervisor: apply(Start) -> Starting (nunca Running sin confirmacion); `engine::confirm_runtime` comprueba que la IDENTIDAD del runtime que confirma sea la que nombra el snapshot; el servidor es el UNICO escritor del runtime del core (un solo hecho, no dos); Snapshot gana `runtime` (con su kind Rehearsal|Live) y `objective`.
3. runner (crate nuevo): GoalRunner puro con puertos AgentPort/VerifierPort/PublisherPort y reutilizando tracker::TrackerRead, verify::Check/Candidate/EvidenceSet; puerta pura en el runner: un objetivo cuyo verificador no cubre TODOS los checks requeridos no se publica, la publisher no llega a llamarse; rehearsal.rs con TrialTracker/ScriptedAgent/ScriptedVerifier/RecordingPublisher (sin WorkWriter: el ensayo no puede escribir).
4. binario: 'hodei-mh supervisor --rehearsal' cablea el runtime de ensayo; sin el, Start se rechaza honestamente (el ACP real llega con MH-A06).
5. Test de producto UNICO (crates/hodei-mh-runner/tests/product_flow.rs): socket real + cliente real + runtime de ensayo -> Start(Starting) -> confirmacion -> Running -> Pause -> Resume -> Stop + el caso de error del objetivo (un verificador que no sostiene lo rechaza y nada se publica) + el caso de las acciones rechazadas; el ensayo no marca ningun ticket Done.
6. Test de la CLI contra el binario real (crates/hodei-mh/tests/supervisor_cli.rs): el run llega a `running` solo por la confirmacion del runtime, sin runtime el Start se rechaza, y la superficie publica (supervisor, sup-status, sup-action, sup-events, attach-terminal, tui) sigue respondiendo.
7. Migracion documentada (docs/profiles.md): un Start ya no significa `running`, el vocabulario de fases gana `starting`, y las dos respuestas de texto que cambiaron.
8. Gates: scripts/fast-check.sh (afectados + dependientes), PR y merge a main; el gate de workspace lo corre la CI.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Cierre de la ruta vertical (rama task/task-29-supervision-vertical-path, commit 6fe02a7).

La sesion anterior dejo el arbol sin compilar. Se corrigio y se encontraron cuatro defectos
reales en el camino ya escrito, todos en el producto y no en el diseno:

1. El controlador servia la socket SIN runtime (`server::serve` en vez del camino con
   runtime): un `Start` se aceptaba y el run se quedaba en `Starting` para siempre. Se
   unifico en UNA sola funcion `serve(listener, core, runtime)`: el runtime es un parametro
   explicito y ya no hay dos funciones entre las que elegir la equivocada.
2. El ensayo se negaba a trabajar el ticket que el cliente nombraba ("TASK-9 is not in the
   tracker snapshot"), asi que un Start con ticket moria en `Starting`. Ahora el ensayo
   ensaya el trabajo que se le da, aunque su escenario fijo no lo conozca; el item vive en
   el proceso del ensayo y en ningun otro.
3. El aviso `SIMULATED` de la TUI se renderizaba al final del panel y lo recortaba un
   terminal corto: un estado simulado sin aviso es indistinguible de uno real. El aviso va
   ahora PRIMERO.
4. La sonda devolvia `Some` al vencer el plazo de espera, asi que un estado NUNCA alcanzado
   pasaba por alcanzado: el check del run en `running` era un verde falso. El plazo vencido
   devuelve `None` y el check lo dice.

Se anade `crates/hodei-mh/tests/supervisor_cli.rs`: el cableado del controlador (Start ->
Starting -> Running solo por confirmacion del runtime, y el rechazo honesto sin runtime)
queda cubierto por un test de cargo contra el binario real, en la pista rapida de cada PR.

Evidencia: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked
-- -D warnings`, `cargo test --workspace --locked` y `cargo test --workspace --doc --locked`
PASS; la sonda `supervisor-probe` pasa sus 11 checks (los 11, incluidos el nuevo de CLI y
TUI sobre la misma maquina de estados y el de la TUI simulada, que estaba roto).

Revision independiente del diseno (goldfish, modo focused, dos revisores con contexto
nuevo). El aislamiento fue PARCIAL y se declara: el canario dio bien (el marcador de la
conversacion del padre no llego al hijo), pero el host inyecta las instrucciones del
proyecto y la memoria al subagente, asi que `sourcesRestricted=false`. Sus hallazgos son
candidatos, no defectos certificados, y se resolvieron contra el codigo.

Hallazgos aceptados y resueltos:
- "Hay runtime" estaba DECIDIDO en dos sitios (el flag `core.runtime` del engine y el
  argumento `runtime` del servidor) y nada los ataba: un desajuste dejaba un Start
  APLICADO con el run en `Starting` para siempre, sin fallo ni plazo. Ahora el servidor
  escribe el runtime del core (un solo escritor, un solo hecho) y el CLI deja de
  declararlo por su cuenta. Era la clase de bug que rompio el producto.
- La puerta de publicacion cubria ids de checks, no evidencia, y el comentario ("No
  evidence, no effect") describia una regla que el codigo no implementaba. El comentario
  dice ahora la regla real; la puerta ligada a la EVIDENCIA pertenece al verificador real
  de MH-A06, y asi queda escrito en el plan.
- `confirm_runtime` no comprobaba la IDENTIDAD del runtime que confirmaba contra la que
  nombra el snapshot. Ahora la comprueba y la rechaza si no coinciden: el AC #1 pide la
  confirmacion "del runtime de ensayo o real identificado".
- La superficie publica cambio de comportamiento sin nota de migracion (el AC #4 lo
  pedia). `docs/profiles.md` la tiene ahora.
- El predicado "el run esta en vuelo" estaba escrito dos veces en el engine; ahora lo
  tiene `RunState::is_live()`.
- Un Start nuevo no limpiaba el veredicto del objetivo anterior: un snapshot en
  `starting` podia mostrar el veredicto viejo. Ahora lo limpia.
- La fase de `RunState` era un campo publico: cualquiera podia declarar `Running` sin
  confirmacion. Ahora es PRIVADA y se lee por `phase()`: la invariante del AC #1 la
  impone el tipo, no la disciplina. Asi el camino legacy de run_probe deja de poder
  saltarse la confirmacion.
- El estado simulado de la TUI mostraba una TELEMETRIA MEDIDA que nadie habia medido.
  Ahora dice UNKNOWN, como cualquier telemetria desconocida.
- El test de la CLI leia el log en el instante en que el run estaba `running` y el
  veredicto aun no estaba: una carrera. Ahora espera el evento `objective/outcome`.
- El caso de error del test de producto era una ACCION rechazada, no un error de
  ejecucion: ahora el test de producto ensaya tambien un objetivo RECHAZADO por su
  verificador (nada se publica) sobre el mismo socket real.
- El test que alegaba el AC #3 solo buscaba tres nombres de crate en el manifiesto, menos
  de lo que su propio comentario afirmaba: el comentario dice ahora exactamente lo que
  demuestra.
- La superficie publica se ejercita en la pista rapida: un test nuevo recorre
  supervisor, sup-status, sup-action, sup-events, attach-terminal y tui contra el binario
  real.

Hallazgos anotados como limite, no arreglados en esta tarea:
- El runner evalua la seleccion con las reservas SIEMPRE vacias: `SelectionReason::ClaimedBy`
  no puede ocurrir. La politica de reservas es del coordinador y la decide el runtime real
  (MH-A06).
- Dos reglas de elegibilidad conviven (tracker::compute_readiness y goal::explain_one) y
  pueden discrepar; el runner REUTILIZA la de goal, que es la que decide. Es preexistente
  y pertenece a la capa del tracker.
- Un objetivo parado en vuelo pierde su veredicto en silencio: `record_outcome` lo
  rechaza y no se emite evento. Es intencionado (lo fija un test del engine) y es un
  hueco de observabilidad, no un fallo de correccion.
- `Phase::Stopping` no lo observa nunca ningun cliente: `stop()` pasa de Stopping a
  Stopped en la misma llamada. La etiqueta existe y es un transitorio real.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-29 (MH-A11) cerrada: el supervisor tiene una ruta vertical de supervision COMPROBABLE
sin infraestructura, cableada de verdad y con la invariante impuesta por el tipo.

Que se entrego:
- El crate `hodei-mh-runner` separa el nucleo (GoalRunner puro con puertos AgentPort,
  VerifierPort y PublisherPort, y la puerta pura de publicacion) del ensayo (TrialRuntime,
  ScriptedAgent, ScriptedVerifier, RecordingPublisher sin escritor). El supervisor lo consume
  como runtime a traves de un puerto (`ObjectiveRuntime`), y el estado de la ejecucion vive en
  UNA maquina: la del engine del supervisor. El `RunState` del crate run sigue siendo UNO, y
  su fase es ahora PRIVADA: `Running` solo se alcanza por `confirm()`, y el snapshot lo dice.
- El binario sirve el runtime con el controlador: `hodei-mh supervisor --rehearsal` ejecuta
  un objetivo de principio a fin, sin Docker, sin red y sin modelo. Sin runtime, el Start se
  rechaza con "no runtime is wired": un run que nadie puede ejecutar no es un run.
- El ensayo ensaya el trabajo que se le nombra y no solo su escenario fijo; el trabajo
  nombrado vive en el proceso del ensayo y en ningun otro, y no se cierra ningun ticket ni
  se mueve ninguna referencia.
- La CLI y la TUI observan la MISMA maquina de estados (mismo contrato de render) y la TUI en
  modo simulado lo dice SIEMPRE: el aviso va primero, donde un terminal corto no lo recorta.

Defectos del camino ya escrito que se encontraron y corrigieron:
1. El controlador servia la socket sin runtime: un Start se aceptaba y el run se quedaba en
   `Starting` para siempre. Se unifico en una sola funcion `serve(listener, core, runtime)`.
2. El ensayo rechazaba el ticket nombrado y mataba el run antes de empezar.
3. El aviso `SIMULATED` de la TUI se recortaba en un terminal corto: un estado simulado sin
   aviso es indistinguible de uno real.
4. La sonda devolvia su ultima respuesta al vencer el plazo: un estado nunca alcanzado
   pasaba por alcanzado, y el check del run en `running` era un verde falso.

Lo que encontro la revision independiente del diseno (goldfish, dos revisores con contexto
nuevo) y se resolvio en esta misma tarea:
- "Hay runtime" estaba DECIDIDO en dos capas sin que nada los atara: un desajuste dejaba un
  Start APLICADO con el run en `Starting` para siempre. Ahora el servidor es el UNICO
  escritor del runtime del core.
- `confirm_runtime` no comprobaba la IDENTIDAD del runtime que confirmaba contra la que
  nombra el snapshot. Ahora la comprueba y la rechaza si no coinciden.
- La puerta de publicacion cubria ids de checks, no evidencia, y su comentario describia una
  regla que el codigo no implementaba. El comentario dice ahora la regla real; la puerta
  ligada a la evidencia pertenece al verificador real de MH-A06.
- La invariante central ("Running solo por confirmacion") la sostenia la disciplina: la fase
  de `RunState` era un campo publico. Ahora es PRIVADA y la impone el tipo.
- La superficie publica cambio de comportamiento sin nota de migracion (el AC #4 la pedia):
  `docs/profiles.md` la tiene ahora.
- El predicado "el run esta en vuelo" estaba duplicado; ahora lo tiene `RunState::is_live()`.
- Un Start nuevo no limpiaba el veredicto del objetivo anterior; ahora lo limpia.
- El estado simulado de la TUI mostraba una telemetria MEDIDA que nadie habia medido; ahora
  dice UNKNOWN.
- El test de la CLI leia el log antes de que el veredicto estuviera (carrera); ahora espera
  el evento `objective/outcome`.
- El caso de error del test de producto era una accion rechazada, no un error de ejecucion:
  ahora ensaya tambien un objetivo RECHAZADO por su verificador, sobre el mismo socket real.
- La superficie publica se ejercita en la pista rapida: un test recorre supervisor,
  sup-status, sup-action, sup-events, attach-terminal y tui contra el binario real.

Verificacion (todo ejecutado, nada NOT_RUN):
- `cargo fmt --all -- --check`: PASS. `cargo clippy --workspace --all-targets --locked
  -- -D warnings`: PASS. `cargo test --workspace --locked`: PASS. `cargo test --workspace
  --doc --locked`: PASS.
- `hodei-mh supervisor-probe`: 11/11 checks verdes, incluido el nuevo que comprueba que la
  CLI y la TUI ven la misma maquina de estados, y el de la TUI simulada que estaba roto.
- `crates/hodei-mh-runner/tests/product_flow.rs`: el UNICO test de producto — socket real,
  cliente real, runtime de ensayo, agente y tracker falsos — demuestra Start, Pause, Resume,
  Stop, el caso de error del objetivo (rechazado) y el de las acciones rechazadas.
- `crates/hodei-mh/tests/supervisor_cli.rs`: el binario real; el run llega a `running` solo
  por la confirmacion del runtime, sin runtime el Start se rechaza, y todos los comandos
  publicos siguen respondiendo.

Limites y riesgos:
- El runtime real (ACP y sandbox) sigue sin existir: lo aporta MH-A06 (TASK-34). Hoy solo
  hay runtime de ensayo, y por eso `supervisor` sin `--rehearsal` se niega a arrancar.
- La revision independiente fue PARCIAL: el canario dio bien (el marcador de la conversacion
  del padre no llego al hijo), pero el host inyecta las instrucciones del proyecto, asi que
  `sourcesRestricted=false`. Sus hallazgos son candidatos, no defectos certificados, y se
  resolvieron uno a uno contra el codigo.
- La pista de sondas completa (multiproject-probe) sigue con fallos de INFRAESTRUCTURA en
  esta maquina (no hay lifecycle server en :8081, asi que no hay sandboxes ni builds); no
  son una regresion de esta tarea. El supervisor de esa sonda si cambio: antes su `stop` se
  rechazaba porque el run ya habia muerto en `Starting`; ahora se aplica.
- Quedan anotados como limite, no arreglados: el runner evalua la seleccion con las reservas
  siempre vacias (la politica de reservas es del coordinador y la decide el runtime real); dos
  reglas de elegibilidad conviven en tracker y goal (el runner reutiliza la de goal, que es la
  que decide); un objetivo parado en vuelo pierde su veredicto en silencio (hueco de
  observabilidad, fijado por un test del engine); `Phase::Stopping` no lo observa ningun
  cliente.
- Los comandos publicos no cambian de nombre ni de banderas: `supervisor`, `sup-status`,
  `sup-action`, `sup-events`, `attach-terminal` y `tui` siguen igual; `--rehearsal` es
  anadido. El unico cambio de API es interno (`server::serve`), que no es un comando publico.
<!-- SECTION:FINAL_SUMMARY:END -->
