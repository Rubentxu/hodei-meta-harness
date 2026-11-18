---
id: TASK-24
title: '[MH-A01] Vincular la verificación al candidato Git realmente ejecutado'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-18T11:49:00Z'
updated_date: '2026-11-18T15:33:00Z'
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
- [x] #1 Bundle válido con SHA declarado distinto o digest alterado se rechaza antes de los checks; se registra HEAD realmente comprobado.
- [x] #2 El conjunto de checks requerido, sus comandos autorizados y el ID de imagen observado están ligados al SHA; cambiar cualquiera invalida la evidencia.
- [x] #3 La promoción exige evidencia del mismo commit y verifica el bundle en su frontera; se rechazan evidencia ausente, obsoleta o de otro trabajo.
- [x] #4 Prueba negativa con bundle válido de otro SHA y política diferente, más un caso positivo usando la ruta real sin LLM.
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

2. `runner.rs` (crates/hodei-mh-verify/src/runner.rs): el transporte se comprueba ANTES de crear el sandbox (un bundle que no es el declarado nunca llega al sandbox). Tras el clone se observan en el sandbox `git rev-parse HEAD` y `git rev-parse HEAD^{tree}` y se comparan con lo declarado: un desajuste aborta ANTES de ejecutar cualquier check. La evidencia registra lo OBSERVADO, no lo declarado: el HEAD y el arbol que git reporto en el checkout que corrio, y los bytes que realmente viajaron. Cada resultado guarda ademas el COMANDO AUTORIZADO al que responde y la huella del ENTORNO (imagen + sandbox) en el que corrio.

3. `evidence.rs` (crates/hodei-mh-verify/src/evidence.rs): la construccion de evidencia es privada del crate (`bind` es `pub(crate)`), asi que solo el runner que observa puede acuñarla. El binding lleva revision, arbol, digest de transporte, checks requeridos con su comando autorizado, id de imagen y entorno. El binding es la UNICA respuesta de "de que habla esta evidencia": no queda una segunda copia de la revision ni del arbol que pueda divergir de la que se produjo. Cambiar cualquiera de los elementos del binding invalida las entradas afectadas (`candidate_changed`, `binding_changed`).

4. `promote` (crates/hodei-mh-promote/src/evidence.rs): nueva puerta `require_evidence` que exige evidencia de la MISMA tarea, del MISMO commit y del MISMO bundle verificado, con los checks satisfechos y un documento coherente con su propio binding (mismo contenido, mismo transporte, misma imagen, mismo entorno, mismo comando). Una evidencia que no exige NINGUN check se rechaza: una politica vacia es un agujero, no una prueba. `promote_verified` la ejecuta ANTES del CAS de git, cuyo algoritmo no cambia. El camino que promociona sin evidencia se llama ahora `promote_ff_only_unchecked` y lo declara en su documentacion.

5. `lifecycle-server`: la creacion de sandbox devuelve el id de la imagen que CORRIO (`imageId`, camelCase del contrato), leido del contenedor creado y no deducido del tag: el id de una imagen fijada por digest no tiene tag. La disponibilidad de la imagen vuelve a ser "esta o se descarga", sin rechazar imagenes que antes se aceptaban. El id es un requisito del control plane al que habla el runner, y su ausencia es un fallo deliberado: sin id el entorno no queda identificado.

RUTA PRODUCTIVA

El bucle autonomo (agente -> verificar -> promover -> cerrar ticket) ahora promueve con `promote_verified`: la invariante vive en el producto y no solo en la sonda. Antes promovia con el CAS a secas, sin exigir la evidencia.

DEFECTOS ENCONTRADOS Y CORREGIDOS EN EL CAMANO

- El cliente leia `image_id` pero el contrato del control plane es camelCase (`imageId`): la verificacion fallaba siempre con "the runtime did not report the image it resolved".
- Cuando el sandbox se creaba pero la respuesta no se podia leer, el sandbox NO se borraba: dos sandboxes huerfanos quedaron en el host. Ahora cada sandbox lleva un token unico en sus metadatos y, si la creacion falla, el runner localiza y borra el huerfano por ese token.
- La observacion del HEAD en el bucle autonomo se hacia DESPUES de borrar el sandbox: el execd ya no existia y la verificacion no tenia identidad ("execd did not accept a session within 60s"). Ahora se observa mientras el sandbox esta vivo.

