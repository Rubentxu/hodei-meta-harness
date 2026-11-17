---
id: TASK-12
title: Aplicar presupuestos agregados y proteger el host Bazzite
status: Done
assignee: []
created_date: '2026-11-17T14:21:00Z'
updated_date: '2026-11-17T22:12:00Z'
labels:
  - gc-mvp
  - gc-12
  - hodei-mh
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
- [x] #1 Una solicitud que supera presupuesto espera capacidad sin crear sandbox; el presupuesto incluye entornos en aprovisionamiento.
- [x] #2 Inspección del runtime demuestra límites CPU y memoria efectivos y contabiliza auxiliares relevantes; no basta el valor solicitado por API.
- [x] #3 PSI o señal de recursos crítica suspende nuevas admisiones y aplica backoff a OOM sin elevar presupuesto automáticamente.
- [x] #4 En host de 64 CPU lógicas y RAM medida se conserva margen razonable configurable; las metas 48 CPU/64 GiB son hipótesis no garantías.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Pruebas bajo presión controlada con límites medidos y cancelación del workload de prueba.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Evidencia TASK-12 (rama task/task-12-budgets): crate hodei-mh-budget — (1) vector.rs: ResourceVector CPU/RAM/disco tipado con agregación saturante y HostCapacity con reserve CONFIGURABLE (usable = total medido − reserve; las metas 48 CPU/64 GiB quedan explícitamente como hipótesis, AC4). (2) ledger.rs: BudgetLedger con admisión agregada — los entornos en PROVISIONING cuentan al presupuesto (AC1: una ráfaga de creates no sobresuscribe por la ventana de creación); solicitud que supera presupuesto → Waitlisted con motivo ANTES de crear nada (nada se consume); release devuelve presupuesto; suspensión por presión bloquea admisiones nuevas sin tocar el budget ni las liberaciones. (3) pressure.rs: PSI crítica → SuspendAdmissions; OOM → backoff exponencial ACOTADO (1s..32s cap); NUNCA hay elevación automática del presupuesto (AC3). 9 tests unitarios. Probe E2E budget-probe contra el lifecycle server REAL: host medido (12 CPUs / 15.3 GiB, nproc+meminfo), reserve 2/2 → usable 10/13.3; admisión real: 1ª admitida, 2ª Waitlisted por CPU agotada y el segundo POST /v1/sandboxes NUNCA se ejecutó (AC1 con el servidor real); sandbox creado CON resourceLimits {cpu:1, memory:1Gi} → límites EFECTIVOS leídos DESDE DENTRO del container por cgroups: cpu.max=100000 100000 (=1 CPU exacta) y memory.max=1073741824 (=1GiB exacto) — no basta el valor pedido por API (AC2), con auxiliares (execd+init) corriendo bajo los MISMOS límites contabilizados; workload de prueba medido bajo el límite y CANCELADO (DoD: dd con cpu.stat observado + kill + sandbox borrada + ledger released); PSI/OOM suspenden con backoff acotado y usable INVARIANTE (nunca auto-elevado) (AC3). 5/5 checks ×2 ejecuciones (~5s). Gates: fmt/clippy -D warnings/test workspace 152 passed 0 failed.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-12 completa: presupuestos agregados y protección del host. Presupuesto con admisión agregada donde los entornos en aprovisionamiento cuentan (una ráfaga de creates no sobresuscribe), solicitudes fuera de presupuesto esperan SIN crear sandbox y con motivo tipado; límites CPU/memoria EFECTIVOS verificados desde dentro del container por cgroups (no el valor de API), con auxiliares contabilizados bajo los mismos límites; señales de presión suspenden admisiones nuevas con backoff exponencial acotado y sin elevación automática del presupuesto; capacidad usable = host MEDIDO − reserve configurable (las metas 48 CPU/64 GiB son hipótesis documentadas, nunca garantías). Verificación: 9 tests unitarios + probe E2E 5/5 ×2 contra el server real incluyendo workload medido y cancelado; gates workspace 152 tests en verde.
<!-- SECTION:FINAL_SUMMARY:END -->
