---
id: TASK-36
title: '[MH-A10] Reutilizar cachés de compilación sin compartir estado de agentes'
status: Done
assignee: []
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-20T08:15:00Z'
labels:
  - audit-v0.2
  - performance
  - profiles
  - p2
dependencies:
  - TASK-25
  - TASK-28
references:
  - crates/hodei-mh-profiles/src/build_plan.rs
  - crates/hodei-mh/src/profile_run.rs
  - crates/lifecycle-server/src/docker.rs
priority: low
ordinal: 36000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P2. build_plan.rs:59-85 pone cache dentro de sandbox efímero; profile_run crea y elimina contenedor, no hay volumen persistente. Separar cache verificable de workspace escribible; usar caché por proyecto/toolchain/digest/lockfile, opt-in, cuotas y GC. Validar mejora antes de diseñar almacenamiento complejo. TASK-16. Preferir capas OCI inmutables y caché de descarga antes de compartir outputs de compilación; nunca confiar en una cache escribible por candidatos no confiables para validar promoción. Si no hay mejora, dejar opt-in apagado.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Dos builds consecutivos del mismo proyecto reutilizan dependencias OCI/toolchain o cache segura y muestran coste frío/caliente.
- [x] #2 Proyecto distinto o toolchain/lockfile incompatible no reutiliza artefactos mutables incorrectos.
- [x] #3 El verificador conserva checkout limpio y puede desactivar la cache; una cache dañada no produce PASS.
- [x] #4 Quota y limpieza mantienen acotado uso de disco; benchmark representa build real, no echo.
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Aceptada por el supervisor. PR #45 squash en main (be7d880, mismo árbol que e1cdd6b; el squash de GitHub inyectó Co-authored-by y el commit se rehízo con mensaje limpio + force-with-lease). Evidencia en /tmp/t36-bench: frío 10.18 s vs caliente 8.00 s (caché de descargas, 846 KB); --cache-target frío 8.92 s vs caliente 0.95 s (12.58 MB); project-beta obtiene clave distinta y restore cold; archivo corrupto -> restore rejected, descartado y rebuild frío; --cache --no-cache desactiva restore y persist; q-beta evicta la entrada de alpha con cuota de 13 MiB y un registro en el manifiesto. AC3 estructural: hodei-mh-verify y hodei-mh-promote no referencian el store y CacheUse::Verification siempre desactiva la caché (test). CI workspace gate SUCCESS (run 44700679678). Desviación ambiental: servidor dedicado en 8083 y proxy propio porque el 8081 previo apuntaba a un contenedor sin listener.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Caché de build opt-in, con clave por proyecto/imagen/toolchain/lockfile/scope, integridad verificada, cuota y GC, y verificador que nunca la consume.
<!-- SECTION:FINAL_SUMMARY:END -->
