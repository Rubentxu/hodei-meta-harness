---
id: TASK-8
title: Resolver permisos y formularios según capacidades y autorización
status: Done
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-17T21:31:00Z'
labels:
  - gc-mvp
  - gc-08
  - hodei-mh
dependencies:
  - TASK-1
  - TASK-5
ordinal: 8000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
InteractionResolver propio independiente de Continuum. Usar permisos nativos/protocolo y reglas tipadas antes de razonamiento. ACP v1 respetuoso de revisión humana cuando sea obligatoria. Plugin nativo solo si el proceso ACP verdaderamente puede cargarlo o exponer un canal complementario; no fingir que funciona. Respuesta atribuida a automatización; scope e identidad validados.

Clave de incorporación: GC-08.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Permisos ordinarios y preguntas técnicas compatibles se resuelven automáticamente sin ventana de consentimiento por cada decisión.
- [x] #2 Respuestas estructuradas validan opciones y cardinalidad; un resultado tardío tras Stop no se aplica.
- [x] #3 Callbacks ACP filesystem/terminal quedan deshabilitados o dirigidos al sandbox; no ejecutan herramientas en el host involuntariamente.
- [x] #4 Interacción no soportada o autenticación humana produce motivo visible y yield; otra tarea independiente sigue sin eludir restricciones del proveedor.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Probar formularios reales soportados y carreras de revocación; registrar limitaciones NOT_RUN sin afirmar pleno soporte.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Evidencia TASK-8 (rama task/task-8-permissions): crate hodei-mh-interact — InteractionResolver propio (independiente de Continuum) con reglas TIPADAS antes que razonamiento: (1) request.rs — InteractionRequest {Permission{tool,ToolKind,options}|Form{fields,cardinality}|Question}, ToolKind::from_tool_name clasificación por reglas (Unknown nunca cae en default optimista). (2) policy.rs — InteractionPolicy::sandbox_first() (Read auto, Write/Execute/Terminal SANDBOX, Network/Unknown revisión) + host_client_capabilities() que NUNCA anuncia fs/terminal del host (los callbacks ACP de fs/terminal no pueden dispararse en el host: AC3 por construcción). (3) resolver.rs — resolve() puro: AutoGranted{source} CON ATRIBUCIÓN (AC1: grants de automatización etiquetados, sin ventana por decisión), RoutedToSandbox (AC3), HumanRequired/Unsupported con motivo visible y yield (AC4). (4) response.rs — validate() con cardinalidad 1 exacta y opciones ofrecidas solamente (AC2); form fields con cardinalidad por campo. (5) lifecycle.rs — Pending acepta, Stopped DESCARTA (LateDiscarded: resultado tardío tras Stop no se aplica, AC2). 11 tests unitarios. Probe E2E interact-probe contra AGENTE REAL (claude-code-acp + modelo real vía gateway, misma infra del leg TASK-1): prompt de LECTURA completado SIN ninguna ventana de consentimiento (el agente real no pidió permiso para leer; contenido del fixture devuelto verbatim), prompt de ESCRITURA → session/request_permission clasificado RoutedToSandbox y respondido {outcome:selected, optionId:<allow_once del request real>} VALIDADO contra las opciones exactas ofrecidas por el agente (cardinalidad 1) → fichero creado DENTRO del sandbox verificado por cat ('sandbox-write-ok'), el host nunca ejecutó herramienta alguna; clientCapabilities enviadas al agente: {} (sin fs/terminal). Limitaciones NOT_RUN registradas honestamente (DoD): carrera real de revocación con agente real no provocable con el CLI actual (late-discard verificado por unidad); formularios estructurados con el agente real NOT_RUN (el CLI solo usa request_permission; el soporte de Form/Question está verificado por unidad y queda listo para agentes que lo usen). 8/8 checks ×2 ejecuciones (~38s). Gates: fmt/clippy -D warnings/test workspace 143 passed 0 failed.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-8 completa: permisos y formularios resueltos por reglas tipadas con sandbox-first. El InteractionResolver propio clasifica cada petición del agente (Permission/Form/Question con ToolKind por reglas) y decide SIN razonamiento: lecturas ordinarias auto-concedidas CON atribución (el agente real completó la lectura sin ventana alguna), writes/executions/terminal SIEMPRE enrutados al sandbox (el host anuncia capacidades vacías — fs/terminal no pueden delegarse al host, probado con el agente real: el Write pidió permiso, se respondió validado contra las opciones exactas ofrecidas y el fichero nació dentro del sandbox), Network/Unknown exigen revisión visible con motivo, y las respuestas tardías tras Stop se descartan (LateDiscarded). Formularios con cardinalidad por campo soportados y validados. Limitaciones NOT_RUN documentadas sin afirmar pleno soporte. Verificación: 11 tests unitarios + probe E2E 8/8 ×2 con agente real; gates workspace 143 tests en verde.
<!-- SECTION:FINAL_SUMMARY:END -->
