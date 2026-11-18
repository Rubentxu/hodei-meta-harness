---
id: TASK-32
title: '[MH-A09] Reducir el coste de verificación y CI con pruebas por riesgo'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-18T11:50:00Z'
updated_date: '2026-11-18T12:54:00Z'
labels:
  - audit-v0.2
  - testing
  - delivery
  - p1
dependencies: []
references:
  - Cargo.toml
  - crates/hodei-mh/src/main.rs
  - crates/hodei-mh/src/multiproject_probe.rs
  - crates/hodei-mh-supervisor/tests/socket_integration.rs
priority: medium
ordinal: 32000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P1 habilitador · F10/F13/F16 · TASK-19/20/21. 17 archivos probe/debug suman 248211 bytes de 803771 bytes Rust; no hay workflows en main auditado. Medir compilación/test por lane antes de añadir paralelismo. Establecer fast local, contracts Git/SQLite/IPC, smoke Docker y UAT ACP. Usar nextest solo si mejora con medición, limitar concurrencia según Bazzite. No usar un selector por paths que permita saltarse dependientes ni omitir fallos. Primera entrega: baseline y comando rápido útil, no esperar a todos los refactors.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Baseline frío y caliente con mismos commit/toolchain/host y varias repeticiones; registrar top 5 casos costosos, tiempo pared, CPU y llamadas/sandboxes; NOT_RUN si no disponible.
- [x] #2 Comando rápido local cubre crate afectado y dependientes relevantes; CI tiene fmt/clippy/test workspace y doctests; fallos no se convierten en PASS.
- [x] #3 Probes caros quedan en lane explícita y su reemplazo exige el mismo invariante en ruta productiva; usar fake ACP/HTTP para fallos, sin tokens ni sleeps arbitrarios.
- [x] #4 Comparar coste caliente equivalente; objetivo orientativo >=30%, no criterio para falsear cierre. Conservar smoke Docker/ACP en cambios pertinentes y release.
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Medir el baseline (compilación y pruebas frías y calientes) con el mismo commit, toolchain y host, y registrar los cinco casos más costosos con su coste en pared y CPU.
2. Entregar el gate local rápido: el crate afectado y los crates que dependen de él, resueltos por caminos explícitos o por diff contra la base (incluidos ficheros sin seguimiento), con los dependientes calculados por cierre transitivo inverso.
3. Separar el lane caro: los cuatro probes en un script explícito que es dueño de su puerto, comprueba que el servidor que responde es el suyo y falla si deja residuo.
4. Añadir la CI con el gate de workspace (fmt, clippy, suite y doctests) en cada push y pull request, y el lane de probes en main y a demanda, con la cadena de herramientas fijada.
5. Sustituir el coste de las rutas de fallo por dobles offline: un cliente Docker que no puede alcanzar ningún demonio y la política pura de la conversación ACP, probada con un agente simulado (sin tokens ni esperas).
6. Comparar el coste caliente equivalente de ambos gates y registrar la evidencia en la tarea.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
## Medición (AC1)

Entorno: commit f2c11f2, rustc 1.96.0, cargo 1.96.0, 12 CPUs, host compartido (load 5-20 de otros inquilinos durante la medición), por lo que las cifras absolutas se reportan con esa salvedad y las comparaciones son back-to-back en la misma máquina.

- Frío (release desde cero): 4:07.52 pared, user 1285.39 s, sys 75.00 s, maxrss 796 MB.
- Caliente (release sin cambios): 0:28.70 pared, user 99.11 s.
- Caliente (compilación de las pruebas, debug): 1:44.66 pared, user 597.16 s.
- Suite completa caliente, 3 repeticiones: 7.04 s / 1.13 s / 1.05 s. La primera abortó por el test de metadatos descrito más abajo, ya corregido.

Top 5 casos costosos (binario de pruebas, debug, caliente): 1) workspace 3.468 s, 2) promote 3.157 s, 3) lifecycle_server 0.798 s, 4) opensandbox 0.070 s, 5) socket_integration 0.056 s. Los dos primeros no esperan nada: arrancan procesos git reales sobre repositorios temporales.

Llamadas/sandboxes: el lane caro toca 6 sandboxes distintos por ronda (release-probe 5, profile-probe 1) y los borra; al terminar quedan 0 sandboxes y 0 contenedores.

## Entrega

- Gate local (scripts/fast-check.sh): el crate tocado y los que dependen de él, resueltos por caminos explícitos o por diff contra la base incluyendo ficheros sin seguimiento. Los dependientes salen del cierre transitivo inverso de cargo metadata. Un cambio en un fichero de workspace (Cargo.toml, Cargo.lock, .cargo/config.toml) selecciona todo el workspace; un cambio fuera del código compilado se declara como tal y solo comprueba el formato. Ningún paso se convierte en PASS: cada exit code se propaga.
- Lane caro (scripts/probes.sh): los cuatro probes contra Docker real y el lifecycle server. El lane es dueño de su puerto (lo comprueba antes de arrancar y verifica que el servidor que responde es el suyo), falla si deja residuo y no usa --offline, que en un runner sin caché impedía construir.
- CI (.github/workflows/ci.yml): gate de workspace (fmt, clippy -D warnings, suite y doctests) en cada push y pull request; lane de probes en main y a demanda. Toolchain fijada en rust-toolchain.toml, acciones fijadas por SHA, permisos de solo lectura y timeouts.
- Falsos offline: el cliente Docker de los tests de HTTP apunta a un fichero que no es un socket, así que ningún test puede alcanzar un demonio ni tirar de una imagen; y la política de la conversación ACP se extrajo a un módulo puro (crates/hodei-mh/src/acp_dialogue.rs) que se prueba con un agente simulado, sin tokens ni esperas.

