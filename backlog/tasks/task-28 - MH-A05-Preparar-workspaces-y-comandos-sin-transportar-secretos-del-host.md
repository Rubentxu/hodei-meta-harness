---
id: TASK-28
title: '[MH-A05] Preparar workspaces y comandos sin transportar secretos del host'
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-19T00:05:00Z'
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

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. AC-2 Staging del workspace (workspace::plan = dueño del denylist): nueva selección pura de ficheros a exportar select_files_for_export(tracked, untracked, policy, denylist) -> niega .git/caches/secreto_conocido_aunque_versionado, y solo añade no-versionados con politica explícita. profile_run reemplaza tar_project(.) por tar de la lista explícita: git ls-files (tracked) + git ls-files --others --exclude-standard (solo con --export-untracked); para dir no-git, walk excluyendo .git/denylist/caches.
2. AC-3 Redaccion ANTES de truncar/persistir (profiles::credentials = dueño de redaccion): redactar el comando ejecutado y la salida COMPLETA antes de tomar el tail; el chequeo de leaks corre sobre los strings crudos aparte. Cerrar hueco de URL con token como username sin colon (https://ghp_x@host) en redact_url_credentials.
3. AC-1 Prueba enfocada: proyecto con metacarácteres y URL con credenciales -> argv/log no ejecutan comando host ni muestran credencial (proxy_address canónico ya lo garantiza; añadir regresión).
4. AC-4 Fixture local sin LLM con secretos sintéticos (.env/*.key versionados + secreto en salida de build) -> artefacto exportado y logs sin proveedor; comprobar que el snapshot y el registro no los contienen.
5. Puerta local: cargo fmt, clippy -D warnings, fast-check --packages hodei-mh,hodei-mh-workspace,hodei-mh-profiles, tests nuevos.

REVISION GOLDISH (2 revisores): fix MAJOR -> (a) el chequeo de leaks debe leer de los strings CRUDOS guardados aparte (hoy se deriva del record; redactar antes cegaria el detector); (b) git ls-files SIEMPRE -z y via git.rs (unico dueno CLI), no split por lineas; (c) caches/.git excluidos via EXPORT_DENYLIST en workspace::plan (no ensanar DEFAULT_DENYLIST de promote; anadir regla prefijo .env* a matches_rule); (d) temporales privados 0700 (tgz y export dir, nombres predecibles hoy); (e) lista vacia vs todo-denegado: mensajes y codigo distintos, tar con --null -T - via stdin (ARG_MAX) y --; (f) record: project_export {policy,source,files_exported,denied[]} tambien en candidate.json; (g) redact_url_credentials: delimitar authority (sin '@' infinito ni break que aborta URLs posteriores), redactar userinfo sin dos-puntos; y redaction de prefijos de token (ghp_, sk-ant-...) para cerrar asimetria deteccion/redaccion. No unificar provenance/rollover redactors (fuera de alcance; nota de drift para TASK-34). Mismo hazard tar en 5 sondas hermanas: fuera de alcance, nota en handoff.

GOLDFISH C (hallazgos aceptados, entran en la implementacion): (1) BLOCKER git ls-files camina al repo PADRE si project_dir es subdirectorio de otro repo -> exigir rev-parse --show-toplevel == project_dir; si no, tree-walk (fuente honesta en record). (2) gitlinks/submodulos y paths del index sin worktree: filtrar cada entrada por symlink_metadata (solo file|symlink; si no, denegada rule not-a-regular-file). (3) AKIA/ASIA detectados pero NO redactados + user:pass@ sin scheme (scp-style git): anadirredaccion de AWS key ids y userinfo con dos-puntos fuera de ://; reescribir redact_url_credentials con authority DELIMITADA (sin '@' infinito, sin break que aborta URLs posteriores) y userinfo sin colon redactada (ghp_x@host). (4) temp privado: dir 0700 unique + tgz pre-creado O_EXCL 0600 (tar truncates, no sigue symlinks), cleanup incondicional; seleccion/packing ANTES de crear sandbox (fallo = nada que limpiar). (5) lista vacia con --null -T produce archive VALIDO vacio (verificado): refusar export vacio explicitamente, distinto de todo-denegado. (6) nombres escritos con prefijo ./ para names arrancon '-' (tar no los trague como opciones). (7) --image con userinfo (no digest) rechazado en spec_for; doctor valida proxy. Token-in-command en main.rs/probes hermanas = follow-up TASK-34, nota handoff.
<!-- SECTION:PLAN:END -->
