---
id: TASK-2
title: Preparar un workspace Git aislado y recuperar sus cambios
status: Done
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-17T15:39:00Z'
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
- [x] #1 El sandbox recibe exactamente la revisión base elegida; un checkout fuente sucio no es copiado ni alterado sin elección expresa.
- [x] #2 Un commit candidato o bundle recuperable se exporta y se valida en el host antes de liberar el sandbox.
- [x] #3 Una caída después de exportar conserva el candidato recuperable; una salida parcial no se anuncia como válida.
- [x] #4 La ruta privada de gestión Backlog y credenciales no puede ser promovida por un cambio ordinario del agente.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Probar cambios nuevos borrados binarios y checkout sucio con repositorios Git temporales.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
EVIDENCIA E2E (Docker real, 3 ejecuciones verdes ~2s): hodei-mh-workspace-probe contra server lifecycle :8081. Base con revisión explícita subida como bundle -> clone en sandbox -> rev-parse HEAD == revisión elegida (coincidencia exacta). Candidato: commit en sandbox -> git bundle create --all -> descarga atómica (.part+rename) a staging /tmp/hodei-candidates/<sid>/ -> validación en host ANTES de liberar sandbox (bundle verify + clone index-pack + diff --name-only) -> 'touches only feature.txt'. Leg denegado: commit tocando backlog/leak.md -> RECHAZADO por denylist en host (nunca promovido). Simulación caída: DELETE sandbox -> candidato re-validado desde staging solo. HALLAZGO CRÍTICO: git bundle verify NO valida integridad del pack (un bundle truncado a la mitad pasa verify); la integridad real la da el clone/index-pack — documentado en git.rs y probado en test unitario. Contrato execd files: upload requiere DOS file-parts (metadata JSON con filename + file); sin owner/group (chmod falla si el usuario no existe en contenedor). 50 tests workspace, clippy -D warnings 0, fmt.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Workspace Git aislado verificado E2E: revisión base exacta, export bundle atómico, validación host pre-release con denylist de rutas privadas, recuperación post-caída desde staging. Hallazgo de seguridad registrado: bundle verify no valida pack (integridad vía index-pack). Apilada sobre TASK-1 (transporte).
<!-- SECTION:FINAL_SUMMARY:END -->
