---
id: TASK-49
title: Referencia de configuración y variables de entorno en docs/configuration.md
status: Done
assignee: []
created_date: '2026-11-20T17:39:00Z'
updated_date: '2026-11-20T18:13:00Z'
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
- [x] #1 Documenta cada variable con su valor por defecto y su efecto
- [x] #2 Ancla cada variable a la ruta real del código que la consume
- [x] #3 Distingue config del control plane, del supervisor y de probes
- [x] #4 No imprime secretos de ejemplo en claro
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 pipelinek validate pipeline.kts pasa
- [x] #2 scripts/fast-check.sh --all pasa
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T18:13:00Z

---

Evidencia (supervisor): rama task/task-49-configuration @b7601bb (base 1014a4b, worktree aislado /tmp/wt49 tras incidente de árbol compartido), solo docs/configuration.md (218 líneas). Gate worker: fast-check --all PASS, pipelinek validate SUCCESSFUL. Merge --no-ff @ffb9b40; post-merge gates PASS. Supervisor spot-check: MAX_CONCURRENT_ENVIRONMENTS anclado a la ruta real lifecycle-server/src/main.rs:206 (corrección del brief), PROBE_IMAGES en scripts/probes.sh, yolo_fallback documentado como no configurable, tokens solo como export placeholder. Verificación confianza: cada variable anclada a código real confirmado por el worker; ninguno inventado.

---
<!-- COMMENTS:END -->
