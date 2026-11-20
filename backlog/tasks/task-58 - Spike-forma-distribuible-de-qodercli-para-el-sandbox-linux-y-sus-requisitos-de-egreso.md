---
id: TASK-58
title: >-
  Spike: forma distribuible de qodercli para el sandbox linux y sus requisitos
  de egreso
status: To Do
assignee: []
created_date: '2026-11-20T21:45:00Z'
updated_date: '2026-11-20T21:46:00Z'
labels: []
dependencies: []
priority: high
type: spike
ordinal: 58000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Responder con evidencia, sin tocar código productivo: (1) cómo se obtiene un qodercli ejecutable en linux-x86_64 dentro de una caja sin red (binario estático, npm tarball u otro) y qué dependencias dinámicas exige (ldd/readelf -d); (2) qué variable de entorno autentica la sesión headless/ACP con PAT (QODER_PERSONAL_ACCESS_TOKEN u otra real) sin imprimir valores; (3) qué dominios de API necesita el proxy de egreso para que 'qoder --acp' funcione desde una caja aislada; (4) qué versión exacta soporta el modo ACP. Formato: comentario estructurado en esta tarea con comandos y salidas reales. Prohibido: publicar tokens, asumir sin probar (probar 'qoder --acp' con un initialize JSON-RPC por stdin contra el binario candidato).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Las cuatro preguntas están respondidas con salida real pegada (o 'no existe' con la prueba que lo demuestra)
- [ ] #2 Se demuestra que 'qoder --acp' responde initialize y session/new de ACP v1 por stdio con el artefacto candidato
- [ ] #3 Ningún valor de credencial aparece en la evidencia
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 las conclusiones quedan en un comentario de esta tarea como insumo de las tareas siguientes
<!-- DOD:END -->
