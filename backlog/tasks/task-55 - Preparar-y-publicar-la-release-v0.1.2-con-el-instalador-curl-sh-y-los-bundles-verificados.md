---
id: TASK-55
title: >-
  Preparar y publicar la release v0.1.2 con el instalador curl|sh y los bundles
  verificados
status: Done
assignee: []
created_date: '2026-11-20T20:44:00Z'
updated_date: '2026-11-20T21:11:00Z'
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
- [x] #1 El tag v0.1.2 señala un commit cuyo Cargo.toml reporta 0.1.2 y cuyo CI gate (workspace fast) queda verde sobre él
- [x] #2 La matriz de verificación de v0.1.1 replicada al pie de la letra sobre los binarios del tag, con 3 rondas de sondas verdes y residuo cero
- [x] #3 Consumer check post-publicación: bundle descargado de la URL real con digest casado e instalación verificada; y el install.sh curl|sh instala v0.1.2 desde /latest/download/ en un prefijo efímero
- [x] #4 La release publicada contiene bundle tar.gz, los dos binarios sueltos y SHA256SUMS con el naming determinista documentado en TASK-53, y las notas anuncian el canal de instalación
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 scripts/fast-check.sh --all pasa
- [x] #2 pipelinek validate pipeline.kts pasa
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T21:11:00Z

---

Evidencia (supervisor + worker w55-build-verify): Release publicada https://github.com/Rubentxu/hodei-meta-harness/releases/tag/v0.1.2 sobre tag v0.1.2 = commit 6b107f3. AC1: CI workspace gate=success sobre 6b107f3 (el bump inicial 343bf59 falló el gate --locked por Cargo.lock desincronizado; corregido y retagueado ANTES de publicar, sin consumidores del tag viejo). AC2: matriz v0.1.1 replicada sobre clon limpia del tag — fmt/clippy limpios, 755 tests 0 fallos, build estático documentado, package manifiesto con standalone=true, prueba FROM scratch responde 'hodei-mh 0.1.2', probes.sh --rounds 3: 18/18 PASS (release/supervisor/profile/multiproject/verify/trust), residuo 0 sandboxes/0 containers/0 networks, proxy propio retirado. Reproducibilidad demostrada: rebuild desde el nuevo tag dio digests byte-idénticos a los assets sellados (ef64cac6…, 6799ff53…). AC3 post-publicación REAL: bundle+binarios descargados de las URLs versionadas, SHA256SUMS de la release casa 3/3 (exit 0); 'curl -fsSL raw…/v0.1.2/install.sh | sh' sin VERSION instaló vía /latest/download/ en prefijo efímero, exit 0, /prefix/bin/hodei-mh --version = 'hodei-mh 0.1.2', docs+INSTALL.md en share/. AC4: los 4 assets publicados con naming determinista de TASK-53 y SHA256SUMS formato v0.1.1 (3 líneas); notas anuncian el canal curl|sh y las docs. Gates DoD: fast-check PASS, pipelinek validate SUCCESSFUL. Descubrimiento cosmético registrado (installer imprime 'latest' en vez de la versión resuelta). Run CI del probe lane sobre main aún en curso como evidencia adicional no requerida.

---
<!-- COMMENTS:END -->
