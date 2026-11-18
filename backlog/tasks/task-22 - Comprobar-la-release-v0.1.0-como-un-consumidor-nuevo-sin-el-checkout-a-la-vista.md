---
id: TASK-22
title: >-
  Comprobar la release v0.1.0 como un consumidor nuevo, sin el checkout a la
  vista
status: Done
assignee:
  - '@claude'
created_date: '2026-11-18T11:27:00Z'
updated_date: '2026-11-18T12:11:00Z'
labels: []
dependencies:
  - TASK-21
ordinal: 22000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
La release v0.1.0 se publico y se verifico desde el propio checkout: quien la comprobo conocia el codigo, tenia el repositorio a mano y ejecuto las sondas que el propio producto define. Lo que NO esta comprobado es lo que ve un consumidor que solo tiene el bundle publicado: la superficie de la CLI descubrible sin mas, los caminos de error honestos, el flujo documentado de primera ejecucion tal como viene escrito en la documentacion que viaja DENTRO del bundle, y el supervisor, que ninguna verificacion de consumidor ha ejercido hasta ahora. Una release cuyo bundle no basta por si solo para descubrir como usarla tiene un hueco de documentacion aunque su README en el repositorio sea bueno. Este trabajo es de verificacion: no se toca el codigo del producto salvo que aparezca un defecto real, y en ese caso se corrige en su propia tarea.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 El bundle publicado se instala en un prefix limpio, fuera de todo checkout, y su digest coincide con el SHA256SUMS publicado; la comprobacion se hace con el repositorio ausente del PATH y sin que el binario instalado pueda encontrar codigo fuente
- [x] #2 La superficie de la CLI es descubrible solo con la release: --version responde la version y --help lista los comandos reales; un comando desconocido sale con codigo distinto de cero y el uso, sin panic ni traza
- [x] #3 El flujo documentado de primera ejecucion funciona tal como viene escrito en INSTALL.md y docs/profiles.md (los que viajan DENTRO del bundle): el control-plane arranca, doctor --profile rust da gate PROCEED, y cada comando nombrado por esa documentacion responde de forma documentada
- [x] #4 Una operacion real desde el prefix instalado: profile-run compila y ejecuta un proyecto en el sandbox pineado, con el digest observado igual al pin y sin posiciones de fuga de secretos
- [x] #5 La superficie del supervisor funciona desde el prefix instalado: un supervisor por proyecto, sup-status lo ve, un segundo supervisor sobre el mismo socket se RECHAZA (sin split-brain) y el socket se queda limpio al terminar
- [x] #6 La documentacion que viaja en el bundle es suficiente: cada comando que nombran INSTALL.md y docs/profiles.md es alcanzable desde el binario instalado, y ninguno devuelve un error no documentado
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
**Consumer test de v0.1.0 (el bundle publicado, instalado FUERA del checkout).** Prefijo limpio /tmp/consumer22/prefix (el checkout no esta en el PATH y el binario instalado no lo referencia: 0 rutas del repo en sus cadenas de panic; las unicas rutas absolutas son las del registro de crates de terceros). Bundle descargado del release remoto y digest igual al SHA256SUMS publicado (el tarball: OK). AC1: instalacion fuera del checkout, sin codigo fuente de por medio.

**AC2 — superficie descubrible.** '--version' y '-V' responden 'hodei-mh 0.1.0' (exit 0); '--help' lista los comandos reales (doctor, profile-run, supervisor, sup-status|sup-action|sup-events|attach-terminal|tui, package y las sondas) (exit 0); un comando desconocido imprime el uso y sale con 2, SIN PANIC ni traza; sin argumentos tambien sale con 2.

**AC3 — el flujo documentado, con el control-plane DEL BUNDLE en el 8081 que dicen los docs** (se paro el del checkout para que la ejecucion sea la documentada, no una variante). 'doctor --profile rust' -> gate PROCEED; 'doctor --profile jvm' -> gate PROCEED; el doctor no creo nada (0 sandboxes y 0 contenedores antes y despues).

**AC4 — operacion real.** 'profile-run --profile rust --project /tmp/consumer22/project' desde el prefix instalado: all_succeeded=true, gate PROCEED, cargo 1.100.0 en el sandbox pineado, el binario compilo y ejecuto el programa del consumidor, digest observado IGUAL al pin, sin posiciones de fuga de secretos, canal de credenciales = env del proceso, candidato exportado con digest_confirmed=true, egress medido desde dentro del sandbox.

**AC5 — el supervisor (superficie que ninguna verificacion de consumidor habia ejercido).** 'supervisor --socket ... --project hodei-mh' levanta el socket; 'sup-status' lo ve; un SEGUNDO supervisor sobre el socket VIVO se RECHAZA con 'a supervisor is already listening on ...: refusing to start a second controller' (exit 1, sin split-brain); 'sup-action start|pause|resume|stop' se aplican con '(confirmation is real)' y 'renew' se rechaza con un motivo honesto ('no managed environment ttl to renew (this was NOT executed)', exit 1); un 'start' con otro proyecto se rechaza con 'project mismatch'; 'sup-events' muestra el evento de arranque con las credenciales redactadas; un supervisor tras un crash TOMA el socket viejo; 'tui' sin terminal falla con honestidad ('cannot enable raw mode: ...', exit 1), sin panic.

