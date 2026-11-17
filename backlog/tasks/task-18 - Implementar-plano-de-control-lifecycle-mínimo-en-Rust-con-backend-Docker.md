---
id: TASK-18
title: Implementar plano de control lifecycle mínimo en Rust con backend Docker
status: Done
assignee: []
created_date: '2026-11-17T14:36:00Z'
updated_date: '2026-11-17T15:01:00Z'
labels:
  - gc-18
  - hodei-mh
dependencies: []
ordinal: 18000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Port Rust del servidor OpenSandbox (FastAPI) limitado al subconjunto del contrato OpenAPI que consume hodei-meta-harness (hodei-mh): crear/listar/obtener/borrar sandboxes, resolución de endpoints con execd accesible y health. Sin pause/resume, templates, snapshots, credential-vault ni Kubernetes: esos endpoints responden 501 honesto. La composición Docker (imagen execd, puertos, hardening) replica la del servidor de referencia. Clave de incorporación: GC-18.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Crear un sandbox desde una imagen pública deja un contenedor con execd operativo; GET /v1/sandboxes/{id} refleja estado y metadatos. El backend Docker de referencia no genera token de execd (verificado en el código del server local)
- [x] #2 La resolución de endpoints devuelve host:port publicados que sirven execd real; DELETE elimina contenedor y registros sin huérfanos
- [x] #3 Los endpoints no implementados del contrato responden 501 con código de error explícito, no silenciosos
- [x] #4 La composición Docker aplica el hardening del servidor de referencia (capabilities dropeadas, no_new_privileges, pids_limit) verificable inspeccionando el contenedor creado
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Port del create-path del server referencia (docker_service.py+container_ops.py+runtime.py): contenedor único sandbox-{id} con execd INYECTADO (extraer /execd+/bootstrap.sh del contenedor temporal execd-image via get_archive; put_archive a /opt/opensandbox), entrypoint=/opt/opensandbox/bootstrap.sh, cmd=entrypoint usuario (obligatorio con imagen), env config+request+OPENSANDBOX_ID+EXECD_INIT, labels opensandbox.io/id + metadata + expires/manual, HostConfig: bridge, no-new-privileges, cap_drop, pids_limit, mem/nano_cpus, ports 44772+8080 publicados (Docker auto-assign, leer mapeo real via inspect; divergencia documentada vs pre-alloc 40000-60000), retry conflicto puertos, cleanup on failure. Endpoints: Docker local SIN token execd. MVP: create/list/get/delete/endpoints/ping; 501 honesto para el resto. Estado derivado de labels Docker (sin SQLite).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
EVIDENCIA E2E (Docker real 29.1.3): server hodei-mh-lifecycle-server en :8081. POST /v1/sandboxes (image opensandbox/code-interpreter:v1.1.0, entrypoint sleep infinity, timeout 600) -> 200 en 4.9s, id 58d8b75f-6708-4ac6-b2e7-8fc0b3300e7f, estado Running. GET /v1/sandboxes/{id} -> Running + metadata probe=task-18-e2e. Endpoint 44772 resuelto -> 127.0.0.1:32769; execd /ping OK (liveness). Hardening inspect: cap_drop 9 caps, no-new-privileges=true, pids_limit 4096; entrypoint=/opt/opensandbox/bootstrap.sh cmd=sleep infinity; labels opensandbox.io/id + expires-at + metadata-keys correctos. DELETE -> 200; docker ps sin sandbox ni execd-cache (cero huérfanos); listado 0 items. GET /v1/templates -> 501 NOT_IMPLEMENTED honesto.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Port Rust del lifecycle server verificado E2E: create 4.9s con execd operativo vía endpoint resuelto, hardening idéntico al server de referencia (9 caps dropeadas, no-new-privileges, pids 4096), delete sin huérfanos, 501 honesto en rutas no implementadas. 20 unit tests offline + clippy -D warnings + fmt. Backend Docker sin token execd (paridad con referencia local).
<!-- SECTION:FINAL_SUMMARY:END -->
