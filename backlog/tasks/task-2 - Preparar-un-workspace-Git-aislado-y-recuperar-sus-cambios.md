---
id: TASK-2
title: Preparar un workspace Git aislado y recuperar sus cambios
status: To Do
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-17T14:41:00Z'
labels:
  - gc-mvp
  - gc-02
  - hodei-mh
dependencies: []
ordinal: 2000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Implementar WorkspaceRef y la entrega de una revisión base explícita a un clon privado. El tracker permanece en el host. Usar Git commit/bundle o patch con archivos nuevos correctamente incluidos; no montar el checkout original ni su .git en escritura. Mantener referencias de recuperación fuera del sandbox. Gestionar archivos no rastreados con política explícita.

Clave de incorporación: GC-02.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 El sandbox recibe exactamente la revisión base elegida; un checkout fuente sucio no es copiado ni alterado sin elección expresa.
- [ ] #2 Un commit candidato o bundle recuperable se exporta y se valida en el host antes de liberar el sandbox.
- [ ] #3 Una caída después de exportar conserva el candidato recuperable; una salida parcial no se anuncia como válida.
- [ ] #4 La ruta privada de gestión Backlog y credenciales no puede ser promovida por un cambio ordinario del agente.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Probar cambios nuevos borrados binarios y checkout sucio con repositorios Git temporales.
<!-- DOD:END -->
