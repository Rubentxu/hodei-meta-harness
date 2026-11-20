---
id: TASK-60
title: >-
  UAT live real: objetivo trivial ejecutado por Qoder dentro del sandbox y
  promovido
status: To Do
assignee: []
created_date: '2026-11-20T21:46:00Z'
updated_date: '2026-11-20T21:46:00Z'
labels: []
dependencies:
  - TASK-58
  - TASK-59
priority: high
type: task
ordinal: 60000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Cierre del circuito, dependiente de TASK-58 y TASK-59: una sola corrida end-to-end honesta. Montar tablero efímero (Backlog.md de prueba en tracker-dir temporal) con UNA tarea trivial verificable (p. ej. crear un fichero y compilar); levantar lifecycle-server + proxy con el dominio Qoder permitido; exportar el PAT SOLO por entorno (nunca imprimir); hodei-mh supervisor --live --agent qoder con managed branch local. Oráculo: journal sup-events --jsonl muestra el ciclo completo aplicado/confirmado/cerrado; el candidato verificado se promueve a la managed branch con el resultado real; residuo cero (sandboxes/containers/networks). Si el proveedor no responde o el gate refusa, la UAT se registra como tal con la causa exacta, no se maquilla. Evidencia pegada en la tarea; después del verde, valorar documentar la vía Qoder en overview/quickstart como tarea menor si procede.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 La corrida completa deja en el journal la secuencia select/work/verify/promote confirmada para la tarea trivial
- [ ] #2 La managed branch contiene el cambio promovido verificado en sandbox desechable
- [ ] #3 Cero residuo de infraestructura al terminar y cero credenciales impresas en la evidencia
- [ ] #4 El resultado se reporta tal cual: verde documentado o rojo con causa clasificada
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 scripts/probes.sh --preflight pasa
- [ ] #2 la evidencia de la corrida está pegada en la tarea
<!-- DOD:END -->
