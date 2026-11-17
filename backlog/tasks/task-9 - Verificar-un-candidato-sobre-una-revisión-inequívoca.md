---
id: TASK-9
title: Verificar un candidato sobre una revisión inequívoca
status: To Do
assignee: []
created_date: '2026-11-17T14:20:00Z'
labels:
  - goalcycle
  - gc-mvp
  - gc-09
dependencies:
  - TASK-2
  - TASK-3
  - TASK-5
ordinal: 9000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
EvidenceSet vinculado a WorkRef/revision/candidate digest y política efectiva. Ejecutar comandos de verificación configurados en entorno aislado; preferir verificador limpio para pruebas sensibles a contaminación. Si no hay comando inferir propuesta desde instrucciones del proyecto y documentar evidencia proporcional. No convertir documentos o tests irrelevantes en gates administrativos.

Clave de incorporación: GC-09.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Exit status terminal y salida truncada/incompleta se distinguen; ausencia de resultado nunca es PASS.
- [ ] #2 Los resultados se vinculan al candidato exacto y al comando y entorno ejecutados; una modificación invalida evidencia afectada.
- [ ] #3 AC y DoD técnicos se verifican individualmente; NOT_RUN no marca cumplido y un commit vacío no acredita avance.
- [ ] #4 Un verificador limpio demuestra que un archivo no versionado o una dependencia ad hoc puede provocar un fallo que el entorno de desarrollo ocultaba.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Ejecutar pruebas reales de PASS FAIL NOT_RUN y cambio de candidato en repositorios de ejemplo.
<!-- DOD:END -->
