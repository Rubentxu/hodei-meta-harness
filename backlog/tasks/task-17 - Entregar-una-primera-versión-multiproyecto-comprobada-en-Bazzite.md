---
id: TASK-17
title: Entregar una primera versión multiproyecto comprobada en Bazzite
status: Done
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-18T05:27:00Z'
labels:
  - gc-mvp
  - gc-17
  - hodei-mh
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
- [x] #1 Cuatro proyectos pueden activarse y seleccionar tareas sin duplicados y con concurrencia limitada por admisión real.
- [x] #2 Los escenarios de renovación Stop Idle nueva tarea y recuperación del supervisor y sandbox se demuestran en ejecuciones reales.
- [x] #3 Memoria y presión del host se registran durante builds; si el equipo de referencia no está disponible no se certifica Bazzite y se documenta qué faltó.
- [x] #4 El paquete instalable contiene binario perfil y guía de uso sin dependencias de Continuum Herdr ni plugins nativos no demostrados.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Ejecutar cargo test/clippy y E2E de instalación con logs reales; no inventar métricas.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
EVIDENCIA (multiproject-probe, 11 pasos ALL=True en ~25s, dos ejecuciones consecutivas estables; builds Rust REALES): AC1 CUATRO proyectos (alpha/beta/gamma/delta) con git + Backlog.md real (2 tickets c/u) y supervisor PROPIO por proyecto (4 sockets, los 4 responden sup-status); los 4 primeros tickets se reclaman SIN DUPLICADOS (coordinator: segundo claimant sobre alpha-task-1 DENIED); concurrencia limitada por ADMISIÓN REAL con un ÚNICO presupuesto global (2 slots): [Admitted, Admitted, Waitlisted{CpuExhausted}, Waitlisted{CpuExhausted}] — gamma y delta waitlisted ANTES de cualquier create. AC2 escenarios en ejecuciones reales: Stop (sup-action start->stop, applied y confirmado), idle-release tras candidato durable confirmado (DELETE->GET 404), NUEVA tarea en el tracker (backlog task create) -> reclamada -> re-admitida -> build del proyecto actualizado en sandbox NUEVO (build-ok-alpha-newtask); liberar beta ADMITIÓ a gamma desde la waitlist; PÉRDIDA REAL de sandbox (DELETE->404 sin snapshot) recuperada desde candidato durable con sesión nueva (build completado); CRASH del supervisor (kill) -> clientes lo reportan honesto -> relanzamiento sobre el MISMO socket -> descubrimiento restaurado; el último waitlisted (delta) entró al liberarse gamma y construyó de verdad. AC3 memoria y presión del host REGISTRADAS durante los builds (muestreador de /proc/meminfo + /proc/pressure/memory: samples=19, min_mem_available=6863848 KiB, psi_memory_some_max=0.68); Bazzite NO certificado honestamente: OS id='ubuntu' != bazzite -> 'reference team/hardware unavailable: NOT certified', lo que faltó queda documentado (métricas son de ESTE host, ninguna inventada). AC4 paquete instalable real: subcomando package -> binarios release + docs/profiles.md + install.sh + INSTALL.md -> tar.gz; el probe EXTRACTA el bundle y el binario empaquetado ejecuta doctor contra la API viva (PROCEED); manifiesto contiene SOLO bin/, docs/, install.sh, INSTALL.md (sin extras no demostrados; yolo_fallback=false; sin Continuum/Herdr/plugins). DoD: cargo test (193 passed 0 failed), clippy workspace -D warnings limpio, fmt limpio, E2E de instalación con logs reales (2 ejecuciones estables) y SHA/comandos registrados en los records JSON. Bug del probe corregido (tupla (nombre,id) desestructurada invertida: se pasaba el nombre del proyecto donde iba el UUID; detectado por el 404 con el mensaje del server). Gates: fmt limpio, clippy limpio, 193 tests 0 failed.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-17 completa: primera versión multiproyecto comprobada de hodei-meta-harness. multiproject-probe (11 pasos, ALL=True, dos ejecuciones estables): CUATRO proyectos reales (git + Backlog.md + supervisor propio cada uno),;tickets activados sin duplicados (claim duplicado DENIED), concurrencia limitada por admisión real con presupuesto global (2 admitidos, 2 waitlisted ANTES de crear sandbox); ciclo completo stop -> idle-release (tras candidato durable) -> tarea nueva -> re-admisión -> build real; pérdida real de sandbox recuperada desde candidato durable en sesión nueva; crash del supervisor con relanzamiento sobre el mismo socket; memoria y presión del host registradas durante los builds (/proc/meminfo + PSI) y Bazzite NO certificado de forma honesta (OS real != bazzite, lo que faltó documentado); paquete instalable real (binarios release + docs + install.sh) verificado E2E extrayéndolo y ejecutando el doctor con el binario empaquetado, sin dependencias no demostradas (yolo_fallback=false, sin Continuum/Herdr). Gates: fmt y clippy workspace -D warnings limpios, 193 tests workspace 0 failed.
<!-- SECTION:FINAL_SUMMARY:END -->
