---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-19T00:50:00Z'
---
# Handoff

> Estado que una sesión deja a la siguiente. Este documento se reescribe **entero** en cada
> cierre de tarea.

## Ahora

- **Ninguna tarea en curso.** TASK-28 está **cerrada** (Done, AC 4/4) y mergeada en `main`.
- Repo: `hodei-meta-harness` (`/home/rdcabrera/Proyectos/rust/goal-cycle-mh`), en `main`,
  `main` = `f154e5b` (merge del PR #39). Árbol limpio, sin ramas locales ni remotas de tarea,
  sin worktrees adicionales.
- Tablero (lo que queda, todo `To Do`): TASK-29 (MH-A11, HIGH), TASK-30 (MH-A07, HIGH),
  TASK-31 (MH-A08, MEDIUM), TASK-33 (MH-A12, HIGH), TASK-34 (MH-A06, HIGH), TASK-35 (MH-A13,
  MEDIUM), TASK-36 (MH-A10, LOW), TASK-38 (sin prioridad, 2 AC). **TASK-34 dependía de TASK-28:
  ya está desbloqueado** y es el que mejor encaja con lo que esta tarea dejó (el supervisor de
  producto tiene que consumir el bucle real de objetivos ACP).
- Sesiones: se abren con el perfil `cuenta-1` (MiniMax-M3.1-Flash). La sesión anterior iba con
  `cuenta-9` (gateway local `qoder2api-hub`, modelos Qoder): ese modelo **corta el turno a
  mitad de tarea** y la iteración se mide FAIL.

## Última tarea cerrada

- **TASK-28 (MH-A05) — Preparar workspaces y comandos sin transportar secretos del host.** Lo que
  sale del host es una **lista explícita de ficheros**, no una copia del árbol de trabajo:
  - Módulo nuevo `hodei-mh-workspace::export` (propietario del empaquetado). La clasificación la
    hace **git** cuando el directorio es la raíz de un árbol de trabajo (el índice voucha lo
    versionado; `diff HEAD` dice qué ficheros llevan cambio sin versionar; `ls-files --others`
    dice lo sin seguimiento). Sin índice (directorio que no es raíz de un árbol, o sin repo), el
    árbol de ficheros **es** el proyecto y el informe lo dice (`source: tree_walk`).
  - La selección la decide `plan::select_files_for_export` con `EXPORT_DENYLIST`: metadata de
    git, caches (`target`, `node_modules`, `__pycache__`, `.venv`, `.gradle`), familia `.env`,
    `*.pem`/`*.key`/`*.p12`, `id_rsa`, `id_ed25519`, `secrets/`, `credentials/`, `.ssh/`,
    `backlog/`, `.claude/`. **Denegado también cuando está versionado**: el gitignore no es un
    detector de secretos.
  - Un simbólico se empaqueta **como** simbólico (nunca se sigue el puntero) y solo se empaquetan
    ficheros regulares (un gitlink queda denegado con su regla). El tar se escribe con la
    **librería** `tar`+`flate2` en un directorio privado `0700` de nombre impredecible, archivo
    `0600` con `O_EXCL`; el valor borra el directorio al soltarse.
  - **Política**: por defecto sale solo el contenido versionado; los cambios sin versionar salen
    solo con `--export-untracked`. El informe `project_export {policy, source, files_exported,
    denied[]}` va en el registro **y** en `candidate.json`.
  - `profile_run`: la exportación se decide **antes** de que exista el sandbox; la salida de cada
    comando se **redacta antes de truncarla**; el detector de fugas lee los comandos **crudos**
    guardados aparte (leer el registro ya redactado dejaría ciego al detector). La ruta del
    proyecto dejó de ser el argumento de un comando del host (se acabó el `tar` de la CLI).
  - `credentials`: la redacción cubre las mismas formas que ve el detector (userinfo sin dos
    puntos, scp-style, `AKIA`/`ASIA`, bloques PEM, prefijos `ghp_`/`sk-ant-`) y el placeholder
    `[redacted]` ya no se confunde con una credencial.
- **Evidencia**: PR #38 (merge `e84c411`; `9d7a042`, `59efe2d`) y PR #39 (merge `f154e5b`;
  `7543ee0`, limpieza del fixture). `cargo fmt`, `cargo clippy --workspace --all-targets -D
  warnings` y `scripts/fast-check.sh` PASS; `cargo test --workspace` verde (34 binarios de test).
  Fixture local con **secretos sintéticos** (7 pruebas offline en
  `crates/hodei-mh-workspace/tests/export_fixture.rs`) que comprueba el **artefacto** (los bytes
  del tar) y el **informe**, sin proveedor LLM, sin sandbox y sin red. Pista de integración contra
  Docker real: `scripts/probes.sh` → **6/6 PASS** (release, supervisor, profile, multiproject,
  verify, trust), `sandboxes=0 containers=0 networks=0`; el `profile-probe` compila los proyectos
  Rust y JVM dentro del sandbox pineado por el camino **nuevo** de exportación. CI: `workspace
  gate` SUCCESS en ambos PR.

