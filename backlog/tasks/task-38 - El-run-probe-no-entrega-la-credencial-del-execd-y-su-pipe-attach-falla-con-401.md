---
id: TASK-38
title: El run-probe no entrega la credencial del execd y su pipe-attach falla con 401
status: To Do
assignee: []
created_date: '2026-11-18T22:13:00Z'
labels:
  - descubrimiento
  - bug
dependencies: []
references:
  - crates/hodei-mh/src/run_probe.rs
ordinal: 38000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Descubierto al medir TASK-26: la sonda 'hodei-mh run-probe' sigue el camino previo a TASK-25. El execd exige la credencial por sandbox (EXECD_ACCESS_TOKEN) desde la frontera cerrada en TASK-25, pero la sonda no la lee del contrato del endpoint ni la envia en el handshake del WebSocket: el check 'pipe-attach' responde 'websocket handshake failed: HTTP error: 401 Unauthorized'. Es un fallo PREEXISTENTE, reproducido tambien sobre el codigo base (antes de TASK-26). La sonda no forma parte de la pista de sondas (scripts/probes.sh), asi que el fallo no lo detecta ninguna lane. Sus checks de coordinador (exclusive-session-via-coordinator) si pasan.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 La sonda run-probe entrega la credencial del execd que el contrato del endpoint devuelve y su check pipe-attach pasa contra un execd real.
- [ ] #2 El fallo aparece como FALLO de la sonda y no como un check omitido.
<!-- AC:END -->
