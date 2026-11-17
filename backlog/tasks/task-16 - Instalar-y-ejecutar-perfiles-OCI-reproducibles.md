---
id: TASK-16
title: Instalar y ejecutar perfiles OCI reproducibles
status: To Do
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-17T14:41:00Z'
labels:
  - gc-mvp
  - gc-16
  - hodei-mh
dependencies:
  - TASK-1
  - TASK-12
ordinal: 16000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Distribución ligera Rust con servicio usuario opcional. OpenSandbox externo elegido y autenticado. Imágenes base y perfil Rust/JVM mínimos con versiones y digests; aceptar imágenes de usuario. Autenticación API/suscripción evaluada; no copiar HOME ni agente SSH ni socket Docker. Egress restringido de acuerdo al backend. Integrar no replicar CLI OpenSandbox.

Clave de incorporación: GC-16.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Documentación permite instalar supervisor y lanzar perfil soportado desde un host limpio con versiones fijadas y doctor no destructivo.
- [ ] #2 Un perfil Rust y otro JVM ejecutan un build real con límites solicitados y observados; caches no contaminan entre proyectos.
- [ ] #3 Credenciales se entregan por mecanismo autorizado de alcance mínimo y se excluyen de logs/state/HANDOFF/snapshots; indicar limitaciones de suscripción.
- [ ] #4 Si OpenSandbox o aislamiento seleccionado no está disponible se informa y no se ejecuta YOLO nativo como fallback.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Instalación limpia y dos builds reales; registrar SHA y comandos.
<!-- DOD:END -->
