---
id: TASK-39
title: 'Las sondas dejan los sockets de supervisor en /tmp: 205 ficheros acumulados'
status: To Do
assignee: []
created_date: '2026-11-19T02:16:00Z'
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
- [ ] #1 La sonda que arranca un supervisor por proyecto borra sus ficheros de socket al terminar, tambien cuando mata al proceso a la fuerza.
- [ ] #2 Una corrida de la sonda no deja sockets de supervisor en /tmp: el numero de ficheros `/tmp/hodei-sup-mproj-*.sock` antes y despues de la corrida es el mismo.
<!-- AC:END -->
