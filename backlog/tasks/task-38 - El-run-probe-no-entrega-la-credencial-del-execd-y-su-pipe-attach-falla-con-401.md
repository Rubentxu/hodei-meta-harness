---
id: TASK-38
title: El run-probe no entrega la credencial del execd y su pipe-attach falla con 401
status: Done
assignee:
  - '@rubentxu'
created_date: '2026-11-18T22:13:00Z'
updated_date: '2026-11-20T07:51:00Z'
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
- [x] #1 La sonda run-probe entrega la credencial del execd que el contrato del endpoint devuelve y su check pipe-attach pasa contra un execd real.
- [x] #2 El fallo aparece como FALLO de la sonda y no como un check omitido.
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
La sonda run-probe adjunta el pipe con las opciones del contrato del endpoint (ExecdEndpoint::attach_options), de modo que la credencial EXECD_ACCESS_TOKEN del sandbox viaja en la cabecera del handshake WebSocket y no en la URL. La credencial y el attach se informan como checks propios; una negativa del execd es un check FALLIDO y una sonda sin checks nunca es verde. Evidencia: corrida real de 'hodei-mh run-probe' contra el lifecycle-server vivo y la imagen opensandbox/code-interpreter:v1.1.0, con execd-credential-delivered y pipe-attach en passed=true y all_passed=true (exit 0, 11.85 s); pruebas offline con execd falso que responde 401 (sin tokens ni sleeps) para la negativa y el caso sin credencial; scripts/fast-check.sh crates/hodei-mh/src/run_probe.rs en PASS y workspace gate de CI en PASS. PR #43 fusionado en main (e9829ed, mensaje reescrito sin el trailer que la política git-control prohíbe).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
run-probe entrega la credencial del execd por cabecera y verifica el attach contra un execd real; 2/2 AC comprobados, gate local y workspace en verde, PR #43 fusionado.
<!-- SECTION:FINAL_SUMMARY:END -->
