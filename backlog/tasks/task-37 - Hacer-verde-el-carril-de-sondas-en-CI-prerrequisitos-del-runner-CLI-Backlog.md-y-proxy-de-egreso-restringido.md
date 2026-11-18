---
id: TASK-37
title: >-
  Hacer verde el carril de sondas en CI: prerrequisitos del runner (CLI
  Backlog.md y proxy de egreso restringido)
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-18T14:07:00Z'
updated_date: '2026-11-18T15:52:00Z'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementacion (TASK-37)

QUE CAMBIO

1. `scripts/egress-proxy.py` (NUEVO): el proxy de egreso restringido, VERSIONADO en el repo. Habla CONNECT y acepta solo clientes de las subredes Docker; se liga a la direccion del gateway del bridge (nunca a 0.0.0.0: un runner con IP publica no debe exponer un proxy abierto) y escribe una linea de "listo" para que el carril pueda esperar su disponibilidad. Si no puede ligar, dice el motivo y sale con codigo 1.

2. `scripts/probes.sh`: el carril declara lo que necesita y se niega a correr sin ello, CON EL MOTIVO: el CLI oficial de Backlog.md en el PATH (las sondas manejan el tracker REAL), Docker, la direccion del gateway del bridge (la misma que usan las sondas para encontrar el proxy) y las IMAGENES de los perfiles en el daemon local (el doctor es de solo lectura y rechaza una imagen que no esta ya ahi, asi que nunca descarga ninguna). Si no hay ningun proxy alcanzable, el carril arranca el suyo versionado y lo para al final, incluso si una sonda falla; si falta una imagen, la descarga y dice cual, y si la descarga falla el carril para con el motivo. Nuevo modo `--preflight`: deja el host preparado y comprueba los prerrequisitos sin correr ninguna sonda.

3. `.github/workflows/ci.yml`: el runner instala el CLI oficial de Backlog.md (`npm install -g backlog.md@1.55.3`, version fijada), el preflight corre antes de las sondas, y los ficheros de salida de las sondas (el JSON y el stderr de cada una) se publican como artefacto cuando el carril falla, para que el motivo no se pierda en el log del job.

4. Documentacion actualizada en el mismo cambio: `AGENTS.md` (los prerrequisitos del carril de integracion), `README.md` y `docs/profiles.md` (el proxy es `scripts/egress-proxy.py` del repo, no un script de /tmp que solo existia en el host).

VERIFICACION (medida, no declarada)

- Preflight local con todo presente: 0 (CLI 1.55.3, gateway 172.17.0.1, proxy alcanzable).
- Preflight local SIN el CLI en el PATH: 1, con el motivo ("the official Backlog.md CLI is not in PATH ... install it with 'npm install -g backlog.md@1.55.3'").
- Preflight local con el gateway distinto del que usan las sondas: 1, con el motivo ("the docker bridge gateway is X but the probes reach the proxy at Y: the sandbox would not find the proxy").
- Carril completo con el proxy DEL PROPIO CARRIL (parado antes el proxy del host): 5/5 sondas PASS y 0 residuos; el log del proxy muestra el trafico real (CONNECT a index.crates.io y static.crates.io) y el carril lo detiene al terminar.
- Carril local con el script final (prerrequisitos + imagenes + proxy del propio carril): 5/5 sondas PASS, 0 residuos.
- Job `integration` de CI sobre la rama (run 44337960980): el workspace gate VERDE y el carril de sondas con 4/5 sondas en verde (release, supervisor, multiproject, verify) y `profile-probe` en ROJO.
- Publicacion de artefactos VERIFICADA con ese fallo real: el job publico el artefacto `probe-output` (8333 bytes) con el JSON y el stderr de CADA sonda, y se descargaron y leyeron. Gracias a ese artefacto se vio la causa que el log del job perdia: el `doctor` rechaza una imagen que no esta en el daemon local (`rust:1-bookworm`), y de ahi venia la cascada de fallos de la `profile-probe`. Tercer prerrequisito, anadido al carril (commit `55a8c16`): las imagenes de los perfiles se descargan antes de las sondas.
- Job `integration` de CI sobre la rama tras el tercer prerrequisito (run 44339307422): **VERDE** — workspace gate en verde y carril de sondas con 5/5 PASS (release, supervisor, profile, multiproject, verify) y 0 residuos. El log muestra al preflight descargando `rust:1-bookworm` y `opensandbox/code-interpreter:v1.1.0` y al carril arrancando su propio proxy.

ESTADO DEL CARRIL DE CI (medido, no supuesto)

- El job `integration` de `main` estaba ROJO desde su primera ejecucion (merge del PR #32, run 44308881486) y seguia ROJO tras el PR #33 (run 44337161788): el runner no tiene el CLI oficial de Backlog.md ni el proxy de egreso, y las sondas que usan el tracker REAL fallan por eso. La causa ya estaba registrada en esta tarea y VERIFICADA en local; aqui se corrige.

LIMITES

- El proxy es infraestructura del carril, no del producto: el producto exige un camino de egreso restringido y no lo impone (ver `docs/profiles.md`). El proxy abre la salida de la sandbox hacia internet a traves del host; la restriccion (que solo la subred Docker puede usarlo) la aplica el propio proxy.
- La direccion del proxy (172.17.0.1:8888) es la que usan las sondas dentro de la sandbox. Si el gateway del bridge del host fuera otra direccion, el carril falla con el motivo en vez de fingir que el proxy esta donde no esta: hacer configurable esa direccion en el codigo de producto es trabajo aparte, no de esta tarea.
- La instalacion del CLI en el runner es del workflow, no del repo: el carril comprueba su presencia y falla con el motivo si falta.
- El carril reutiliza un proxy que ya este escuchando en esa direccion (el del host de desarrollo) en vez de pelearse con el: solo arranca el suyo cuando no hay ninguno.
- El carril descarga las imagenes de los perfiles si no estan en el daemon. Es una mutacion del host, y la hace ANTES de cualquier sonda y a la vista (dice cual descarga); la alternativa (que el `doctor` descargara) romperia su contrato de solo lectura.
<!-- SECTION:NOTES:END -->
