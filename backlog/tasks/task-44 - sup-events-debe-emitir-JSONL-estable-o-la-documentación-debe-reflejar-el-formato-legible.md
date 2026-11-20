---
id: TASK-44
title: >-
  sup-events debe emitir JSONL estable (o la documentación debe reflejar el
  formato legible)
status: To Do
assignee: []
created_date: '2026-11-20T14:45:00Z'
labels:
  - descubrimiento
dependencies: []
priority: low
ordinal: 44000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
El driver UAT esperaba JSONL (un JSON por línea) desde hodei-mh sup-events --since 0, pero el CLI imprime un formato legible multi-línea. Para poder consumir eventos desde scripts/probes.sh y desde oráculos de UAT deterministas hace falta un canal JSONL: o bien --jsonl explícito, o que --since 0 emita JSONL cuando stdout no es TTY. Mientras no exista, las evidencias UAT tienen que depender de un lector de socket ad-hoc (tools5/events_jsonl.py en la run 5), lo cual es una dependencia de prueba paralela que AGENTS.md no admite como sustituto del contrato.
Evidencia: /tmp/uat-task34/evidence5/*_events.err vacío + comentario en el driver; el fallback events_jsonl.py vive en tools5/.
Requisito observable: sup-events documentado, con --jsonl o autodetección, probado offline sobre un fake event stream.
<!-- SECTION:DESCRIPTION:END -->
