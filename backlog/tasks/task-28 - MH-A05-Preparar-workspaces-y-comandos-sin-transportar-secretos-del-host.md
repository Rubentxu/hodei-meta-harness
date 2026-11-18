---
id: TASK-28
title: '[MH-A05] Preparar workspaces y comandos sin transportar secretos del host'
status: To Do
assignee: []
created_date: '2026-11-18T11:50:00Z'
labels:
  - audit-v0.2
  - security
  - workspace
  - p0
dependencies: []
references:
  - crates/hodei-mh/src/profile_run.rs
  - crates/hodei-mh-workspace/src/git.rs
  - crates/hodei-mh-profiles/src/credentials.rs
  - crates/hodei-mh-supervisor/src/provenance.rs
priority: high
ordinal: 28000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0. profile_run.rs:106-121 interpola proxy en shell del host; :199-210 empaqueta todo, incluidas rutas ignoradas; :511-521 guarda comando y redacta solo tras truncar salida. Reusar workspace Git, exportación explícita de cambios autorizados, argv/URL tipada y redacción única basada en valores secretos conocidos y patrones. Usar temporales privados. TASK-2/8/16/19. No confiar en gitignore como detector universal; política de exportación explícita y secreto conocido excluido aunque esté versionado. No añadir un gestor de secretos paralelo.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 URL/proyecto con caracteres especiales no ejecuta comandos host inesperados; URL con credenciales no aparece en argv/logs.
- [ ] #2 El snapshot excluye secretos ignorados, .git y caches por defecto; cambios no versionados solo se exportan con política explícita.
- [ ] #3 Redacción antes de truncar/persistir protege tokens conocidos, URL y salida estructurada conservando estructura.
- [ ] #4 Un fixture local con secretos sintéticos comprueba el artefacto y logs sin proveedor LLM.
<!-- AC:END -->
