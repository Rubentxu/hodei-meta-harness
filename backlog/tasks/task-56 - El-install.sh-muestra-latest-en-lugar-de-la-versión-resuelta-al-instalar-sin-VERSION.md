---
id: TASK-56
title: >-
  El install.sh muestra 'latest' en lugar de la versión resuelta al instalar sin
  VERSION
status: To Do
assignee: []
created_date: '2026-11-20T21:11:00Z'
labels: []
dependencies: []
priority: medium
type: chore
ordinal: 56000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Cosmético descubierto en el consumer check de v0.1.2: cuando no se fija VERSION, la línea de bienvenida imprime 'hodei-mh latest (linux-x86_64) -> ...' aunque el installer SÍ resuelve la versión concreta al parsear SHA256SUMS (el asset descargado es hodei-mh-v0.1.2-...). Debería imprimir la versión resuelta del nombre del asset. No afecta a la verificación de integridad ni a la instalación: solo al mensaje.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 La línea de bienvenida imprime la versión concreta resuelta del nombre del asset, nunca la palabra latest salvo en el prefijo URL
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 scripts/fast-check.sh --all pasa
<!-- DOD:END -->