**HALLAZGOS (defectos reales, reproducidos).** D1 (media-alta): 'hodei-mh package' toma 'docs/profiles.md' de una ruta NO verificada relativa al DIRECTORIO DE TRABAJO: desde un directorio sin ese fichero falla con 'staging failed for docs/profiles.md' (exit 1, mensaje no documentado) y desde un directorio con OTRO docs/profiles.md mete en el bundle la documentacion AJENA en silencio, con el manifiestoulenotando 'standalone_binaries=true, undemonstrated_dependencies=[]'. El binario instalado tiene su documentacion en <prefix>/share/hodei-mh/docs/profiles.md, que es donde deberia mirarla. D2 (media): el bundle NO lleva los digests de los binarios que contiene, y el INSTALL.md del bundle no menciona el checksum en absoluto: el consumidor que solo tiene el bundle no puede verificar la integridad sin red. D3 (baja): '--help' apunta a 'docs/profiles.md', ruta que NO existe junto al binario instalado (la doc esta en <prefix>/share/hodei-mh/docs/).

**AC6 NO se cumple en v0.1.0** (queda sin marcar, con la razon): el comando 'package' —que nombran INSTALL.md y docs/profiles.md— es alcanzable, pero desde el binario INSTALADO y con un directorio de trabajo cualquiera devuelve 'staging failed for docs/profiles.md', un error que la documentacion no explica; y en el caso peor (un directorio con otro docs/profiles.md) mete en el bundle una documentacion AJENA sin avisar. La documentacion que viaja en el bundle, ademas, no permite verificar la integridad sin red (D2). La correccion de D1, D2 y D3 va en su propia tarea (TASK-23) y no puede cerrar AC6 hasta que exista una release que la lleve.

**CIERRE EN v0.1.1 (la release que lleva el arreglo).** Publicada desde `d444602` (tag v0.1.1), con los cuatro assets y el `SHA256SUMS` de la release. Verificacion de consumidor REAL contra los assets publicados (descargados del release remoto, no del checkout): digest de los tres assets = OK contra el `SHA256SUMS` publicado; el bundle desempaquetado en un directorio PROPIO (fuera de todo checkout) verifica sus propios digests offline (`bin/hodei-mh: OK`, `bin/hodei-mh-lifecycle-server: OK`, `docs/profiles.md: OK`); instalado en `/tmp/hodei-release-consumer-0111/prefix`; `doctor --profile rust` da gate PROCEED; `profile-run` real da all_succeeded=true con el candidato exportado y el entorno liberado; 0 sandboxes de residue; el binario corre en una imagen `FROM scratch` (sin libc, sin loader, sin shell).

**AC6 se cumple en v0.1.1.** Con el binario INSTALADO y el directorio de trabajo `/tmp/foreign-0111` (que contiene OTRO `docs/profiles.md`), `hodei-mh package` responde con `origin: "installed"`, el manifiesto declara el digest `a956b9d8...` de la documentacion del PRODUCTO y el `docs/profiles.md` que viaja en el bundle es el del producto ("# hodei-meta-harness: language profiles (Rust / JVM)"), no el ajeno. Ademas el bundle lleva los digests de lo que embarca y su `INSTALL.md` explica la verificacion sin red, y `--help` nombra la documentacion que el consumidor tiene de verdad (`<prefix>/share/hodei-mh/docs/profiles.md`), que existe. Ningun comando que nombran `INSTALL.md` y `docs/profiles.md` devuelve ya un error no documentado.

**Evidencia de la release que la lleva:** 256 tests verdes, fmt y clippy -D warnings limpios en el commit publicado; TRES rondas SEGUIDAS de las cuatro sondas (release, supervisor, profile, multiproject) contra la infra viva con los binarios construidos desde el commit etiquetado en un clon limpio: 12/12 PASS, 0 residuos.

**Nota de proceso (transparencia).** El tag v0.1.1 se apunto al commit `cd72ff4` y se REAPUNTO a `d444602` ANTES de que existiera la release (no se publico ningun asset de v0.1.1): la primera sonda de las rondas fallo porque la sonda `multiproject-probe` afirmaba un conjunto de contenidos del bundle que no includia el nuevo `SHA256SUMS`, y el binario publicado habria carried esa sonda con una expectativa obsoleta. Se corrigio la sonda (PR #30) para que ademas VERIFIQUE el sello del bundle con `sha256sum -c`, y se reconstruyo todo desde el commit corregido. Ninguna release publicada fue reescrita: v0.1.0 sigue intacta.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
La release v0.1.0 se compro como un consumidor nuevo y salio con tres defectos reales en la ruta de distribucion (D1 documentacion de origen no verificado, D2 bundle sin digests, D3 --help con una ruta inexistente). Los tres se corrigieron en TASK-23 y se publicaron en v0.1.1 (tag v0.1.1 -> d444602). Verificado contra los assets publicados: digest OK, bundle verificado offline, instalacion fuera del checkout, --help apuntando a la documentacion del prefix, doctor PROCEED, profile-run real con 0 residuos, y 12/12 sondas en tres rondas seguidas.
<!-- SECTION:FINAL_SUMMARY:END -->
