---
id: TASK-34
title: '[MH-A06] Conectar el supervisor de producto al bucle real de objetivos ACP'
status: In Progress
assignee: []
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-20T09:53:00Z'
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
- [ ] #1 Un repo externo con Backlog.md y dos tickets dependientes se ejecuta mediante CLI de producto y ACP real; el segundo recibe el commit promovido del primero.
- [ ] #2 Start/Pause/Resume/Stop corresponden a efectos confirmados u objetivos explícitamente pendientes/inciertos; Renew solo confirma si backend lo confirma, si no Unsupported.
- [ ] #3 Candidato fallido o permiso negado no promociona ni cierra; objetivo sin trabajo no consume sandbox ni tokens.
- [ ] #4 La UAT usa el propio binario, conserva evidencia de SHA, cierre, cancelación y liberación; los probes equivalentes se sustituyen solo tras comprobar paridad.
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementación integrada en la rama task/task-34-live-core (PR #47, head ec4f11b rebasado sobre main 2e1fa12): supervisor --live con tracker real, agente ACP en sandbox, verificación, candidato con autoría del host y promoción con CAS honesto (RefRejected vs BaseMoved); stop cooperativo con stops pendientes por RunId; resultado de un run detenido solo se aplica si es un efecto confirmado; el CLI deja de entrar en pánico cuando el lector cierra el pipe (sup-events | head). Gates: scripts/fast-check.sh --all PASS antes y después del rebase; cobertura determinista de stop antes del intento, durante la verificación y durante la publicación, arranque tardío de un run anterior y EPIPE.

UAT con binario real y Docker (evidencia en /tmp/uat-task34/evidence y evidence2): AC2(a) pause/resume PASS; AC2(d) Renew Unsupported explícito PASS (applied=false); AC3(iii) tablero vacío no crea sandbox PASS; AC4 parcial: SHA de binarios hodei-mh 1c906919… y servidor 961241f7…, cierre de sandbox y liberación de claims observados, escaneo de secretos 0 coincidencias. El fallo de promoción previo quedó corregido en producto (autoría del commit en el host + RefRejected honesto) y verificado contra los hooks reales.

BLOQUEO EXTERNO (requiere intervención humana): ningún perfil configurado puede ejecutar un turno ACP real. Sondado con una petición mínima a /v1/messages: cuenta-1 minimax 429 'Token Plan usage limit reached (2056)'; cuenta-2 minimax 402 insufficient balance; cuenta-3 y cuenta-4 z.ai 429 límite semanal/mensual, reinicia 2026-11-21 23:07 (hora local); cuenta-5 deepseek 401 clave inválida; cuenta-6 sin token; cuenta-7/9 puentes locales sin listener; cuenta-8 aliyun 403 clave inválida. Sin proveedor no se pueden demostrar AC1 (dos tickets dependientes con ACP real), AC2(c) (veredicto confirmado tras stop) ni AC3(i,ii) (check insatisfacible y permiso denegado). No se marca la tarea como Done hasta completarlos.

Reanudación: restaurar cuota o credenciales de un perfil; reiniciar proxy de egreso y lifecycle-server (PORT 18081/HODEI_MH_CONTROL_TOKEN/HODEI_MH_EGRESS_PROXY_CONTAINER), supervisores en /tmp/uat-task34/sup{,2}-a.sock con PATH normal (los hooks de git-control del host deben quedar activos: el fixture ya es conforme); fixtures repo-a y repo-c intactos en /tmp/uat-task34/repos. La receta completa está en /tmp/uat-task34/evidence/commands.txt.
<!-- SECTION:NOTES:END -->
