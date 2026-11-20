---
id: TASK-56
title: >-
  El install.sh muestra 'latest' en lugar de la versión resuelta al instalar sin
  VERSION
status: Done
assignee: []
created_date: '2026-11-20T21:11:00Z'
updated_date: '2026-11-20T23:29:00Z'
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
- [x] #1 La línea de bienvenida imprime la versión concreta resuelta del nombre del asset, nunca la palabra latest salvo en el prefijo URL
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 scripts/fast-check.sh --all pasa
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T23:29:00Z

---

Aceptado por el supervisor. Worker w56 ef57b59: diff exacto install.sh (1 línea printf 'resolved:' tras la resolución desde SHA256SUMS, antes del download; lógica de verificación intacta). Verificación independiente del supervisor en main (17a331a): mirror falso con hodei-mh-v9.9.9 → imprime 'resolved: hodei-mh v9.9.9', install exit 0, prefix escrito; bundle adulterado → checksum mismatch exit 1, prefix2 intacto (invariant digest-before-prefix vivo). bash -n y sh -n ok. scripts/fast-check.sh --all post-merge exit 0.

---
<!-- COMMENTS:END -->
