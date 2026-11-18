---
id: TASK-37
title: >-
  Hacer verde el carril de sondas en CI: prerrequisitos del runner (CLI
  Backlog.md y proxy de egreso restringido)
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-18T14:07:00Z'
updated_date: '2026-11-18T15:35:00Z'
labels: []
dependencies: []
priority: high
ordinal: 37000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
El job `integration` de .github/workflows/ci.yml (carril caro, `scripts/probes.sh`) esta ROJO en `main` desde su primera ejecucion: el merge del PR #32 (run 44308881486) termino con release-probe FAIL, profile-probe FAIL y multiproject-probe FAIL (supervisor-probe PASS). El carril fallo abierto, que es lo que debe hacer, pero `main` lleva un job rojo desde que existe.

CAUSA VERIFICADA (no supuesta), reproducida en local:
1. El CLI oficial de Backlog.md no esta instalado en el runner. Las sondas que usan el tracker REAL lo necesitan: sin el en el PATH, la `release-probe` falla con `[fixture error: spawn No such file or directory (os error 2)]` (reproducido en local quitando el directorio del CLI del PATH), y la `multiproject-probe` crea cuatro proyectos con backlog real.
2. El proxy de egreso restringido que los perfiles exigen (`http://172.17.0.1:8888`, DEFAULT_PROXY_URL en crates/hodei-mh/src/profile_run.rs) solo existe como script del host en `/tmp/hodei-connect-proxy.py`: no esta versionado, asi que el runner no puede tenerlo. El `doctor` lo comprueba y el gate refuses sin el (sin fallback YOLO), por eso la `profile-probe` falla.

QUE HAY QUE HACER
- Versionar la implementacion del proxy de egreso restringido en el repo (scripts/), con el mismo contrato: CONNECT sobre el gateway de Docker, solo clientes de la subred Docker.
- Instalar el CLI oficial de Backlog.md en el runner y COMPROBAR su presencia antes de las sondas, con motivo claro si falta.
- Arrancar el proxy antes de las sondas y pararlo al final; el doctor debe ver el proxy alcanzable.
- Publicar los ficheros de salida de las sondas (JSON y stderr) como artefacto del job cuando el carril falla: hoy el log del job solo dice el path de /tmp y el diagnostico se pierde.
- Ningun prerrequisito puede degradarse en silencio: si falta algo, el job falla con el motivo.

El carril debe quedar VERDE en `main`, con las sondas que el runner puede sostener, y sin que el carril barato o el de workspace dependan de esto.

REFERENCIAS: .github/workflows/ci.yml (job integration), scripts/probes.sh, crates/hodei-mh/src/profile_run.rs (DEFAULT_PROXY_URL), crates/hodei-mh-profiles/src/{doctor,gate}.rs, crates/hodei-mh/src/{release_probe,multiproject_probe}.rs.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 El runner tiene el CLI oficial de Backlog.md instalado y el carril COMPRUEBA su presencia antes de las sondas, fallando con el motivo si falta
- [ ] #2 El proxy de egreso restringido esta versionado en el repo, arranca en el runner antes de las sondas y el doctor lo ve alcanzable
- [ ] #3 El job integration termina en verde en main y, cuando una sonda falla, sus ficheros JSON y stderr se publican como artefacto para poder diagnosticar
- [ ] #4 Ningun prerrequisito ausente degrada el carril en silencio: el job falla con el motivo
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Versionar el proxy de egreso restringido en scripts/egress-proxy.py: HTTP CONNECT, solo clientes de las subredes Docker, ligado a la direccion del gateway de Docker (no a 0.0.0.0: un runner con IP publica no debe exponer un proxy abierto) y con una linea de 'listo' para que el carril pueda esperar su disponibilidad.
2. Anadir a scripts/probes.sh un PREFLIGHT que compruebe los prerrequisitos que el carril NO puede aportar y falle con el motivo: el CLI oficial de Backlog.md en el PATH (las sondas usan el tracker real), docker disponible y la direccion del gateway de Docker igual a la que usan las sondas (172.17.0.1). Ningun prerrequisito ausente degrada el carril en silencio.
3. El carril ARRANCA el proxy versionado cuando no hay ninguno alcanzable, lo para al final, y registra el estado del proxy en el log. Modo --preflight para diagnosticar el runner sin correr sondas.
4. En .github/workflows/ci.yml: instalar el CLI oficial de Backlog.md (npm, version fijada) antes de las sondas, ejecutar el preflight, y publicar los ficheros de salida de las sondas (JSON y stderr) como artefacto cuando el carril falla.
5. Verificar: en local, el carril con el CLI ausente DEBE fallar con el motivo y con el proxy versionado arrancado por el propio carril las sondas pasan; en CI, dispatch del job integration sobre la rama en verde antes de fusionar.
<!-- SECTION:PLAN:END -->
