---
id: TASK-44
title: >-
  sup-events debe emitir JSONL estable (o la documentación debe reflejar el
  formato legible)
status: Done
assignee: []
created_date: '2026-11-20T14:45:00Z'
updated_date: '2026-11-20T15:24:00Z'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Worker entregó en 3e3be05 sobre 730297b. Merge e3e615b; fast-check --all PASS. Sin push/PR/Backlog edit del worker.
Diseño: helper puro render_event_line(&StructuredEvent, EmitFormat) -> String en supervisor_cli.rs:553; ADT EmitFormat::{Human,Jsonl} en :508; EmitChoice::{Format,Refused(String)} en :517; resolve_emit(args, stdout_is_terminal) -> EmitChoice en :529; rama de autodetección con std::io::stdout().is_terminal() en run_events (:583-584); readable tail deduplicado (:216); flags documentados en --help de main.rs:82.
MSRV 1.96 edition 2024: std::io::IsTerminal, cero dependencias nuevas (serde_json ya presente).
Contrato: JSONL reutiliza Serialize derivado de StructuredEvent (una fuente, jq-compatible); refusal ocurre ANTES de tocar el socket y exit 2, coherente con ParsedAction::Refused; "history lost" va por stderr en JSONL; fallo de serialización es un render_error explícito, nunca unwrap. El formato legible TTY queda byte-idéntico.
Pruebas: 7 offline (2x2 flags/TTY matriz completa + redonde JSONL sin 
 + readable tail idéntico al anterior + exclusión con causa en :1068); 3 integración real binary con Stdio::piped sin sleeps (events_reach_a_pipe_as_one_json_object_per_line :517, the_pretty_flag_answers_the_readable_tail_on_a_pipe :551, the_two_event_channels_asked_for_together_are_refused :580). wait_for_event ahora pasa --pretty (helper documentado).
Desviación autorizada: rama "TTY real + sin flags → pretty" se cubre solo en la prueba pura (sin script(1) ni sleeps).
Métricas: +311/−22 en 3 ficheros, 136 tests bins + 14 integración, clippy -D warnings limpio, cero solape con agent/policy/checkpoint/runner.
<!-- SECTION:NOTES:END -->
