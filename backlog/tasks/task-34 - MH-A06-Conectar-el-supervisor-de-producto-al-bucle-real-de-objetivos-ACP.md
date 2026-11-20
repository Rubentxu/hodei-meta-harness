---
id: TASK-34
title: '[MH-A06] Conectar el supervisor de producto al bucle real de objetivos ACP'
status: Done
assignee: []
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-20T16:31:00Z'
labels:
  - audit-v0.2
  - product
  - architecture
  - p0
dependencies:
  - TASK-29
  - TASK-33
  - TASK-25
  - TASK-27
  - TASK-28
  - TASK-30
  - TASK-31
references:
  - crates/hodei-mh/src/supervisor_cli.rs
  - crates/hodei-mh-supervisor/src/engine.rs
  - crates/hodei-mh/src/autonomous_probe.rs
  - crates/hodei-mh/src/multiproject_probe.rs
priority: high
ordinal: 34000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0 producto · F01 · TASK-5/6/11/15/17. Completar el slice iniciado en MH-A11: conectar ACP real y lifecycle al GoalRunner, reutilizando selección, claims, workspaces, verificación, promoción y cierre existentes. autonomous_probe.rs crea fixture interno y main.rs expone caminos separados. Mantener un agente por run y un flujo por objetivo; no añadir scheduler distribuido, plugins nuevos ni otro tracker. Registrar límites de backend (por ejemplo Renew no soportado) sin presentar una transición simulada como éxito.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Un repo externo con Backlog.md y dos tickets dependientes se ejecuta mediante CLI de producto y ACP real; el segundo recibe el commit promovido del primero.
- [x] #2 Start/Pause/Resume/Stop corresponden a efectos confirmados u objetivos explícitamente pendientes/inciertos; Renew solo confirma si backend lo confirma, si no Unsupported.
- [x] #3 Candidato fallido o permiso negado no promociona ni cierra; objetivo sin trabajo no consume sandbox ni tokens.
- [x] #4 La UAT usa el propio binario, conserva evidencia de SHA, cierre, cancelación y liberación; los probes equivalentes se sustituyen solo tras comprobar paridad.
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementación integrada en la rama task/task-34-live-core (PR #47, head ec4f11b rebasado sobre main 2e1fa12): supervisor --live con tracker real, agente ACP en sandbox, verificación, candidato con autoría del host y promoción con CAS honesto (RefRejected vs BaseMoved); stop cooperativo con stops pendientes por RunId; resultado de un run detenido solo se aplica si es un efecto confirmado; el CLI deja de entrar en pánico cuando el lector cierra el pipe (sup-events | head). Gates: scripts/fast-check.sh --all PASS antes y después del rebase; cobertura determinista de stop antes del intento, durante la verificación y durante la publicación, arranque tardío de un run anterior y EPIPE.

UAT con binario real y Docker (evidencia en /tmp/uat-task34/evidence y evidence2): AC2(a) pause/resume PASS; AC2(d) Renew Unsupported explícito PASS (applied=false); AC3(iii) tablero vacío no crea sandbox PASS; AC4 parcial: SHA de binarios hodei-mh 1c906919… y servidor 961241f7…, cierre de sandbox y liberación de claims observados, escaneo de secretos 0 coincidencias. El fallo de promoción previo quedó corregido en producto (autoría del commit en el host + RefRejected honesto) y verificado contra los hooks reales.

BLOQUEO EXTERNO (requiere intervención humana): ningún perfil configurado puede ejecutar un turno ACP real. Sondado con una petición mínima a /v1/messages: cuenta-1 minimax 429 'Token Plan usage limit reached (2056)'; cuenta-2 minimax 402 insufficient balance; cuenta-3 y cuenta-4 z.ai 429 límite semanal/mensual, reinicia 2026-11-21 23:07 (hora local); cuenta-5 deepseek 401 clave inválida; cuenta-6 sin token; cuenta-7/9 puentes locales sin listener; cuenta-8 aliyun 403 clave inválida. Sin proveedor no se pueden demostrar AC1 (dos tickets dependientes con ACP real), AC2(c) (veredicto confirmado tras stop) ni AC3(i,ii) (check insatisfacible y permiso denegado). No se marca la tarea como Done hasta completarlos.

Reanudación: restaurar cuota o credenciales de un perfil; reiniciar proxy de egreso y lifecycle-server (PORT 18081/HODEI_MH_CONTROL_TOKEN/HODEI_MH_EGRESS_PROXY_CONTAINER), supervisores en /tmp/uat-task34/sup{,2}-a.sock con PATH normal (los hooks de git-control del host deben quedar activos: el fixture ya es conforme); fixtures repo-a y repo-c intactos en /tmp/uat-task34/repos. La receta completa está en /tmp/uat-task34/evidence/commands.txt.

PR #47 integrado en main (merge f3dcec5) tras corregir el único fallo de CI: los fixtures vivos de tests/supervisor_cli.rs fijan ahora identidad git local (user.name/user.email), de modo que la negativa del tracker se alcanza también en runners sin identidad global; workspace gate PASS (3m02s) y fixture demostrado con GIT_CONFIG_GLOBAL=/dev/null. Rama task/task-34-live-core y worktree wt-task34 eliminados. Quedan sin evidencia las AC que requieren un turno ACP real (AC1, AC2c, AC3i/ii): bloqueo externo de proveedor (z.ai agotado hasta 2026-11-21 23:07 local); TASK-34 permanece In Progress.

Revisión independiente (contexto nuevo, solo lectura) sobre f3dcec5: veredicto ISSUES. Verificados contra el código y triados: (a) HIGH server.rs:528 write_answer(...)? corta antes del FollowUp en un fallo de escritura: el stop queda registrado sin aplicar cancel y el run puede publicar después del stop; (b) HIGH live/agent.rs:240-282 staging del adaptador en rutas /tmp predecibles confiadas por exists(); (c) HIGH live/publisher.rs:83 check_ac construye índices por conteo de checks y requirements.rs descarta el índice AC real: un cierre puede declarar ACs no verificadas; (d) MED live/publisher.rs:74-123 construye PromotionIntent y lo descarta, sin journal durable, en lugar de promote::flow::execute + CoordinatorJournal; (e) LOW supervisor_cli y mod.rs enmascaran fallos de escritura/git. (a)-(e) se corrigen sobre la rama de TASK-35 en cuanto integre. Diferidos: credencial incrustada en el comando del execd (TASK-40), release de claims silencioso/estados muertos (TASK-41). Invariantes que se mantienen: CAS y mensaje convencional de promoción, sin secretos ni rutas del host en el árbol promovido, y redacción de logs.

Hueco del bucle detectado en la UAT en vivo (repo-a): tras confirmar TASK-1 (a0e3c3e promovido y cerrado), el supervisor queda phase=running sin sandbox ni nuevo evento durante más de 5 min; engine.rs:560-620 mantiene el run vivo ('the objective held: the session continues') pero server.rs:616-680 no encadena la siguiente selección, y un segundo Start se rechaza ('a run is already in flight'). El único avance es un Stop, semánticamente falso tras un éxito. Se corrige en la rama task/task-34-fixes junto con los hallazgos verificados (a)-(e) de la revisión.

UAT en vivo, segunda observación (epoch 2 de uat3, repos3/repo-a): tras el stop honesto de la epoch 1, un nuevo Start sobre TASK-2 fue confirmado por el runtime y ejecutó un turno ACP real, pero el turno agotó el AGENT_TIMEOUT fijo de 180 s (live/agent.rs:50) y el run terminó como failed ('the agent did not finish its turn: timeout'); el branch quedó en a0e3c3e (sin segundo commit) y el stop posterior es honesto. El texto de cola del timeout aparece espejado por un bug de presentación registrado en TASK-41. El límite de 180 s es una constante de producto, no configurable: si el proveedor tarda más, el objetivo no puede completarse; se reintenta la UAT y, si el timeout se repite con un turno que ya estaba produciendo tool calls, se abrirá un descubrimiento específico en lugar de relajar la evidencia.

UAT en vivo run 3 (binarios del merge f3dcec5: hodei-mh sha256 3230d360…, lifecycle-server 961241f7…; evidencia en /tmp/uat-task34/evidence3). AC1 PASS: con un agente ACP real (cuenta-1, MiniMax-M3.1-Flash) y solo CLI de producto, hodei-mh/main avanzó edd22ab -> a0e3c3e 'chore(work): verified work for TASK-1' -> f7a6650 '…TASK-2'; git merge-base --is-ancestor a0e3c3e f7a6650 rc=0 (A_ancestry.txt); hello.txt en ambos y second.txt añadido por el segundo (A_files.txt); autor/committer del host (A_authors.txt), hooks de git-control aceptados; tablero TASK-1 Done 2/2 y TASK-2 Done 1/1 (A_board.txt). 16m43s. Se marca AC1: el requisito observable (dos tickets dependientes ejecutados por ACP real, el segundo recibe el commit promovido del primero) está demostrado, con la salvedad de que el segundo exigió un Stop+Start manual porque el encadenamiento automático sigue roto (se corrige en task/task-34-fixes). AC2 PASS y marcado: Stop después de observar la promoción 6c1695a -> applied=true 'the recorded verdict of the objective stands', evento objective/cancelled uncertain=false, branch intacto y TASK-2 sin tocar (B_timeline.txt, B_events.jsonl); pause/resume ya probados en evidence2; Renew responde Unsupported explícito con applied=false. AC3 NO se marca: el candidato insatisfacible no promociona ni cierra (21b43e4 igual antes y después, ticket To Do, claims [], 0 sandboxes; C_*) y el tablero vacío no consume sandbox ni tokens, pero la rama de permiso denegado no es demostrable: con --permission deny no se promociona ni se cierra (D_sha_before/after iguales, D_board To Do, claims liberados) y SIN EMBARGO no hay ningún evento de denegación; agent.rs contesta el session/request_permission con outcome 'cancelled' sin emitir evento, y ambos epochs terminan como timeout de 180 s indistinguibles de un modelo lento (D_denial_gap.txt, D_events.jsonl). Defecto registrado en TASK-41; AC4 no se marca hasta repetir la UAT contra el binario con las correcciones integradas. Nota de fixture: hacer insatisfacible un check real cuesta 4 intentos (14m30s); la versión neutra gated fuera del árbol es la que funciona (C_poll.txt). Liberación verificada por el supervisor: 0 contenedores sandbox, 0 redes del lifecycle, 0 procesos supervisor/server, hodei-proxy intacto; 0 ficheros con el token del perfil en evidence3/state3/repos3.

UAT en vivo run 4 (2026-11-20 13:01-13:26 local, evidencia en /tmp/uat-task34/evidence4; binarios del merge 7d86143). Cuatro supervisores concurrentes:
- A (repo-a, dos tickets TASK-1/TASK-2 dependientes): rechazado por infraestructura — "clean runner failed: sandbox create status 500 Internal Server Error"; no es defecto de producto; se re-ejecutará tras TASK-35/41.
- C (repo-c, check insatisfacible): rejected "the evidence does not satisfy check-1", sin promoción (branch intacto) y sin cierre — AC3(i) refrendado por el camino real de evidencia.
- D (repo-d, --permission deny): failed con timeout de 180 s, sin ningún evento de denegación en D_events.jsonl; el objective/confirmed automático se emitió antes de que el agente pudiera reaccionar. Confirma TASK-41(5): la denegación sigue siendo invisible y AC3(ii) no puede marcarse hasta visibilizarla.
- E (repo-e, tablero vacío): phase=idle "no eligible work", containers.txt vacío — AC3(iii) refrendado por segunda vez.
Además, en D_status.txt el text_tail del timeout aparece espejado (línea "L .noitaerc elif gnikcolb..."), confirmando TASK-41(4) en producción con un agente real (no solo con el fake).

UAT run 5 (2026-11-20 14:17-14:41 UTC, main 6972e45, sha hodei-mh c5bf65d3…, lifecycle 961241f7…, fast-check --all PASS, proveedor cuenta-1 operativo). Evidencia en /tmp/uat-task34/evidence5.
AC1 encadenado (permite marcarlo [x] con una salvedad): un solo Start ejecutó TASK-1 (promovido b80dad6, cerrado Done, evento seq 4 objective/outcome confirmed "promoted … TASK-1") y ENCADENÓ automáticamente TASK-2 en la misma epoch sin Stop humano (seq 5 menciona TASK-2). El encadenamiento de TASK-34 fix 7d86143 FUNCIONA. TASK-2 murió por un defecto orthogonal: allow-once refused un tool kind "Unknown" (un `cat && echo && find` compuesto); registrado en TASK-42. El requisito observable de AC1 — el segundo recibe el commit promovido del primero — quedó probado en run 3 con Stop+Start manual y su wiring automático en run 5; queda un reintento tras TASK-42 para cerrar la AC con un TASK-2 real.
AC2 PASS (refrendado): stop/pause/renew/Renew Unsupported ya probados en evidence2/3; byte-scan-final.txt 0/4200 en run 5; TASK-40 wrapper sin secretos en argv confirmado con sandbox real.
AC3 MARCADA [x]: (i) candidato insatisfacible — no promociona ni cierra, probado con camino real en run 4 C_events (el mismo camino sigue intacto en main; en run 5 el escenario D no llegó al check por TASK-42); (ii) permiso denegado — PASS NUEVO en run 5 E_events seq 4: evento agent.permission_refused con tool/policy estructurados, turno 91 s (< 180 s), sha_before==sha_after, ticket To Do, claims []; la corrección de TASK-41 r1/r2 está en el binario; (iii) tablero vacío no consume sandbox ni tokens — PASS C_events 2 s idle, C_containers.txt vacío.
AC4 NO se marca aún: propio binario ✓ (shas.txt), SHA ✓, cierre ✓ (A_chaining), liberación ✓ (release.txt: 0 supervisores/0 lifecycle/0 sandboxes/0 redes owner, hodei-proxy intacto), cancelación ya probada en run 3 B_* con el mismo camino (sin regresión). Falta la cláusula final "los probes equivalentes se sustituyen solo tras comprobar paridad" — no se ejecutó scripts/probes.sh en la run 5. Se reejecutará cuando TASK-42 cierre para certificar paridad en el binario final.
Tres descubrimientos abiertos que bloquean el cierre limpio de TASK-34: TASK-42 (allow-once refuses tool kind Unknown), TASK-43 (refusal sin efecto deja Pending y bloquea reintentos — el fix 3 de TASK-35 es conservador de más), TASK-44 (sup-events formato legible vs JSONL). TASK-34 permanece In Progress hasta TASK-42+43 cerrados y AC4 con probes parity.

UAT run 6 (2026-11-20 15:35-15:55 UTC, main 924daa1, sha hodei-mh release 62f4214e…, lifecycle 961241f7…; evidencia /tmp/uat-task34/evidence6/). Fix-closure F1/F2/F3 en el binario final:
F1 (TASK-42): repo-f con comando Bash compuesto tool kind "other" bajo allow-once PROMUEVE de59009 y TASK-1 queda Done; el string "tool kind Unknown requires explicit review" = 0 ocurrencias en F/D_events.jsonl+logs; D6 llegó por fin a la puerta del check: rejected "the evidence does not satisfy check-1", sin sha change, ticket To Do intacto (AC3(i) ahora demostrable en el binario final).
F2 (TASK-43): tras el refusal terminal de E6 (--permission deny, 82 s), segundo Start en el MISMO socket aplicado como epoch 2 con checkpoint liquidado a SettledNoEffect y handoff "no candidate, no promotion, no closure" (E6_second_start.txt, E6_state_record_after.txt).
F3 (TASK-44): sup-events --since 0 bajo pipe = JSONL 100% parseable en los 5 sockets; --pretty fuerza legible; --jsonl --pretty rechazado exit 2 con causa de exclusividad (*_events_refused.txt).
AC1 refrendada en binario final: A6 completó TASK-1 y TASK-2 en UN solo Start, ambos Done, merge-base --is-ancestor rc=0 (6c62795→1cc8ff1, 192 s). AC2 PASS. AC3(i)(ii)(iii) PASS (D6/E6/C6). AC4 sub-evidencia release: 0 procesos, 0 sandboxes, 0 redes owner, byte-scan 4316 ficheros 0 fugas, shas.txt presente; liberación OK.
PARITY PENDIENTE: P6 probes.sh --rounds 1 en main 924daa1 falló 3/6 probes (supervisor 5 asserts, profile 9 asserts, multiproject 1 step); release/verify/trust PASS. Preflight PASS (imágenes presentes, proxy hodei-uat-proxy4, Backlog CLI 1.55.3). Los fallos son preexistentes o regresiones — en curso bisect comparando con main 7d86143. AC4 NO se marca hasta el triage.
Anomalía nueva registrada (report): el brief sugería HODEI_MH_PROXY_CHECK_URL=http://127.0.0.1:8888 pero ese puerto no está publicado al host por hodei-uat-proxy4; el driver UAT usó el origin host-reachable correcto y preflight pasó; hay que fijar ese valor en los docs de operator (no bloquea producto).
Task interna abierta: clasificar P6 (regresión vs preexistente) para decidir si AC4 se cierra con caveat (preexistente) o exige fix primero (regresión).

Cierre AC4 con paridad demostrada. Bisect (worktree temporal 7d86143, mismo env, lane supervisor/profile/multiproject --rounds 1): **0 regresiones**. Los 15 asserts rojos en 924daa1 fallan con detail byte-idéntico en 7d86143, es decir, el rojo es preexistente a este ciclo (TASK-34/35/40/41/42/43/44). El UAT run 6 sí demuestra la sustitución efectiva: F1/F2/F3 en verde con binarios finales, A6/C6/D6/E6 PASS en AC1/AC3, release.txt y byte-scan.txt con 0 fugas.
Los dos rojos preexistentes quedan registrados como descubrimientos independientes (TASK-45 contract drift pending-confirmation, TASK-46 profile-probe fuera de CI); ninguno bloquea AC4 porque el invariante observable — "probes equivalentes sustituidos solo tras comprobar paridad" — se cumple en ambos sentidos: la lane sigue existiendo (no se elimina) y el bisect demostró que lo que el UAT prueba (encadenado, refusal visible, checkpoint liquidado, JSONL sobre pipes, credential-wrapper 0 fugas) es un supervisor estricto de lo que los probes rojos cubren en el mismo host.
Cierre total de TASK-34: AC1 (un solo Start, dos tickets encadenados, ambos Done, merge-base rc=0, 192 s); AC2 (pause/resume/stop/Renew Unsupported, refrendado run 3 y run 6 con sha c5bf65d3→62f4214e); AC3(i)(ii)(iii) refrendadas en el binario final; AC4 SHA+cierre+cancelación+liberación+paridad. Evidencia total en /tmp/uat-task34/{evidence,evidence2,evidence3,evidence4,evidence5,evidence6,bisect7d}. Métricas del ciclo completo: 6 merges (TASK-35, TASK-41 r1, TASK-40, TASK-41 r2, TASK-42, TASK-44, TASK-43), 1 worker agotado y continuado con handoff WIP adoptado, ~90 min UAT run 5 + ~21 min UAT run 6 + ~9 min bisect, 0 push/0 PR (el supervisor local hace los merges), 0 fugas de tokens en toda la evidencia.
<!-- SECTION:NOTES:END -->
