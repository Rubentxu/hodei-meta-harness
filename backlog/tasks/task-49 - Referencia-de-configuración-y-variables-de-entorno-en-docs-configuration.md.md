---
id: TASK-49
title: Referencia de configuración y variables de entorno en docs/configuration.md
status: To Do
assignee: []
created_date: '2026-11-20T17:39:00Z'
labels: []
dependencies: []
references:
  - crates/hodei-mh/src/main.rs
  - crates/hodei-mh-coordinator/src/paths.rs
priority: high
type: docs
ordinal: 49000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Tabla de variables de entorno y su efecto con ancla al código: PORT, MAX_CONCURRENT_ENVIRONMENTS, XDG_STATE_HOME, HODEI_MH_CONTROL_TOKEN, HODEI_MH_PROXY_CHECK_URL, HODEI_MH_EGRESS_PROXY_CONTAINER, PROBE_IMAGES, --permission allow-once|deny, yolo_fallback. Distingue config del control plane, del supervisor y del carril de probes.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Documenta cada variable con su valor por defecto y su efecto
- [ ] #2 Ancla cada variable a la ruta real del código que la consume
- [ ] #3 Distingue config del control plane, del supervisor y de probes
- [ ] #4 No imprime secretos de ejemplo en claro
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 pipelinek validate pipeline.kts pasa
- [ ] #2 scripts/fast-check.sh --all pasa
<!-- DOD:END -->
