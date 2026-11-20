---
id: TASK-34
title: '[MH-A06] Conectar el supervisor de producto al bucle real de objetivos ACP'
status: In Progress
assignee: []
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-20T13:21:00Z'
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
- [ ] #3 Candidato fallido o permiso negado no promociona ni cierra; objetivo sin trabajo no consume sandbox ni tokens.
- [ ] #4 La UAT usa el propio binario, conserva evidencia de SHA, cierre, cancelación y liberación; los probes equivalentes se sustituyen solo tras comprobar paridad.
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
<!-- SECTION:NOTES:END -->
