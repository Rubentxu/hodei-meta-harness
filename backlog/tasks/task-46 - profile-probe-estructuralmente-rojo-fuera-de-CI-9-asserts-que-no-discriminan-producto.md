---
id: TASK-46
title: >-
  profile-probe estructuralmente rojo fuera de CI: 9 asserts que no discriminan
  producto
status: To Do
assignee: []
created_date: '2026-11-20T16:31:00Z'
labels:
  - descubrimiento
dependencies: []
priority: low
ordinal: 46000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Descubrimiento preexistente del bisect P6. En 7d86143 y en 924daa1 los nueve asserts de profile-probe fallan con detail idéntico:
- user-image-present-locally-is-accepted: profile-run --image rust:1-bookworm (presente localmente, sin pin) es refused con digest mismatch que el probe considera falso.
- doctor-non-destructive-with-pinned-versions: doctor sale 1.
- rust-profile-real-build-with-limits-and-pin / jvm-profile-real-build-with-limits-and-pin: build exit 1; missing=[exit code, all_succeeded, build output, observed cpu/memory limits, build command, compiler command]; observed_limits=null; image_digest=null.
- caches-isolated-per-project: dos builds reales de dos proyectos con cache dirs distintos.
- credentials-excluded-from-commands-and-records: requiere una credencial random inyectada en el env del proceso; el probe la introduce, el producto no la filtra donde el probe espera.
- sandbox-egress-measured-from-inside: proxy reachable:false medido desde el sandbox.
- observed-image-digest-recorded: requested pin vacío; digest observado vacío.
- release-goes-through-the-idle-gate-with-a-durable-export: cleanup vacío, candidate export null digest.
Preflight SÍ pasó (imágenes presentes, proxy hodei-uat-proxy4 host-reachable por bridge 172.17.0.4:8888, Backlog CLI 1.55.3), luego no es prerequisite gate. Es divergencia real entre los asserts del probe y el comportamiento del producto en un host sin la configuración de CI (TASK-37 cerró la lane en CI, no localmente).
Acción: o los asserts del probe deben relajarse para hosts locales (configurar per-assert expectations por entorno), o profile-run necesita un modo local honesto, o la lane local debe declarar explícitamente "only CI" con motivo.
Referencia: /tmp/uat-task34/bisect7d/probes-out/profile-probe-1.json y /tmp/uat-task34/evidence6/probes-out/profile-probe-1.json.
<!-- SECTION:DESCRIPTION:END -->
