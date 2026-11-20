---
id: TASK-55
title: >-
  Preparar y publicar la release v0.1.2 con el instalador curl|sh y los bundles
  verificados
status: In Progress
assignee: []
created_date: '2026-11-20T20:44:00Z'
updated_date: '2026-11-20T20:44:00Z'
labels: []
dependencies: []
priority: high
type: task
ordinal: 55000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Bump 0.1.1->0.1.2, tag v0.1.2, build estático documentado, hodei-mh package, y la matriz de verificación de v0.1.1 replicada: fmt/clippy/cargo test workspace en el commit publicado, 3 rondas del carril de sondas contra infraestructura real con los binarios publicados, prueba FROM scratch del binario estático, consumer check del bundle descargado (digest, SHA256SUMS offline, instalación en prefijo, doctor gate PROCEED), y el extra nuevo: el install.sh de raíz (TASK-54) probado post-publicación descargando desde la URL real de la release. Publicar gh release v0.1.2 con assets (bundle, 2 binarios, SHA256SUMS) y notas que anuncien el canal curl|sh y las docs. Incluye el contenido real desde v0.1.1: encadenamiento de objetivos (TASK-34), sondas journal-driven (TASK-45/46/52), docs A1-A4+índice, instalador.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 El tag v0.1.2 señala un commit cuyo Cargo.toml reporta 0.1.2 y cuyo CI gate (workspace fast) queda verde sobre él
- [ ] #2 La matriz de verificación de v0.1.1 replicada al pie de la letra sobre los binarios del tag, con 3 rondas de sondas verdes y residuo cero
- [ ] #3 Consumer check post-publicación: bundle descargado de la URL real con digest casado e instalación verificada; y el install.sh curl|sh instala v0.1.2 desde /latest/download/ en un prefijo efímero
- [ ] #4 La release publicada contiene bundle tar.gz, los dos binarios sueltos y SHA256SUMS con el naming determinista documentado en TASK-53, y las notas anuncian el canal de instalación
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 scripts/fast-check.sh --all pasa
- [ ] #2 pipelinek validate pipeline.kts pasa
<!-- DOD:END -->