## Qué queda a medias

- **Nada de TASK-28.** Salvedades conscientes:
  - **Desviación de la revisión goldfish**: se pedía escribir los miembros del tar con prefijo
    `./` para que un nombre que empieza por `-` no fuera leído como opción al extraer. El crate
    `tar` normaliza `./x` a `x`, así que el prefijo no es posible. El riesgo se **midió** y no se
    materializa: GNU tar 1.34 (la imagen `rust:1-bookworm` del perfil) y 1.35 (el host) extraen un
    miembro llamado `-rf` como fichero literal, `rc=0`, sin interpretarlo como opción.
  - La política de exportación **no** toca los redactores de `provenance` ni de `rollover`
    (drift ya anotado para TASK-34). El token en línea de comandos de `main.rs` y de las sondas
    hermanas sigue siendo follow-up de TASK-34.
  - Un directorio que **no** es raíz de un árbol de trabajo no tiene índice: su árbol entero sale
    (`source: tree_walk`) y el informe lo dice. Es la única fuente no clasificada por git.
  - **Descubrimiento (no arreglado, fuera del alcance de la tarea)**: las pruebas del coordinador
    (`crates/hodei-mh-coordinator/src/store.rs:290` y `actor.rs:369`) crean un directorio temporal
    por caso y **no lo borran**: cada `cargo test` deja ~40 directorios `/tmp/hodei-store-*` y
    `/tmp/hodei-actor-*`. Se limpiaron los 540 que había, pero la fuga sigue ahí.
  - La pista de probes se ejecutó en este host; el CI la ejecuta en `main` (job `probe lane`):
    conviene mirar ese job tras el merge.

## Recursos liberados

| Recurso | Qué era | Liberado |
| --- | --- | --- |
| rama local y remota | `task/task-28-workspace-secretless-export` | sí (PR #38 mergeado, rama borrada local y remota) |
| rama local y remota | `task/task-28-fixture-cleanup` | sí (PR #39 mergeado, rama borrada local y remota) |
| contenedor | `hodei-proxy-t28` (proxy de egreso de confianza de la pista) | sí (`docker rm -f`) |
| sandboxes / redes / contenedores de la pista | los que crearon las 6 sondas | sí (`sandboxes=0 containers=0 networks=0`) |
| procesos auxiliares | el control plane en 8081 y el build release | sí (los lanza y los para la pista) |
| directorios temporales | `/tmp/hodei-store-*`, `/tmp/hodei-actor-*` (540), `/tmp/tartest*` | sí |
| salida de las sondas | `/tmp/hodei-probes` (JSON de las 6 sondas) | **NO**: es la evidencia citada arriba; son unos pocos KB |
| gateway local `qoder2api-hub` | servicio systemd de usuario en 127.0.0.1:8790 | **NO**: sigue en pie a propósito (el perfil `cuenta-9` lo necesita); tiene un *hold* en `/run/user/1001/proxy-hold-qoder2api-hub.service.hold` para que el temporizador de inactividad no lo pare |

## Aprendido

- **Una sesión cortada deja el árbol a medias y, además, con la puerta rota**: el commit `9d7a042`
  de la sesión anterior no compilaba (un `find(["AKIA","ASIA"])` que no existe en `str::find`)
  y además **fallaba `clippy -D warnings`** (un `if` anidado en `plan.rs`). Compilar **y** pasar
  clippy antes de cerrar la tarea es barato y evita que la siguiente sesión descubra dos fallos
  distintos.
- **Un cambio de comportamiento necesita su prueba en el mismo sitio**: el fixture con secretos
  sintéticos (artefacto + informe) es lo que demuestra el AC #4 sin proveedor LLM; la prueba del
  comando crudo frente al registro redactado es lo que demuestra el AC #3. Sin ellas, la
  afirmación "no hay fugas" no tiene dientes.
- **El tar de la CLI era el punto flojo del camino de salida**: empaquetar con la librería quita la
  ruta del proyecto del `argv` de un comando del host y elimina de paso el `ARG_MAX` y el
  `--null -T -` que la revisión goldfish pedía.
- **Un detector que lee lo ya redactado está ciego**: el chequeo de fugas tiene que leer los
  strings **crudos** guardados aparte; y el placeholder `[redacted]` no puede contar como
  credencial, o el registro redactado seguiría "goteando".
- **La pista de integración se puede montar en este host**: el proxy de egreso es un contenedor
  normal (`python:3.12-slim` + `scripts/egress-proxy.py`, `--bind 0.0.0.0 --port 8888`) y el
  `HODEI_MH_PROXY_CHECK_URL` es su IP en el puente por defecto (`http://172.17.0.2:8888`). Con
  `HODEI_MH_CONTROL_TOKEN` y las 6 sondas, `scripts/probes.sh` da verde y sin residuo.
- **Un modelo que corta el turno a mitad de tarea se mide FAIL, y el trabajo se pierde**: por eso
  el trabajo a medias se commitea en la rama de la tarea y se documenta aquí.
