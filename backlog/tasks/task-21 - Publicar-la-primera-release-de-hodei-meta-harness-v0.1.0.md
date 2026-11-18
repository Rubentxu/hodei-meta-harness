---
id: TASK-21
title: Publicar la primera release de hodei-meta-harness (v0.1.0)
status: To Do
assignee: []
created_date: '2026-11-18T07:52:00Z'
labels: []
dependencies: []
ordinal: 21000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Publicar la release del proyecto desde main: binaries y bundle instalable por GitHub Releases, con verificación real del consumidor. Antes de publicar hay que cerrar un defecto de la herramienta de verificación: el multiproject-probe deja un sandbox huérfano por ejecución (delta no entra en la lista de limpieza) y su aserción del presupuesto global (preexisting + created == SLOTS) solo es válida cuando el host tiene menos de 2 entornos previos; con 3 o más acumulados produce un fallo FALSO del propio probe (medido: la 4a ejecución consecutiva falla). La invariante correcta es: el servidor admite exactamente SLOTS - preexisting creaciones (0 si ya está en el límite) y a continuación rechaza.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 El multiproject-probe limpia TODOS sus sandboxes (incluido delta) y su aserción del presupuesto es correcta para cualquier estado previo del host (preexisting + created == min(preexisting + admitted, SLOTS) y created == SLOTS - preexisting cuando preexisting < SLOTS; created == 0 cuando ya está en el límite)
- [ ] #2 Los cuatro probes se ejecutan contra la infra viva en 3 rondas SEGUIDAS sin dejar residuos en el host y sin falsos fallos
- [ ] #3 cargo test --workspace, fmt y clippy -D warnings limpios en el commit publicado
- [ ] #4 Release publicada en GitHub Releases con el bundle instalable, los dos binarios y SHA256SUMS, desde un commit identificado y verificado
- [ ] #5 Verificación de consumidor REAL: descarga del asset remoto, digest comparado, instalación fuera del checkout y ejecución de una operación real
<!-- AC:END -->
