---
id: TASK-30
title: '[MH-A07] Acotar IPC y salida conservando eventos estructurados'
status: In Progress
assignee: []
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-19T02:53:00Z'
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

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Contrato del socket: directorio runtime privado (0700), socket 0600, dos bloqueos exclusivos (identidad del proyecto y ruta del socket) y negativa dura ante un fichero que no sea socket.
2. Límites del IPC: línea de petición (64 KiB), respuesta (1 MiB), página de eventos (256) con límite pedido por el cliente, cuerpo de evento (4 KiB), plazo de escritura (10 s), inactividad (5 min) y conexiones simultáneas (64).
3. Eventos con estructura: payload tipado y redactado en árbol, época por evento, cursor next_seq y aviso de pérdida de historial (oldest_seq/history_lost). Contrato versionado (v2) en cada respuesta.
4. Salida acotada: cupo de 1 MiB en el colector del execd con contabilidad separada de bytes recibidos, guardados, ausentes y descartados.
5. Verificación: tests de socket y de colector locales (sin contenedores) y gates local/workspace.
<!-- SECTION:PLAN:END -->
