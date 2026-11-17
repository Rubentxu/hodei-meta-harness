---
id: TASK-12
title: Aplicar presupuestos agregados y proteger el host Bazzite
status: To Do
assignee: []
created_date: '2026-11-17T14:21:00Z'
labels:
  - goalcycle
  - gc-mvp
  - gc-12
dependencies:
  - TASK-1
  - TASK-4
ordinal: 12000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
ResourceVector CPU/RAM/disco y admisión simple sin DRF. Límites efectivos del backend, cgroups/hipervisor y presión del host; no confundir quota con cores reservados. Presupuestos por proyecto ajustables; contar overhead, nodos/servicios auxiliares y cargas no gestionadas. Requerir límites demostrables antes de YOLO sandbox-first.

Clave de incorporación: GC-12.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Una solicitud que supera presupuesto espera capacidad sin crear sandbox; el presupuesto incluye entornos en aprovisionamiento.
- [ ] #2 Inspección del runtime demuestra límites CPU y memoria efectivos y contabiliza auxiliares relevantes; no basta el valor solicitado por API.
- [ ] #3 PSI o señal de recursos crítica suspende nuevas admisiones y aplica backoff a OOM sin elevar presupuesto automáticamente.
- [ ] #4 En host de 64 CPU lógicas y RAM medida se conserva margen razonable configurable; las metas 48 CPU/64 GiB son hipótesis no garantías.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Pruebas bajo presión controlada con límites medidos y cancelación del workload de prueba.
<!-- DOD:END -->
