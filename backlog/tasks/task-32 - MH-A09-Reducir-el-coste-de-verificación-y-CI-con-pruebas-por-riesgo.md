---
id: TASK-32
title: '[MH-A09] Reducir el coste de verificación y CI con pruebas por riesgo'
status: To Do
assignee: []
created_date: '2026-11-18T11:50:00Z'
labels:
  - audit-v0.2
  - testing
  - delivery
  - p1
dependencies: []
references:
  - Cargo.toml
  - crates/hodei-mh/src/main.rs
  - crates/hodei-mh/src/multiproject_probe.rs
  - crates/hodei-mh-supervisor/tests/socket_integration.rs
priority: medium
ordinal: 32000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P1 habilitador · F10/F13/F16 · TASK-19/20/21. 17 archivos probe/debug suman 248211 bytes de 803771 bytes Rust; no hay workflows en main auditado. Medir compilación/test por lane antes de añadir paralelismo. Establecer fast local, contracts Git/SQLite/IPC, smoke Docker y UAT ACP. Usar nextest solo si mejora con medición, limitar concurrencia según Bazzite. No usar un selector por paths que permita saltarse dependientes ni omitir fallos. Primera entrega: baseline y comando rápido útil, no esperar a todos los refactors.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Baseline frío y caliente con mismos commit/toolchain/host y varias repeticiones; registrar top 5 casos costosos, tiempo pared, CPU y llamadas/sandboxes; NOT_RUN si no disponible.
- [ ] #2 Comando rápido local cubre crate afectado y dependientes relevantes; CI tiene fmt/clippy/test workspace y doctests; fallos no se convierten en PASS.
- [ ] #3 Probes caros quedan en lane explícita y su reemplazo exige el mismo invariante en ruta productiva; usar fake ACP/HTTP para fallos, sin tokens ni sleeps arbitrarios.
- [ ] #4 Comparar coste caliente equivalente; objetivo orientativo >=30%, no criterio para falsear cierre. Conservar smoke Docker/ACP en cambios pertinentes y release.
<!-- AC:END -->
