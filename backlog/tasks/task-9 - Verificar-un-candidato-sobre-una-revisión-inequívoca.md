---
id: TASK-9
title: Verificar un candidato sobre una revisión inequívoca
status: Done
assignee: []
created_date: '2026-11-17T14:20:00Z'
updated_date: '2026-11-17T16:30:00Z'
labels:
  - gc-mvp
  - gc-09
  - hodei-mh
dependencies:
  - TASK-2
  - TASK-3
  - TASK-5
ordinal: 9000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
EvidenceSet vinculado a WorkRef/revision/candidate digest y política efectiva. Ejecutar comandos de verificación configurados en entorno aislado; preferir verificador limpio para pruebas sensibles a contaminación. Si no hay comando inferir propuesta desde instrucciones del proyecto y documentar evidencia proporcional. No convertir documentos o tests irrelevantes en gates administrativos.

Clave de incorporación: GC-09.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Exit status terminal y salida truncada/incompleta se distinguen; ausencia de resultado nunca es PASS.
- [x] #2 Los resultados se vinculan al candidato exacto y al comando y entorno ejecutados; una modificación invalida evidencia afectada.
- [x] #3 AC y DoD técnicos se verifican individualmente; NOT_RUN no marca cumplido y un commit vacío no acredita avance.
- [x] #4 Un verificador limpio demuestra que un archivo no versionado o una dependencia ad hoc puede provocar un fallo que el entorno de desarrollo ocultaba.
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Ejecutar pruebas reales de PASS FAIL NOT_RUN y cambio de candidato en repositorios de ejemplo.
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Evidencia TASK-9 (rama task/task-9-verify): crate hodei-mh-verify con (1) digest.rs — CandidateDigest sha256 canónico (paths ordenados, framing path\nlen\nbytes anti-colisión); candidate_progress distingue Advanced vs NoChange (commit vacío NO acredita avance, AC3). (2) result.rs — ExecutionRecord con exit_code Option (None=ausencia real), output_truncated visible y stream_completed; verdict_of total: exit 0+stream completa=Pass, no cero=Fail, ausencia/signal/stream incompleto=NotRun; truncado seguro en char boundaries, cap 64KiB. (3) evidence.rs — EvidenceSet::bind liga cada resultado a check_id+entorno+digest+comando; candidate_changed invalida la evidencia afectada (Pass almacenado pasa a NotRun efectivo); check_satisfied solo con Valid+Pass; all_checks_satisfied verifica AC/DoD individualmente. (4) policy.rs — explicit vs infer_from_instructions (comandos conocidos + hints de herramientas; propuesta documentada con rationale, nunca disfrazada de gate configurado). (5) runner.rs — CleanRunner: sandbox efímero + upload bundle + git fetch/checkout real (integridad) + ejecución attach-first (el execd purga sesiones sin holder antes de ejecutar: attach inmediato tras create); timeout→SIGKILL→señal=código negativo=NOT_RUN nunca PASS; retry de conexión post-create. 21 tests unitarios verdes. Probe E2E hodei-mh verify-probe (repositorio de ejemplo con trampa de contaminación: test.sh pasa SOLO con fixture.local no versionado en .gitignore): dev pasa (exit 0, contaminado), verificador limpio FALLA el mismo comando (AC4 demostrado), timeout sleep10→exit None→NotRun (AC1), PASS real con digest+entorno (AC2), checks individuales (AC3), cambio de candidato real (commit) invalida 3 resultados, commit vacío→NoChange, inferencia propuesta+documentada, cleanup completo. 11/11 checks verdes ×2 ejecuciones (~2.9s). Gates: fmt/clippy -D warnings/test --workspace en verde (114 tests). Hallazgo de infra documentado: execd real exige ser holder (attach WS inmediato) para que la sesión ejecute; sin attach la sesión se purga sin ejecutar — el runner del producto lo respeta por construcción.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
TASK-9 completa: verificación de candidatos sobre revisiones inequívocas. Crate hodei-mh-verify: CandidateDigest por contenido (sha256 canónico), EvidenceSet que liga cada resultado al candidato exacto (digest+revision), al comando y al entorno ejecutado; una modificación del candidato invalida la evidencia afectada automáticamente; checks AC/DoD verificados individualmente (NOT_RUN y FAIL nunca satisfacen; commit vacío=NoChange no acredita avance); verificador limpio en sandbox efímero con checkout real del bundle (integridad) que demuestra E2E el fallo que el entorno dev contaminado ocultaba (mismo comando: dev exit 0, limpio FAIL); timeout y muerte por señal producen resultado AUSENTE (exit None) que jamás se convierte en PASS; sin comando configurado se infiere PROPUESTA documentada con rationale, nunca un gate fingido. Verificación: 21 tests unitarios + probe E2E 11/11 checks ×2 ejecuciones contra sandbox real con repositorio de ejemplo; gates workspace en verde (114 tests). Hallazgo de infraestructura: el execd real purga sesiones sin holder WS antes de ejecutar el proceso — todo runner del producto debe attach-first; el CleanRunner lo incorpora por construcción.
<!-- SECTION:FINAL_SUMMARY:END -->
