---
id: TASK-52
title: >-
  El carril de sondas está rojo en main: las sondas afirman un run estable en
  fase running que el encadenamiento ya no sostiene
status: In Progress
assignee:
  - '@rubentxu'
created_date: '2026-11-20T19:30:00Z'
updated_date: '2026-11-20T19:31:00Z'
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
- [ ] #1 El supervisor-probe deja de exigir una fase transitoria: sus cuatro checks se reapuntan al invariante real (secuencia del journal: epoca aplicada, confirmada y cerrada; y acuerdo entre CLI y TUI sobre el MISMO snapshot), sin borrar checks ni volverlos vacios, y el detalle de cada check cuenta la verdad sobre lo observado.
- [ ] #2 El step stop-idle-release-new-task-cycle acepta que el stop se rechace porque no hay run vivo (fin honesto de la epoca) y sigue afirmando el invariante completo: release permitido por la puerta durable, DELETE 200, sandbox desaparecido, tarea nueva creada, reclamada, re-admitida y proyecto actualizado compilado en sandbox nuevo.
- [ ] #3 El carril de sondas queda verde en local (supervisor-probe y multiproject-probe, una ronda, con los prerrequisitos que exige scripts/probes.sh) y el carril completo de seis sondas queda verde en CI sobre main.
- [ ] #4 Ninguna asercion nueva depende de una carrera de timing: no se espera a una fase que el ensayo no sostiene, y no se duerme para que algo pase.
<!-- AC:END -->
