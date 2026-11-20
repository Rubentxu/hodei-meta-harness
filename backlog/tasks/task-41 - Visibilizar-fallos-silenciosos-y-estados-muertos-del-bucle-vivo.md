---
id: TASK-41
title: Visibilizar fallos silenciosos y estados muertos del bucle vivo
status: To Do
assignee: []
created_date: '2026-11-20T10:16:00Z'
updated_date: '2026-11-20T10:53:00Z'
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
<!-- SECTION:NOTES:END -->
