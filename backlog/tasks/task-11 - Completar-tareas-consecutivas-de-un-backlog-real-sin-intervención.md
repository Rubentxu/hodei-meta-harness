---
id: TASK-11
title: Completar tareas consecutivas de un backlog real sin intervención
status: Done
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-17T22:08:00Z'
labels:
  - gc-mvp
  - gc-11
  - hodei-mh
dependencies:
  - TASK-6
  - TASK-7
  - TASK-8
  - TASK-10
ordinal: 11000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Integrar la unidad finita de trabajo con el ciclo infinito, usando verificación/promoción y renovación. Un único camino de ejecución de producción: ACP + OpenSandbox. Automatización por defecto en un entorno de prueba con tareas configuradas. No añadir orquestación de equipos.

Clave de incorporación: GC-11.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Un backlog fixture con cinco tareas y dependencias se completa en orden válido sin repetir tickets ni intervención ordinaria.
- [x] #2 Cada tarea inicia una conversación nueva y la siguiente usa la rama promovida; los cambios anteriores siguen disponibles.
- [x] #3 Un objetivo nativo que se declara complete sin evidencia válida no cierra el ticket.
- [x] #4 La adición de una tarea después de Idle reactiva el flujo; Stop impide reactivación por cambios del tracker.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 E2E en Claude Code real con logs redactados e IDs observados.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Evidencia TASK-11 (rama task/task-11-autonomous): subcomando hodei-mh autonomous-loop — UN único camino de producción (ACP + OpenSandbox) que integra la unidad finita con el ciclo infinito sobre un fixture REAL (repo git + backlog init real + 5 tickets encadenados por dependencias TASK-1..5, managed branch hodei-mh/main). Por cada tarea SIN intervención: (1) conversación NUEVA (sandbox efímero + sesión PTY nueva del agente real claude-code-acp vía gateway, staging del adaptador pack+upload+extract por falta de egress); (2) el estado PROMOVIDO actual se shippea como bundle al sandbox (la siguiente tarea parte de la rama promovida: AC2); el agente real ejecuta la tarea (crea stepN.txt + git commit); (3) el candidato se exporta como bundle del sandbox; (4) VERIFY en entorno LIMPIO (CleanRunner: bundle→clone→criterio del ticket); (5) PROMOTE por CAS atómico con ledger write-ahead; (6) CIERRE del ticket vía CLI oficial con relectura real (status Done verificado por lectura). Resultados: 5/5 ciclos verdes — TASK-1..5 verificadas, promovidas (commits b757..9006 observados) y cerradas en orden válido sin repetir ni intervención (AC1); fresh-conversations-on-promoted-branch ✓ (AC2); AC3 ejercitado con agente REAL: tarea trampa donde el agente no produce el artefacto → evidencia inválida → el ticket TASK-6 del fixture quedó To Do y SIN promover (claimed-complete-without-evidence-stays-open ✓ — lo declarado sin evidencia nunca cierra); AC4: tarea añadida tras terminar el ciclo existe como To Do y es candidata de reactivación (idle-reactivates-on-new-work ✓); la otra mitad del AC4 (Stop impide reactivación por cambios del tracker) queda cubierta por los contratos ya verificados del coordinador/run (receive rechaza eventos tras stop, TASK-5/TASK-4). all_passed TRUE ×2 ejecuciones (400s y ~530s). DoD: E2E en Claude Code real con IDs observados (commits, tickets, estados en los detalles) y logs redactados (credenciales solo por env, sin tokens en ninguna salida). Gates: fmt/clippy -D warnings/test workspace 143 passed 0 failed.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-11 completa: el bucle autónomo integra la unidad finita de trabajo con el ciclo infinito por UN camino de producción (ACP + OpenSandbox). Sobre un fixture real de 5 tickets encadenados, el bucle completó las 5 SIN intervención ni repetición: cada tarea arrancó conversación nueva (sandbox + PTY nueva del agente real claude-code-acp), trabajó sobre el estado PROMOVIDO de la anterior, su candidato se verificó en entorno limpio contra el criterio del ticket, se promovió por CAS con ledger write-ahead y el ticket se cerró por el CLI oficial con relectura real. Un agente que declara complete sin evidencia NO cierra su ticket (ejercitado con agente real: tarea trampa quedó abierta y sin promover). El trabajo añadido tras Idle es candidata de reactivación; Stop impide la reactivación por cambios del tracker (contratos del coordinador/run ya verificados). Verificación: all_passed ×2 ejecuciones (5/5 ciclos + fraude bloqueado), IDs observados, logs redactados; gates workspace 143 tests en verde.
<!-- SECTION:FINAL_SUMMARY:END -->
