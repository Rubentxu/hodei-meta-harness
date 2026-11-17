---
id: TASK-10
title: Promover resultados Git y cerrar el ticket sin doble verdad
status: To Do
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-17T14:41:00Z'
labels:
  - gc-mvp
  - gc-10
  - hodei-mh
dependencies:
  - TASK-4
  - TASK-9
ordinal: 10000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
WorkWriter host-side mediante CLI oficial; no editar Markdown directamente. Promover a rama local gestionada por proyecto por CAS de ref base y verificar integración. Conservar checkout usuario intacto. Releer ticket antes de cerrar; conservar notas/AC previos. Separar promoción de cierre con outbox y reconciliación sin duplicación. No push/merge/deploy sin grant específico.

Clave de incorporación: GC-10.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Un candidato validado se integra a la rama gestionada y solo entonces se actualizan AC DoD y final-summary mediante CLI; Done usa estado configurado.
- [ ] #2 Un cambio de rama destino o ticket desde la verificación exige revalidar; no pierde cambios humanos ni pisa notas.
- [ ] #3 Crash tras promover antes de actualizar tracker recupera el mismo commit y no crea otro ni declara Done sin evidencia.
- [ ] #4 Error de tracker conserva trabajo técnico y actualizaciones pendientes; no se modifica directamente el Markdown ni se archiva o borra automáticamente.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Prueba de caída entre Git y CLI con relectura real del ticket y branch.
<!-- DOD:END -->
