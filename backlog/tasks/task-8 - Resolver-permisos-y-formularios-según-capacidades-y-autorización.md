---
id: TASK-8
title: Resolver permisos y formularios según capacidades y autorización
status: To Do
assignee: []
created_date: '2026-11-17T14:20:00Z'
labels:
  - goalcycle
  - gc-mvp
  - gc-08
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
- [ ] #1 Permisos ordinarios y preguntas técnicas compatibles se resuelven automáticamente sin ventana de consentimiento por cada decisión.
- [ ] #2 Respuestas estructuradas validan opciones y cardinalidad; un resultado tardío tras Stop no se aplica.
- [ ] #3 Callbacks ACP filesystem/terminal quedan deshabilitados o dirigidos al sandbox; no ejecutan herramientas en el host involuntariamente.
- [ ] #4 Interacción no soportada o autenticación humana produce motivo visible y yield; otra tarea independiente sigue sin eludir restricciones del proveedor.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Probar formularios reales soportados y carreras de revocación; registrar limitaciones NOT_RUN sin afirmar pleno soporte.
<!-- DOD:END -->
