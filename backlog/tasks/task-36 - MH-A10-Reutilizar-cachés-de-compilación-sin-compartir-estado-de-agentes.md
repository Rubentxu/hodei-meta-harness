---
id: TASK-36
title: '[MH-A10] Reutilizar cachés de compilación sin compartir estado de agentes'
status: In Progress
assignee: []
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-20T08:01:00Z'
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
- [ ] #1 Dos builds consecutivos del mismo proyecto reutilizan dependencias OCI/toolchain o cache segura y muestran coste frío/caliente.
- [ ] #2 Proyecto distinto o toolchain/lockfile incompatible no reutiliza artefactos mutables incorrectos.
- [ ] #3 El verificador conserva checkout limpio y puede desactivar la cache; una cache dañada no produce PASS.
- [ ] #4 Quota y limpieza mantienen acotado uso de disco; benchmark representa build real, no echo.
<!-- AC:END -->
