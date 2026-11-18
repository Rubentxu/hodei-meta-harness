---
id: TASK-24
title: '[MH-A01] Vincular la verificación al candidato Git realmente ejecutado'
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-18T11:49:00Z'
updated_date: '2026-11-18T14:07:00Z'
labels:
  - audit-v0.2
  - security
  - verification
  - p0
dependencies: []
references:
  - crates/hodei-mh-verify/src/runner.rs
  - crates/hodei-mh-verify/src/evidence.rs
  - crates/hodei-mh-promote/src/git.rs
priority: high
ordinal: 24000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0 · F02 · TASK-9/10. runner.rs:55-81,169-191 recibe revision/digest y ejecuta main sin contrastar HEAD. evidence.rs:39-67 permite construcción y binding públicos; promote/git.rs:106-172 hace CAS sobre la base, no sobre la evidencia. Separar digest de transporte, GitRevision y política/entorno observado; construir evidencia únicamente desde mediciones. Un candidato verificado no es una prueba de que el código sea seguro: los checks autorizados deben ser independientes del agente. No cambiar algoritmo de Git CAS.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Bundle válido con SHA declarado distinto o digest alterado se rechaza antes de los checks; se registra HEAD realmente comprobado.
- [ ] #2 El conjunto de checks requerido, sus comandos autorizados y el ID de imagen observado están ligados al SHA; cambiar cualquiera invalida la evidencia.
- [ ] #3 La promoción exige evidencia del mismo commit y verifica el bundle en su frontera; se rechazan evidencia ausente, obsoleta o de otro trabajo.
- [ ] #4 Prueba negativa con bundle válido de otro SHA y política diferente, más un caso positivo usando la ruta real sin LLM.
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Separar cuatro identidades en hodei-mh-verify: GitRevision (commit), GitTree (contenido), TransportDigest (bytes del transporte) y EvidenceBinding (política y entorno observados). El digest de archivos deja de ser la identidad del candidato: git es la autoridad del contenido.
2. runner.rs: comprobar el transporte ANTES de crear el sandbox; tras el clone observar en el sandbox `git rev-parse HEAD` y `git rev-parse HEAD^{tree}` y compararlos con lo declarado. Un desajuste aborta antes de ejecutar cualquier check. La evidencia se liga al id de imagen que resolvió el runtime, no al tag.
3. evidence.rs: la construcción de evidencia es privada del crate (bind es pub(crate)); el binding lleva revisión, árbol, digest de transporte, checks requeridos con su comando autorizado e id de imagen. Cambiar cualquiera de ellos invalida las entradas.
4. promote: nueva puerta require_evidence que exige evidencia del mismo work_ref, del mismo commit, del mismo bundle y con los checks satisfechos; promote_verified la ejecuta antes del CAS de git, cuyo algoritmo no cambia.
5. lifecycle-server: la creación de sandbox devuelve el id de imagen resuelto (imageId, camelCase del contrato) para que el consumidor sepa en qué entorno corrieron los checks.
6. Pruebas: unitarias del binding y de la puerta de promoción; probe real verify-probe (Docker + lifecycle server) con caso negativo de bundle de otro SHA y de otra política, y caso positivo por la ruta real sin LLM. El probe entra en el carril caro scripts/probes.sh.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementacion (TASK-24 / MH-A01)

QUE CAMBIO

1. Cuatro identidades separadas en hodei-mh-verify: `GitRevision` (el commit), `GitTree` (el contenido tal como git lo nombra), `TransportDigest` (los bytes que transportan al candidato) y `EvidenceBinding` (la politica y el entorno observados). El "digest de candidato" sobre un conjunto de archivos desaparece: la autoridad del contenido es git. `digest.rs` conserva solo el digest de bytes, que sigue usandose para el export durable del release.

2. `runner.rs` (crates/hodei-mh-verify/src/runner.rs): el transporte se comprueba ANTES de crear el sandbox (un bundle que no es el declarado nunca llega al sandbox). Tras el clone se observan en el sandbox `git rev-parse HEAD` y `git rev-parse HEAD^{tree}` y se comparan con lo declarado: un desajuste aborta ANTES de ejecutar cualquier check. La evidencia registra lo OBSERVADO, no lo declarado: el HEAD y el arbol que git reporto en el checkout que corrio, y los bytes que realmente viajaron.

3. `evidence.rs` (crates/hodei-mh-verify/src/evidence.rs): la construccion de evidencia es privada del crate (`bind` es `pub(crate)`), asi que solo el runner que observa puede acuñarla. El binding lleva revision, arbol, digest de transporte, checks requeridos con su comando autorizado e id de imagen. Cada entrada lleva ademas el contenido, el transporte y la imagen en que se produjo; cambiar cualquiera de ellos invalida la evidencia (`candidate_changed`, `binding_changed`).

