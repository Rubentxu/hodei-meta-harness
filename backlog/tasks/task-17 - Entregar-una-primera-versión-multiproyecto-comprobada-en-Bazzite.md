---
id: TASK-17
title: Entregar una primera versión multiproyecto comprobada en Bazzite
status: To Do
assignee: []
created_date: '2026-11-17T14:21:00Z'
labels:
  - goalcycle
  - gc-mvp
  - gc-17
dependencies:
  - TASK-11
  - TASK-13
  - TASK-14
  - TASK-15
  - TASK-16
ordinal: 17000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Integrar cuatro proyectos de prueba con compilaciones Rust/JVM y presupuesto global medido. Validar aprendizaje de Symphony, ACP y OpenSandbox en producto, no mediante certificado administrativo. Pruebas de contrato y crash recovery más documentación de usuario. No exigir Kind anidado ni Fast Sandbox para esta entrega.

Clave de incorporación: GC-17.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Cuatro proyectos pueden activarse y seleccionar tareas sin duplicados y con concurrencia limitada por admisión real.
- [ ] #2 Los escenarios de renovación Stop Idle nueva tarea y recuperación del supervisor y sandbox se demuestran en ejecuciones reales.
- [ ] #3 Memoria y presión del host se registran durante builds; si el equipo de referencia no está disponible no se certifica Bazzite y se documenta qué faltó.
- [ ] #4 El paquete instalable contiene binario perfil y guía de uso sin dependencias de Continuum Herdr ni plugins nativos no demostrados.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Ejecutar cargo test/clippy y E2E de instalación con logs reales; no inventar métricas.
<!-- DOD:END -->
