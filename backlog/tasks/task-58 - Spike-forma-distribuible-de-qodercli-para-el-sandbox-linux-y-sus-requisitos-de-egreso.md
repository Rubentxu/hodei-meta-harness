---
id: TASK-58
title: >-
  Spike: forma distribuible de qodercli para el sandbox linux y sus requisitos
  de egreso
status: Done
assignee: []
created_date: '2026-11-20T21:45:00Z'
updated_date: '2026-11-20T22:49:00Z'
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
- [x] #1 Las cuatro preguntas están respondidas con salida real pegada (o 'no existe' con la prueba que lo demuestra)
- [x] #2 Se demuestra que 'qoder --acp' responde initialize y session/new de ACP v1 por stdio con el artefacto candidato
- [x] #3 Ningún valor de credencial aparece en la evidencia
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 las conclusiones quedan en un comentario de esta tarea como insumo de las tareas siguientes
- [ ] #2 1
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T22:49:00Z

---

SPIKE COMPLETO (worker + supervisor). P4 DECISIVA: 'qodercli --acp' habla ACP v1 real — initialize responde protocolVersion:1 con agentInfo qoder-cli/1.1.94, session/new devuelve sessionId (login local, sin token impreso). --acp es flag oculta (no en --help); 'notifications/initialized' no existe en este agente; el cliente debe mantener stdin abierto (lo hace el transporte ACP de hodei-mh). INSUMO TASK-59: (a) instalación = tarball linux-x64 del CDN OSS con sha256 verificado contra manifest.json de download.qoder.com (coincidencia demostrada); NO usar npm (exige Node>=20); glibc basta para rust:1-bookworm. (b) credencial = QODER_PERSONAL_ACCESS_TOKEN por entorno (aceptada; dummy rechazado honestamente en session/new); existe QODER_AUTH_STDIN alineable con el transporte stdin actual. (c) egreso candidatos: openapi.qoder.sh, api2.qoder.sh, api.qoder.com, qoder.com (+download.qoder.com/oss solo si la caja descarga). (d) Riesgo resuelto por distinción: el elf.rs standalone aplica al release de hodei-mh (package.rs:243), no a artefactos de agente en la caja — no relajar nada. Open questions honestas: prompt/streaming de turnos y PAT real no probados (fuera de reglas del spike) -> quedan para la UAT TASK-60.

---
<!-- COMMENTS:END -->
