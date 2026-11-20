---
id: TASK-54
title: Instalador curl | sh estilo get.docker.com en install.sh de raiz
status: Done
assignee: []
created_date: '2026-11-20T19:36:00Z'
updated_date: '2026-11-20T19:53:00Z'
labels: []
dependencies: []
priority: high
type: feature
ordinal: 54000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Crear install.sh en la raiz del repo: instalador POSIX sh (no bash-only) que detecta os/arch, resuelve la version (VERSION env o argumento; por defecto el release mas reciente via /latest/download/), descarga hodei-mh-v{version}-{os}-{arch}.tar.gz + SHA256SUMS desde GitHub Releases, VERIFICA el checksum antes de instalar (aborta si no casa o si falta), descomprime y ejecuta el install.sh del bundle con PREFIX. Debe fallar cerrado ante binario corrupto o platform no soportada, con mensaje accionable. Verificacion real: instalar v0.1.1 publicado en un prefijo efimero y ejecutar el binario con --version. No toca README.md (lo documenta TASK-53).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 POSIX sh puro: pasa con dash/sh, sin bashismas; rechaza platform desconocida con exito no-cero y mensaje
- [x] #2 Descarga v0.1.1 real, verifica SHA256SUMS e instala en prefijo efimero; el binario instalado reporta --version (evidencia pegada)
- [x] #3 Un byte corrupto en el tar.gz aborta la instalacion sin tocar el prefijo (verificado experimentalmente)
- [x] #4 El checksum del download falla y aborta antes de ejecutar nada del bundle
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 scripts/fast-check.sh --all pasa
- [x] #2 pipelinek validate pipeline.kts pasa
<!-- DOD:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-11-20T19:53:00Z

---

Evidencia (supervisor): rama @9d2327d (worktree aislado /tmp/wt54), solo install.sh (166 líneas, mode 755). AC1 re-verificada por el supervisor: shebang #!/bin/sh, dash -n OK, 0 bashismas al grep, platform desconocida fail-closed. AC2 re-verificada INDEPENDIENTEMENTE del worker: PREFIX=/tmp/prefix54v VERSION=0.1.1 sh install.sh → exit 0 y /tmp/prefix54v/bin/hodei-mh --version = 'hodei-mh 0.1.1' (descarga real de GitHub Releases). AC3/AC4: worker probó con mirrors locales (puertos 8541/8542) byte corrupto (mismatch 598098 vs 603f40, exit 1, prefijo intacto), tar corrupto con digest actualizado (abort 'not a readable tar.gz'), SHA256SUMS sin línea (abort antes de descargar) y bonus sin curl/sha256sum. El asset se parsea de SHA256SUMS, nunca se adivina. Gates: fast-check PASS + validate SUCCESSFUL (worker y post-merge --no-ff).

---
<!-- COMMENTS:END -->