4. `promote` (crates/hodei-mh-promote/src/evidence.rs): nueva puerta `require_evidence` que exige evidencia de la MISMA tarea, del MISMO commit y del MISMO bundle verificado, con los checks satisfechos y un documento coherente con su propio binding (mismo contenido, mismo transporte, misma imagen). `promote_verified` la ejecuta ANTES del CAS de git, cuyo algoritmo no cambia.

5. `lifecycle-server`: la creacion de sandbox devuelve el id de imagen que resolvio el runtime (`imageId`, camelCase del contrato) para que el consumidor sepa en que entorno corrieron sus checks.

RUTA PRODUCTIVA

El bucle autonomo (agente -> verificar -> promover -> cerrar ticket) ahora promueve con `promote_verified`: la invariante vive en el producto y no solo en la sonda. Antes promovia con el CAS a secas, sin exigir la evidencia.

DEFECTOS ENCONTRADOS Y CORREGIDOS EN EL CAMINO

- El cliente leia `image_id` pero el contrato del control plane es camelCase (`imageId`): la verificacion fallaba siempre con "the runtime did not report the image it resolved".
- Cuando el sandbox se creaba pero la respuesta no se podia leer, el sandbox NO se borraba: dos sandboxes huerfanos quedaron en el host. Ahora todo lo que puede fallar despues de crear el sandbox corre dentro del bloque que garantiza el borrado.
- La observacion del HEAD en el bucle autonomo se hacia DESPUES de borrar el sandbox: el execd ya no existia y la verificacion no tenia identidad ("execd did not accept a session within 60s"). Ahora se observa mientras el sandbox esta vivo.

VERIFICACION (medida, no declarada)

- Gate local: `scripts/fast-check.sh` PASS.
- Gate de workspace: 291 tests verdes, `cargo fmt --all --check` y `cargo clippy --workspace --all-targets --locked -- -D warnings` limpios.
- Carril caro (`scripts/probes.sh --rounds 1`, que ahora incluye el `verify-probe`): 5/5 sondas PASS contra Docker y el lifecycle server reales (release, supervisor, profile, multiproject, verify) y 0 residuos (0 sandboxes, 0 contenedores).
- `verify-probe` (15/15 checks): un candidato que declara otro commit se rechaza en 1.4 s SIN ejecutar el check que habria tardado 60 s; la evidencia con un check fallido NO promueve; un bundle valido de otro SHA no promueve ("the evidence belongs to another commit"); la evidencia de otra politica no promueve ("the evidence is stale: the check pass-check is no longer required"); y el bundle verificado SI se promueve por el CAS.
- `autonomous-loop` (11/11 steps, 349 s, 0 residuos): las 5 tareas dependientes se verifican, se promueven por la puerta de evidencia y se cierran por la CLI oficial, en orden y una sola vez; el ticket de fraude (el agente declara completo sin producir nada) sigue abierto y sin promover.

ESTADO DEL CARRIL DE CI (medido, no supuesto)

El job `integration` de CI esta ROJO en `main` desde su primera ejecucion (merge del PR #32, run 44308881486): release-probe, profile-probe y multiproject-probe fallan porque el runner NO tiene el CLI oficial de Backlog.md ni el proxy de egreso restringido que los perfiles exigen. Causa VERIFICADA en local (sin el CLI en el PATH, la release-probe falla con `[fixture error: spawn No such file or directory (os error 2)]`). Registrada como TASK-37 con sus criterios. El `verify-probe` que esta tarea anade al carril NO necesita ninguno de los dos prerrequisitos (git, shell y la imagen que descarga el host), asi que su resultado en CI no depende de ellos.

LIMITES

- La puerta exige que el documento de evidencia sea coherente consigo mismo y con el candidato, y que la evidencia sea del mismo commit y del mismo bundle verificado. La AUTENTICIDAD del documento (que nadie lo falsee) es responsabilidad del almacen de evidencia, no de esta tarea.
- `autonomous-loop` necesita un agente real dentro del sandbox (credenciales solo por el entorno del proceso): no pertenece al carril de CI, que corre sondas sin agente.
- El camino feliz y los negativos de la promocion los cubren `verify-probe` y `autonomous-loop`; el fallo del CAS (base movida) lo mantiene `promote-probe`, cuyo sujeto es el algoritmo de git, que esta tarea no cambia.
<!-- SECTION:NOTES:END -->
