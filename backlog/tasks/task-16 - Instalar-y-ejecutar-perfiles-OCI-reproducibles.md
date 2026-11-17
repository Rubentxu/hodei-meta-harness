---
id: TASK-16
title: Instalar y ejecutar perfiles OCI reproducibles
status: Done
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-17T23:03:00Z'
labels:
  - gc-mvp
  - gc-16
  - hodei-mh
dependencies:
  - TASK-1
  - TASK-12
ordinal: 16000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Distribución ligera Rust con servicio usuario opcional. OpenSandbox externo elegido y autenticado. Imágenes base y perfil Rust/JVM mínimos con versiones y digests; aceptar imágenes de usuario. Autenticación API/suscripción evaluada; no copiar HOME ni agente SSH ni socket Docker. Egress restringido de acuerdo al backend. Integrar no replicar CLI OpenSandbox.

Clave de incorporación: GC-16.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Documentación permite instalar supervisor y lanzar perfil soportado desde un host limpio con versiones fijadas y doctor no destructivo.
- [x] #2 Un perfil Rust y otro JVM ejecutan un build real con límites solicitados y observados; caches no contaminan entre proyectos.
- [x] #3 Credenciales se entregan por mecanismo autorizado de alcance mínimo y se excluyen de logs/state/HANDOFF/snapshots; indicar limitaciones de suscripción.
- [x] #4 Si OpenSandbox o aislamiento seleccionado no está disponible se informa y no se ejecuta YOLO nativo como fallback.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Instalación limpia y dos builds reales; registrar SHA y comandos.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
EVIDENCIA (profile-probe, 6 checks ALL=True en 24.6s, builds REALES): AC1 docs/profiles.md con instalación limpia (docker+cargo, supervisor, versiones fijadas por workspace) y tablas de perfiles con DIGESTS REALES del registry (rust:1-bookworm@sha256:ade39a71..., code-interpreter@sha256:133a3c17...); doctor NO destructivo verificado: checks read-only (pin, digest local vs pinado, egress, lifecycle ready, limitaciones de suscripción) sin crear/borrar nada, PROCEED con sha256. AC2 build Rust REAL con dependencia de registry (serde_json) vía proxy de egress restringido (cargo build+run OK, 'rust-build-ok'), límites pedidos cpu=1/mem=1Gi observados DESDE DENTRO (cpu.max=100000 100000, memory.max=1073741824); build JVM REAL con javac Main+Util interdependientes y java -cp (jvm-build-ok from-util); caches por-proyecto aislados (token sha256 del path absoluto: dos builds reales de dos proyectos con cache_dir distintos y deterministas). AC3 comandos de build SIN secretos (secret_leak_positions vacío en el record), canal de credenciales = env del proceso que las necesita, scrub visible ([redacted]), limitaciones de suscripción en el informe del doctor. AC4 imagen inexistente -> REFUSE informado (missing + yolo_fallback:false) SIN crear sandbox y SIN fallback nativo YOLO; digest mismatch y egress caído también refusan (tests unit). Núcleo puro: crates/hodei-mh-profiles (profile/gate/build_plan/credentials/doctor, 14 tests). Subcomandos producto: doctor, profile-run (record JSON con SHA+comandos+outputs para archivo por build). DoD: instalación limpia documentada + DOS builds reales ejecutados con SHA y comandos registrados en el record JSON de cada profile-run. Fixes de producto: egress check /dev/tcp host:port -> host/port; staging con 3 reintentos acotados (conexión execd temprana falla ~1s tras create). Gates: fmt limpio, clippy workspace -D warnings limpio, cargo test workspace 193 passed 0 failed.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-16 completa: perfiles OCI reproducibles para hodei-meta-harness. Núcleo puro nuevo crates/hodei-mh-profiles: profile (ImagePin con digest obligatorio para perfiles producto, imágenes de usuario aceptadas con nota honesta), gate (AvailabilityGate -> Proceed|Refuse{missing}: jamás fallback nativo YOLO), build_plan (comandos por perfil con cache por-proyecto = sha256 del path absoluto, nunca compartido), credentials (canal autorizado único = env del proceso, scrub de secretos, detección de leaks en comandos, SubscriptionNote explícita), doctor (plan de checks NO destructivos con decisión pura). Producto en el bin: doctor (informe read-only + gate) y profile-run (gate -> sandbox con imagen pinada y límites pedidos -> límites observados por cgroups -> build real -> record JSON con SHA, comandos y outputs). Builds reales verificados: Rust con dependencia de registry vía proxy CONNECT de egress restringido y JVM con clases interdependientes. docs/profiles.md permite instalación limpia con versiones y digests fijados. 193 tests workspace, fmt/clippy -D warnings limpios.
<!-- SECTION:FINAL_SUMMARY:END -->
