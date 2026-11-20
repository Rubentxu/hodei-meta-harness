---
id: TASK-41
title: Visibilizar fallos silenciosos y estados muertos del bucle vivo
status: To Do
assignee: []
created_date: '2026-11-20T10:16:00Z'
updated_date: '2026-11-20T14:01:00Z'
labels:
  - descubrimiento
dependencies: []
priority: medium
ordinal: 41000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Descubrimiento de la revisión independiente de TASK-34 (linea base f3dcec5), fuera del parche inmediato. (1) StoreClaims::release descarta el error del store con let _ (crates/hodei-mh/src/live/claims.rs:61-71) mientras el puerto devuelve (): un claim que no se libera queda como propiedad del run anterior sin que ningún registro lo diga; decidir si el puerto debe devolver Result y cómo se reporta el outcome incierto sin duplicar efectos. (2) LiveError/estado NoNewCommit (crates/hodei-mh-runner/src/live.rs:77) nunca se produce: un agente sin cambios llega como AgentError/Failed en lugar de Rejected, y el estado muerto invita a un match incompleto. (3) install descarta los stops pendientes de runs anteriores (live.rs:234) y solo el abort del spawn lo enmascara; documentar la invariante o eliminarlos explícitamente con evidencia de que ningún intento retrasado puede arrancar después. Sin cambio de comportamiento no demostrado.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Un fallo al liberar un claim deja de descartarse: se propaga al resultado del run como estado incierto visible y queda cubierto por una prueba con un store que falla.
- [ ] #2 El estado NoNewCommit deja de ser código muerto: un agente sin cambios se reporta como rechazo con causa (o el estado se elimina) y el match queda exhaustivo sin ramas inalcanzables, con prueba.
- [ ] #3 Los stops pendientes de runs anteriores no se pierden en silencio al instalar uno nuevo: o se retienen, o una prueba demuestra que ningún intento retrasado puede arrancar después del install.
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Cuarto hallazgo (UAT en vivo 11:21, época 2 de uat3): el resumen de timeout del agente imprime el texto espejado. agent.rs:563-565 hace agent_text.chars().rev().take(200) para quedarse con la cola y lo formatea sin invertirlo de nuevo, así que el error muestra '...ti etaerc em teL' en lugar de 'Let me create it...'. No hay corrupción del stream: es un mensaje de fallo engañoso (bajo impacto, alta confusión). Corregir con prueba que afirme la cola legible.

Quinto hallazgo (UAT run 3, escenario D con --permission deny, 11:43-11:50): la denegación de permiso es invisible para el consumidor. crates/hodei-mh/src/live/agent.rs responde al session/request_permission con {"outcome":{"outcome":"cancelled"}} y no emite ningún evento; los dos epochs terminaron como 'the agent did not finish its turn: timeout' a los 180 s y la única traza de la denegación quedó en el text_tail. Un observador no puede distinguir 'permiso denegado por política' de 'modelo lento', así que la AC3(ii) de TASK-34 no puede darse por demostrada. Arreglo exigible: emitir un evento explícito de denegación (herramienta, motivo, política aplicada) y que el resultado del run lo refleje como rechazo con causa en lugar de timeout genérico; prueba determinista con el fake de ACP, sin sandbox y sin dormir.

Ronda 1 entregada en rama task/task-41-visibility (commits a7153e3, 052be7c, cdf4b42 sobre 1b7cd59). Verificada por el supervisor contra el código:
- (4) text_tail espejado: corregido con readable_tail (agent.rs:626-631), TEXT_TAIL_LIMIT nombrado (agent.rs:173); resumen ahora "timeout after {}s, text_tail={:?}" con la cola legible; dos pruebas offline deterministas sin dormir. Introdujo la tubería AcpPipe (agent.rs:602,611) que drive_agent acepta como dyn; callers de work() y autonomous_probe.rs coerced sin cambios visibles. Encaja con AC2/AC3 sin ampliar alcance.
- (5) denegación invisible: en el refusal branch (agent.rs:687-699) se emite un evento estructurado agent.permission_refused (stderr JSON) con tool/policy/effect (permission_refusal_event, agent.rs:790-802), y el turno termina de inmediato con summary "the permission policy refused..." y refusal Some(event); work() (agent.rs:290-301) propaga la causa en el mensaje del run. Prueba nueva a_denied_permission_ends_the_turn_as_a_refusal_without_waiting (agent.rs:1003) verifica que devuelve ANTES de consumir los eventos encolados (2 restantes intactos). Mapping completo a AttemptEnd::Rejected sigue pendiente de runner/live.rs (ronda 2). AC3(ii) de TASK-34 se vuelve demostrable en la próxima UAT.
- Bonus AGENT_TIMEOUT: doc con el porqué en agent.rs:163-168, mensaje "within AGENT_TIMEOUT (180s); retry or shorten the task" (agent.rs:295-299). Sin flag de CLI (fuera de alcance).
- Gate: scripts/fast-check.sh --all PASS; 15 tests de live::agent verdes.
- (1) StoreClaims::release NO se tocó conscientemente: la firma ClaimPort (runner/live.rs:46), el call site en ClaimGuard::release_now/Drop (live.rs:178,184) y AttemptEnd/outcome_of (live.rs:63-148) están en runner/live.rs, ocupado por el worker de TASK-35. Pasa a ronda 2 junto con (2) NoNewCommit y (3) install stops.
- Hallazgo nuevo dentro del mismo tema: main.rs:704-712 tiene el mismo patrón mirror-tail (chars().rev().take(400) sin devolver) en el detalle del probe acp-prompt-response. No es crítico pero es un mensaje engañoso igual que (4); va en ronda 2.
- Hallazgo nuevo: drive_agent (agent.rs:660-669) hace busy-spin si pipe.next_event() devuelve None inmediatamente (socket cerrado limpio) — el timeout interno de 10 s envuelve la espera, no el reintento; hay que dormir o cortar el loop en Ok(None). Va en ronda 2, con prueba del corte.

Ronda 1 integrada en main (merge fa7dde5). Reconciliación tras el rebase sobre 2986261: el worker de TASK-35 había refactorizado interaction_answer(-> Option<(Value,String)>), la r1 introduce el refusal como tercer elemento Option<Value>. Shape final: permission_answer -> (Value,String,Option<Value>) y interaction_answer -> Option<(String,String,Option<Value>)>; AcpTurn { completed, summary, interactions, refusal }; refusal exit anexa también interactions (commit 5459479) para que ambas salidas sean comparables. fast-check --all PASS; 22 tests live::agent; 122 binarios. Ninguna llamada externa cambió (permission_answer/interaction_answer/AcpTurn no se usan fuera de agent.rs). Ronda 2 en curso (ab332294) en task/task-41-round2 sobre main: (A) StoreClaims::release propaga error, (B) NoNewCommit decidir producir/eliminar, (C) install stops pendientes, (D) main.rs:704 mirrored tail, (E) busy-spin Ok(None).
<!-- SECTION:NOTES:END -->
