---
id: TASK-30
title: '[MH-A07] Acotar IPC y salida conservando eventos estructurados'
status: To Do
assignee: []
created_date: '2026-11-18T11:50:00Z'
labels:
  - audit-v0.2
  - security
  - performance
  - p1
dependencies: []
references:
  - crates/hodei-mh-supervisor/src/server.rs
  - crates/hodei-mh-supervisor/src/engine.rs
  - crates/hodei-mh-supervisor/src/protocol.rs
  - crates/opensandbox/src/execd/oneshot.rs
priority: high
ordinal: 30000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P1. server.rs:68-90 elimina ruta tras comprobación no atómica; :95-129 conexiones/líneas sin límites. engine.rs:83-93 convierte JSON a String y drena Vec; :238-243 suma TTL sin check. oneshot acumula bytes sin cupo. Proteger socket con directorio privado/lock, límites de tamaño/tiempo y salida acotada; mantener gap explícito y payload tipado. TASK-15/19. Usar directorio runtime del usuario 0700, socket restrictivo y lock por identidad, no solo nombre de ruta. Autenticación por peer cuando sea aplicable. Versionar contrato si cambia formato. No cambiar a gRPC por defecto.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Dos supervisores concurrentes no pueden asumir la misma identidad; un archivo no socket nunca se elimina.
- [ ] #2 NDJSON sobredimensionado, cliente lento y salida excesiva quedan acotados; un cliente normal conserva servicio.
- [ ] #3 El evento conserva su estructura, época y cursor; pérdida de historial se anuncia al cliente.
- [ ] #4 Tests de socket y collector locales, sin contenedores; no ocultar truncamiento.
<!-- AC:END -->
