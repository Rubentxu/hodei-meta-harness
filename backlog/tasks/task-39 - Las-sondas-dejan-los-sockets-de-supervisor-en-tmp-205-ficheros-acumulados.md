---
id: TASK-39
title: 'Las sondas dejan los sockets de supervisor en /tmp: 205 ficheros acumulados'
status: Done
assignee:
  - '@rubentxu'
created_date: '2026-11-19T02:16:00Z'
updated_date: '2026-11-20T08:07:00Z'
labels:
  - descubrimiento
  - bug
dependencies: []
references:
  - crates/hodei-mh/src/multiproject_probe.rs
ordinal: 39000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Descubierto al cerrar TASK-29: las sondas que arrancan un supervisor por proyecto (multiproject-probe) matan el proceso al terminar, pero el fichero de socket se queda en /tmp: cada corrida deja cuatro ficheros `/tmp/hodei-sup-mproj-<pid>-<proyecto>.sock`. Medido en este host: 205 ficheros de socket huerfanos acumulados (204 de ellos de las carridas de la sonda, mas el guard de docker offline). No hay ningun proceso escuchando detras de ellos (`ss -xl` no lista ninguno), asi que son estado desechable. El `supervisor` solo borra su socket si el bucle de serve termina con normalidad, y la sonda lo mata: por eso el fichero sobrevive. El `bind` borra los sockets rancios al arrancar, asi que el bug no rompe la sonda, pero ensucia el host en cada corrida.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 La sonda que arranca un supervisor por proyecto borra sus ficheros de socket al terminar, tambien cuando mata al proceso a la fuerza.
- [x] #2 Una corrida de la sonda no deja sockets de supervisor en /tmp: el numero de ficheros `/tmp/hodei-sup-mproj-*.sock` antes y despues de la corrida es el mismo.
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
La sonda multiproyecto registra ahora cada socket de supervisor que nombra (los cuatro por proyecto y el alpha relanzado), mata a los supervisores, elimina exactamente esas rutas y publica el paso 'no-supervisor-socket-survives-the-probe', que solo pasa si el cleanup es completo Y el recuento de sockets de supervisor en /tmp despues es igual al de antes. La eliminacion es idempotente (un supervisor que desvinculo su propio socket da AlreadyGone, no fallo), informa fallos por fichero y un Drop cubre la cancelacion antes del cleanup explicito. Evidencia: fast-check local PASS (fmt, clippy -D warnings, 52 tests de crate incluidos 5 de probe_sockets y 3 de supervisor_cli); multiproject-probe real con el proxy de egreso del lane (contenedor hodei-t39-egress, PORT=8082): 14/14 pasos, all_passed=true, 0 residuo (sandboxes=0, containers=0, networks=0); paso de sockets con 4 de 4 ficheros eliminados, 0 supervivientes, 0 sockets de supervisor antes y 0 despues; workspace gate PASS (la primera ejecucion fallo por un timeout flaky de supervisor_cli, que pasa 3/3 en local y en la reejecucion). PR #44 fusionado con squash en 045fded.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
La sonda multiproyecto elimina los sockets de supervisor que crea, tambien tras matar a los supervisores; 2/2 AC comprobados con probe real 14/14 y 0 residuo, gate local y workspace en verde, PR #44 fusionado en 045fded.
<!-- SECTION:FINAL_SUMMARY:END -->