REVISION INDEPENDIENTE: HALLAZGOS RESUELTOS

Dos revisores en contexto nuevo y de solo lectura (uno de riesgos, otro de fiabilidad) sobre la rama. Cada hallazgo se verifico contra el codigo antes de tocarlo; los que no se pudieron reproducir quedaron como FALTA_EVIDENCIA. Hallazgos CORREGIDOS:

1. La ligadura del comando comparaba el comando autorizado (desnudo) con la linea que realmente corrio (`cd /workspace/checkout && ...`): `binding_changed` invalidaba SIEMPRE todas las entradas y la ligadura del comando nunca se exigia. La fixture unitaria usaba el comando desnudo, que el runner nunca produce, y por eso lo ocultaba. Ahora `BoundResult` guarda el comando AUTORIZADO y todas las comparaciones (la del binding y la de la puerta de promocion) usan esa representacion.
2. La puerta de promocion no comparaba ni el comando ni el entorno, y aceptaba una evidencia sin checks exigidos. Ahora compara ambos y rechaza la politica vacia.
3. El id de imagen se resolvia por `repo_tags`: una imagen fijada por digest no tiene tag y el filtro de docker por `reference` no la encuentra, de modo que el ciclo de vida paso a RECHAZAR imagenes que antes aceptaba (regresion real). Verificado en el host: `docker images --filter reference=<digest>` devuelve vacio, mientras que `docker inspect <contenedor>.Image` si devuelve el id. Corregido como se describe arriba.
4. Los fallos de la respuesta de creacion (cuerpo ilegible, respuesta sin id) seguian FUERA del bloque que garantiza el borrado: el codigo afirmaba mas de lo que hacia. Corregido con el token por metadatos y con la busqueda del huerfano.
5. El aviso de borrado fallido nunca se emitia cuando la verificacion fallaba (el error se propagaba antes). Ahora se emite ANTES de propagar el error: un residuo en el host no puede esconderse detras del error.
6. La sonda habia perdido dos aserciones (`clean-verifier-fails-uncontaminated` y `absent-result-never-pass`): el carril dejaba de comprobar que el verificador limpio FALLA de verdad el comando que el entorno de desarrollo aprueba, y que un timeout es un registro AUSENTE. Restauradas.
7. La sonda no borraba el `base_bundle` en su primer camino de error. Corregido.
8. El camino de promocion sin evidencia se llamaba igual que el de la puerta. Ahora se llama `promote_ff_only_unchecked`.
9. Del cierre de la revision: dos observaciones menores se resolvieron DOCUMENTANDO lo que el codigo hace de verdad, sin cambiar el comportamiento. (a) El fallo de borrado del sandbox se REGISTRA (warning) y no se propaga al llamante: la evidencia sigue siendo valida porque los checks SI corrieron en ese sandbox, y el sandbox restante expira por su TTL. (b) El id de imagen es un REQUISITO del control plane al que habla el runner (una extension del contrato de creacion aguas arriba), documentado como tal: sin id el entorno no se puede identificar, y un tag no lo sustituye porque dos imagenes pueden responder al mismo tag.

VERIFICACION (medida, no declarada)

- Gate local: `scripts/fast-check.sh` PASS.
- Gate de workspace: 295 tests verdes, `cargo fmt --all --check` y `cargo clippy --workspace --all-targets --locked -- -D warnings` limpios.
- Carril caro (`scripts/probes.sh --rounds 1`): 5/5 sondas PASS contra Docker y el lifecycle server reales (release, supervisor, profile, multiproject, verify) y 0 residuos (0 sandboxes, 0 contenedores).
- `promote-probe` (9/9 checks): el CAS de git (base movida, cierre idempotente, relectura de la rama) sigue igual: la tarea no toca ese algoritmo.
- `verify-probe` (17/17 checks): un candidato que declara otro commit se rechaza en 1.5 s SIN ejecutar el check que habria tardado 60 s; el verificador limpio FALLA el comando que el entorno de desarrollo aprueba (contaminacion); un timeout produce un registro AUSENTE, nunca un PASS; la evidencia con un check fallido NO promueve; un bundle valido de otro SHA no promueve ("the evidence belongs to another commit"); la evidencia de otra politica no promueve; y el bundle verificado SI se promueve por el CAS.
- `autonomous-loop` (11/11 steps, 479 s, 0 residuos): las 5 tareas dependientes se verifican, se promueven por la puerta de evidencia y se cierran por la CLI oficial, en orden y una sola vez; el ticket de fraude (el agente declara completo sin producir nada) sigue abierto y sin promover.

