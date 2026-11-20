---
id: TASK-4
title: Persistir objetivos y claims con un coordinador de escritor único
status: Done
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-20T20:24:00Z'
labels:
  - gc-mvp
  - gc-04
  - hodei-mh
dependencies: []
ordinal: 4000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Un actor Tokio acotado y SQLite privada XDG. Goal desired state separado de Run observed state. Newtypes para identidad, epoch, autorización y operaciones. Reducer puro, efectos fuera del actor, SQL transaccional y outbox pequeña. Claim único por WorkRef global local; un solo controlador por sesión.

Clave de incorporación: GC-04.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Dos procesos clientes que solicitan el mismo ticket simultáneamente obtienen como máximo un claim activo del supervisor.
- [x] #2 Eventos repetidos o tardíos con epoch anterior no generan nuevas operaciones ni modifican otro run.
- [x] #3 Estado e intención de efecto sobreviven a un crash; resultado incierto se reconcilia antes de reemitir.
- [x] #4 El actor continúa atendiendo Stop y consultas mientras un efecto externo está bloqueado.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Ejecutar pruebas de propiedades de reducer y fallos inducidos en transacciones.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
EVIDENCIA (tests de integración con SQLite real y actor real, sin simulaciones): 9 tests coordinador. AC1: carrera de dos claims concurrentes sobre T9 vía actor con mailbox serializado -> exactamente 1 Granted, otro Denied{holder}. AC2: apply_event descarta epoch < actual (inerte, sin ops nuevas, sin tocar otros runs). AC3: roundtrip crash real (store dropped y reabierto) restaura claims+epochs+efectos idénticos; efectos Uncertain listados por uncertain_effects() y can_reemit=false hasta reconcile(applied|not_applied); write-ahead de intents antes de ejecutar. AC4: actor con tokio::select! sobre mailbox + canal de completaciones — Stop respondido en <2s mientras un efecto duerme 120s; outcome timeout/panic/drop -> Uncertain. Durabilidad: save en transacción con rollback de memoria si falla el save (memoria = disco). 73 tests workspace total, clippy -D warnings 0, fmt.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Coordinador durable de escritor único: claims excluyentes verificados por carrera concurrente, idempotencia por epoch, persistencia SQLite con intents write-ahead y reconciliación pre-reemisión, actor select! que atiende Stop y consultas durante efectos bloqueados.
<!-- SECTION:FINAL_SUMMARY:END -->
