---
id: TASK-4
title: Persistir objetivos y claims con un coordinador de escritor único
status: To Do
assignee: []
created_date: '2026-11-17T14:20:00Z'
labels:
  - goalcycle
  - gc-mvp
  - gc-04
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
- [ ] #1 Dos procesos clientes que solicitan el mismo ticket simultáneamente obtienen como máximo un claim activo del supervisor.
- [ ] #2 Eventos repetidos o tardíos con epoch anterior no generan nuevas operaciones ni modifican otro run.
- [ ] #3 Estado e intención de efecto sobreviven a un crash; resultado incierto se reconcilia antes de reemitir.
- [ ] #4 El actor continúa atendiendo Stop y consultas mientras un efecto externo está bloqueado.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Ejecutar pruebas de propiedades de reducer y fallos inducidos en transacciones.
<!-- DOD:END -->
