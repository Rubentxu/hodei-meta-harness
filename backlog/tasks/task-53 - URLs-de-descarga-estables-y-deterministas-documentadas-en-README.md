---
id: TASK-53
title: URLs de descarga estables y deterministas documentadas en README
status: Done
assignee: []
created_date: '2026-11-20T19:36:00Z'
updated_date: '2026-11-20T19:44:00Z'
labels: []
dependencies: []
priority: high
type: docs
ordinal: 53000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Documentar el contrato de URLs estables del canal de distribución al estilo download.docker.com: los assets del release siguen el patron determinista hodei-mh-v{version}-{os}-{arch}.tar.gz (fuente: crates/hodei-mh/src/package.rs:348) y son descargables via https://github.com/Rubentxu/hodei-meta-harness/releases/download/v{version}/{asset} con SHA256SUMS por release y /latest/download/ como alias. Verificar con curl -sI real contra v0.1.1 publicado que las dos plantillas resuelven (evidencia en la tarea). Solo README.md (seccion de descarga/instalacion); no toca install.sh de raiz (otra tarea).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 La seccion documenta la plantilla versionada y el alias latest con un ejemplo real por asset
- [x] #2 curl -sI contra los assets de v0.1.1 devuelve 200 y se registra como evidencia
- [x] #3 Sin secretos ni URLs inventadas; solo el repo real
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 scripts/fast-check.sh --all pasa
- [x] #2 pipelinek validate pipeline.kts pasa
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T19:44:00Z

---

Evidencia (supervisor): rama @82c7fe0 (worktree aislado /tmp/wt53), solo README.md +40 líneas. AC1: plantilla versionada + alias latest con los 4 assets reales; AC2: HEADs reales pegados (4x200 + latest/SHA256SUMS 200 + latest/bundle-sin-version 404 documentado como inexistente, honesto); AC3: URLs derivadas de package.rs:348 y gh release view, sin invención. Gates worker y post-merge --no-ff: fast-check PASS, pipelinek validate SUCCESSFUL.

---
<!-- COMMENTS:END -->