ESTADO DEL CARRIL DE CI (medido, no supuesto)

El job `integration` de CI esta ROJO en `main` desde su primera ejecucion (merge del PR #32, run 44308881486): release-probe, profile-probe y multiproject-probe fallan porque el runner NO tiene el CLI oficial de Backlog.md ni el proxy de egreso restringido que los perfiles exigen. Causa VERIFICADA en local (sin el CLI en el PATH, la release-probe falla con `[fixture error: spawn No such file or directory (os error 2)]`). Registrada como TASK-37 con sus criterios. El `verify-probe` que esta tarea anade al carril NO necesita ninguno de los dos prerrequisitos (git, shell y la imagen que descarga el host), asi que su resultado en CI no depende de ellos.

LIMITES

- La puerta exige que el documento de evidencia sea coherente consigo mismo y con el candidato, y que la evidencia sea del mismo commit y del mismo bundle verificado. La AUTENTICIDAD del documento (que nadie lo falsee) es responsabilidad del almacen de evidencia, no de esta tarea.
- `autonomous-loop` necesita un agente real dentro del sandbox (credenciales solo por el entorno del proceso): no pertenece al carril de CI, que corre sondas sin agente.
- La puerta exige que los checks declarados en el BINDING de la evidencia esten satisfechos; la puerta no sabe cuales SON los checks correctos: esa politica la fija quien la pide (el bucle autonomo la deriva del criterio de aceptacion del ticket) y que la politica exigida sea la del ticket y no una mas debil es trabajo de MH-A12 (TASK-33).
- El camino feliz y los negativos de la promocion los cubren `verify-probe` y `autonomous-loop`; el fallo del CAS (base movida) lo mantiene `promote-probe`, cuyo sujeto es el algoritmo de git, que esta tarea no cambia.
- La huella de entorno liga la evidencia a un sandbox concreto: la evidencia de una corrida no se reutiliza en otra. Es deliberado (un resultado no es portable entre entornos), y por eso la evidencia que se promotes la produce la MISMA corrida que verifica.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
La verificacion queda ligada al candidato Git que realmente ejecuto: cuatro identidades separadas (GitRevision, GitTree, TransportDigest, EvidenceBinding), el transporte comprobado antes del sandbox y el HEAD y el arbol OBSERVADOS en el checkout y comparados con lo declarado antes de correr un solo check; la evidencia registra lo observado y solo el runner que observa puede acuñarla. La promocion exige evidencia de la misma tarea, del mismo commit y del mismo bundle verificado, con los checks satisfechos y un documento coherente con su propio binding (contenido, transporte, imagen, entorno y comando autorizado); una politica vacia se rechaza. El bucle autonomo, que es la ruta productiva, promueve por esa puerta. El lifecycle server reporta el id de la imagen que CORRIO, leido del contenedor: una imagen fijada por digest vuelve a poder usarse. VERIFICACION: 295 tests verdes, fmt y clippy -D warnings limpios; carril caro 5/5 sondas PASS con 0 residuos; verify-probe 17/17 (el candidato de otro commit se rechaza en 1.5 s sin correr el check; el verificador limpio FALLA el comando que el entorno de desarrollo aprueba; un timeout es un registro AUSENTE; evidencia de otro bundle o de otra politica no promueve) y promote-probe 9/9; autonomous-loop 11/11 en 479 s con 0 residuos (las 5 tareas se verifican, se promueven por la puerta de evidencia y se cierran por la CLI oficial; el ticket de fraude sigue abierto). Dos revisores independientes en contexto nuevo revisaron la rama: sus ocho hallazgos se verificaron contra el codigo y se corrigieron todos, y dos observaciones menores se resolvieron documentando el comportamiento real. El job de CI de integracion sigue ROJO en main por falta del CLI de Backlog.md y del proxy de egreso en el runner: causa verificada y registrada como TASK-37.
<!-- SECTION:FINAL_SUMMARY:END -->