## Revisión independiente (goldfish)

12 hallazgos (2 bloqueantes, 6 should-fix, 4 nits), todos resueltos antes de integrar: la clave de metadatos del test usaba un prefijo que no colisionaba con el reservado (el test tocaba Docker de verdad); --offline impedía la CI en caché fría; el selector por rutas no veía ficheros sin seguimiento ni cambios de workspace y podía devolver verde sin comprobar nada; la CI no usaba el lane local; AGENTS.md describía tres gates contradictorios; probes.sh arrancaba el servidor en el puerto por defecto mientras esperaba en otro; faltaban timeout, permisos, toolchain fijada y --locked; las rutas se partían por espacios; tower rompía la convención de dependencias del workspace; y el lane caro no es el gate de publicación.

## Verificación

- cargo fmt --all -- --check: limpio.
- cargo clippy --workspace --all-targets --locked -- -D warnings: limpio.
- cargo test --workspace --locked: 272 pruebas, 0 fallos (256 antes, 16 nuevas: 9 de la política ACP y 7 de la superficie HTTP).
- cargo test --workspace --doc --locked: limpio.
- Lane caro: 4/4 probes PASS (release 22.5 s, supervisor 2.1 s, profile 28.4 s, multiproject 25.1 s), 0 sandboxes y 0 contenedores al terminar.
- acp-probe en modo mock contra el agente simulado: 12/12 checks, all_passed=true, sandbox borrado.

## Coste comparable (AC4)

- Gate de workspace (caliente): 8040 / 8077 / 9602 ms, mediana 8077 ms.
- Gate local (caliente, hodei-mh-verify): 1248 / 1264 / 1023 ms, mediana 1248 ms.
- Gate local (caliente, hodei-mh-budget): 1212 / 1261 / 1086 ms, mediana 1212 ms.
- Ahorro del gate local sobre el gate de workspace: 84.5% (mediana 1248 ms frente a 8077 ms), por encima del objetivo orientativo del 30%.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
La verificación se pagaba entera en cada cambio: se construía, linteaba y probaba el workspace completo para cada edición, y los cuatro probes que necesitan Docker real no tenían lane propio. Ahora hay tres gates explícitos.

Entregado: scripts/fast-check.sh (el crate tocado y sus dependientes, resueltos por rutas o por diff contra la base incluyendo ficheros sin seguimiento, con los ficheros de workspace selecciona todo el workspace y los cambios fuera del código declarado, nunca un verde silencioso); scripts/probes.sh (los cuatro probes en lane caro, dueño de su puerto, con verificación de que el servidor que responde es el suyo y fallo si deja residuo); .github/workflows/ci.yml (gate de workspace en cada push y pull request, lane de probes en main y a demanda, toolchain fijada en rust-toolchain.toml y acciones fijadas por SHA); y los falsos offline de las rutas de fallo: un cliente Docker que no puede alcanzar ningún demonio para la superficie HTTP, y la política de la conversación ACP extraída a un módulo puro y probada con un agente simulado, sin tokens ni esperas.

Coste: el gate de workspace caliente cuesta 8077 ms de mediana y el local 1248 ms (84.5% menos, por encima del objetivo orientativo del 30%). El baseline frío de release es 4:07.52 y el top 5 de casos costosos queda registrado en la tarea.

Verificación: 272 pruebas del workspace en verde (16 nuevas), fmt y clippy limpios, doctests limpios, lane caro 4/4 PASS con 0 residuo, acp-probe en modo mock 12/12 checks y sandbox borrado. Una revisión independiente con contexto nuevo recorrió el trabajo: 12 hallazgos (2 bloqueantes, 6 should-fix, 4 nits), todos resueltos antes de integrar; entre ellos, un test que alcanzaba Docker de verdad por usar un prefijo reservado equivocado, el --offline que impedía la CI en caché fría, un selector por rutas que podía devolver verde sin comprobar nada, y el lane caro que arrancaba el servidor en un puerto distinto del que esperaba.

Riesgo y seguimiento: el gate local decide el alcance por análisis de dependencias, por lo que un cambio en un fichero de workspace o fuera del repositorio se trata como cambio global; la CI conserva el gate de workspace en cada pull request, así que la integración nunca depende de esa selección. La publicación sigue verificándose contra los assets publicados, no contra el lane de probes.
<!-- SECTION:FINAL_SUMMARY:END -->
