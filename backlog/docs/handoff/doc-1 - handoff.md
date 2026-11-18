---
id: doc-1
title: handoff
type: other
created_date: '2026-11-18T21:38:00Z'
updated_date: '2026-11-18T21:38:00Z'
---
# Handoff — TASK-25 (frontera de confianza del lifecycle y del execd)

## Qué se entregó

La frontera de confianza del plano de control y del plano de ejecución queda
cerrada y **medida** contra Docker real (29.1.3), no supuesta:

- **Red por sandbox.** Cada entorno vive en un puente interno propio
  (`internal=true`, IPv6 apagado, `gateway_mode_ipv4=isolated`), con la
  subred asignada por Docker **verificada** contra el rango que el operador
  declaró (`SANDBOX_ADDRESS_POOL`, por defecto `172.16.0.0/12`, el mismo
  rango que acepta el proxy). Una subred fuera de ese rango se rechaza.
- **El proxy es el único camino de salida.** Se adjunta por su id de Docker
  inmutable, bajo el alias `hodei-proxy`. El proxy habla `CONNECT` de HTTPS
  contra una lista exacta de destinos, valida que la respuesta DNS sea global
  y acepta clientes solo desde prefijos de al menos /12.
- **La credencial es la barrera, no la topología.** El execd escucha en
  `0.0.0.0:44772` de la red del propio sandbox: es alcanzable desde dentro y
  por eso su credencial por sandbox es lo que protege el plano de ejecución.
  El servidor acuña la credencial, la inyecta en el entorno del contenedor,
  la lee de vuelta **por nombre exacto**, la **compara con la que acuñó** y
  la entrega en el contrato del endpoint. El execd rechaza lo que no sea su
  credencial, incluso desde dentro del sandbox (401).
- **El entorno pertenece al operador.** La petición no puede escribir en los
  espacios `EXECD_` ni `OPENSANDBOX_`; **la imagen** tampoco: el daemon fusiona
  el `ENV` de la imagen con el del contenedor, así que el servidor inspecciona
  la imagen y la rechaza si declara ese espacio. Un nombre que empieza por
  `EXECD_ACCESS_TOKEN=` no es la credencial: se lee por nombre exacto y una
  entrada ambigua no es credencial.
- **Fallo cerrado.** Sin token de control el binario no arranca; una política
  desconocida, un rango de direcciones mal formado o la ausencia de límites
  obligatorios (memoria, CPU, pids) impiden servir nada. Los puertos se
  publican en el loopback; una dirección no-loopback exige aceptación
  explícita. El endpoint resuelto usa el origen configurado, no la cabecera
  `Host` de la petición.

## La red que se quedaba en el host

La red se crea **antes** del contenedor, así que una creación que falla — o que
se abandona porque su cliente se rindió — dejaba un puente en el host que
nadie era dueño. En este ciclo se cerró ese hueco con cuatro medidas que se
verificaron contra Docker:

1. **La creación sostiene su red hasta terminar.** Un guard (RAII) toma la red
   creada y la suelta con la creación: por lo que ocurra —fallo, cancelación o
   abandono— la red se va con ella. Hay un test de Docker que lo comprueba
   (`an_abandoned_creation_leaves_no_network`).
2. **El barrido no toca las creaciones en vuelo.** La edad de la red no dice
   nada de cuánto tarda la creación, así que las creaciones en vuelo se
   registran y el barrido nunca les quita la red. El margen de dos minutos es
   solo la última línea para las redes que ya no pertenecen a ninguna
   creación, porque el proceso que las creó ya no está.
3. **El rollback reporta sus fallos** en vez de descartarlos: una red que
   nadie posee tiene que ser visible.
4. **Cada reintento de borrado desconecta de verdad al proxy**, que es
   exactamente lo que hace que Docker se niegue a borrar la red.
5. **El barrido** retira las redes huérfanas a los **dos** minutos (antes:
   diez), y la liberación de un sandbox se registra con lo que quitó.

## Cómo se verificó

- `cargo test --workspace` en verde (64 pruebas del lifecycle-server, 41 de
  hodei-mh y el resto de los crates), incluidos los dos tests de Docker
  (`orphan_network_cleanup_uses_original_proxy_after_reconfiguration` y
  `an_abandoned_creation_leaves_no_network`), ejecutados con `-- --ignored`.
- `cargo fmt --all --check` y `cargo clippy --all-targets` limpios.
- `scripts/fast-check.sh --all` en verde.
- 12/12 pruebas del proxy de egreso (`scripts/egress-proxy.py`).
- **Pista de sondas** (`scripts/probes.sh --rounds 2`): las 12 ejecuciones de
  sonda en verde (release, supervisor, profile, multiproject, verify y trust)
  y **cero residuo** en el host (`sandboxes=0 containers=0 networks=0`),
  **repetida dos veces** con el código final.
- `hodei-mh trust-probe`: 23 comprobaciones distintas (36 registros, porque
  algunas propiedades se miden en más de un sandbox) medidas desde dentro y
  desde fuera de un sandbox real; veredicto `PASS` y `not_measured` vacío. Un
  check no medido se reporta `NOT_MEASURED` y **hace fallar la sonda**: la
  ausencia de evidencia no es evidencia. El informe no lleva credenciales, y
  los entornos que crea se borran antes de terminar —incluido el que nunca
  recibió identificador, recuperado por la marca que dejó en los metadatos.

## Límites conocidos (deliberados, no accidentales)

- El proxy de egreso es una **decisión del operador**: el servidor exige que
  `HODEI_MH_EGRESS_PROXY_CONTAINER` nombre un contenedor en marcha y se niega
  a crear un entorno sin él. El servidor no implementa el proxy.
- `SANDBOX_NETWORK` y `SANDBOX_SUBNET` se rechazan: las redes son por sandbox
  y Docker asigna la subred. El rango esperado se declara con
  `SANDBOX_ADDRESS_POOL`.
- El presupuesto global de entornos cuenta y reserva sin atomicidad: pertenece
  a TASK-27.
- La descarga de imágenes ocurre en el host, fuera del proxy restringido.
- El listener del plano de control no acota el tamaño del cuerpo ni fija un
  timeout de lectura por petición.
- El contrato de entorno del execd vive en la imagen `opensandbox/execd:v1.1.0`,
  que no está en el repositorio: la reserva de los espacios
  `EXECD_`/`OPENSANDBOX_` cierra lo que el código ve, no lo que el execd haga
  con variables que nadie conoce.
- La sonda mide UDP hacia resolvers públicos: una respuesta (incluso un
  rechazo) demuestra el camino; la ausencia de respuesta no se puede
  distinguir de una caída, y se reporta como `NOT_MEASURED`.

## Estado del tablero

TASK-25 queda `Done`. Sus dependientes (TASK-34, TASK-35, TASK-36) ya no
esperan a esta tarea: el supervisor y el bucle de objetivos se apoyan en el
plano de control autenticado que aquí se cerró.
