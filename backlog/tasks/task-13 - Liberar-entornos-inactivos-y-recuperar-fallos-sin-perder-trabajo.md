---
id: TASK-13
title: Liberar entornos inactivos y recuperar fallos sin perder trabajo
status: To Do
assignee: []
created_date: '2026-11-17T14:21:00Z'
labels:
  - goalcycle
  - gc-mvp
  - gc-13
dependencies:
  - TASK-7
  - TASK-10
  - TASK-12
ordinal: 13000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Implementar lifecycle OpenSandbox: create/inspect/renew/delete y endpoint resolution, estado, TTL, ownership tags, generación y lease. Zero-token idle: detener agente y liberar entorno cuando trabajo recuperable. Recuperación de proceso/sandbox y retry por causa con jitter; no asumir snapshots tras OOM. Proteger estado y archivos externos.

Clave de incorporación: GC-13.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Desconexión/timeout tras create reconcilia el efecto por metadata/estado antes de intentar otro; huérfanos solo se limpian si pertenecen a GoalCycle.
- [ ] #2 Idle libera el entorno gestionado después de confirmar candidato/checkpoint y sigue observando el tracker sin inferencia.
- [ ] #3 Pérdida del sandbox se recupera desde candidato durable y sesión nueva; datos nunca exportados se marcan como posible pérdida.
- [ ] #4 Dos intentos improductivos o unos 20–30 minutos observados activan investigación y cambio de estrategia; antes de una hora se decide otro slice o yield sin cierre ficticio.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Inyectar pérdida de respuesta creación OOM simulado caída del supervisor y expiración real de un sandbox.
<!-- DOD:END -->
