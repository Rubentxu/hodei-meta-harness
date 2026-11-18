---
id: TASK-24
title: '[MH-A01] Vincular la verificación al candidato Git realmente ejecutado'
status: To Do
assignee: []
created_date: '2026-11-18T11:49:00Z'
labels:
  - audit-v0.2
  - security
  - verification
  - p0
dependencies: []
references:
  - crates/hodei-mh-verify/src/runner.rs
  - crates/hodei-mh-verify/src/evidence.rs
  - crates/hodei-mh-promote/src/git.rs
priority: high
ordinal: 24000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0 · F02 · TASK-9/10. runner.rs:55-81,169-191 recibe revision/digest y ejecuta main sin contrastar HEAD. evidence.rs:39-67 permite construcción y binding públicos; promote/git.rs:106-172 hace CAS sobre la base, no sobre la evidencia. Separar digest de transporte, GitRevision y política/entorno observado; construir evidencia únicamente desde mediciones. Un candidato verificado no es una prueba de que el código sea seguro: los checks autorizados deben ser independientes del agente. No cambiar algoritmo de Git CAS.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Bundle válido con SHA declarado distinto o digest alterado se rechaza antes de los checks; se registra HEAD realmente comprobado.
- [ ] #2 El conjunto de checks requerido, sus comandos autorizados y el ID de imagen observado están ligados al SHA; cambiar cualquiera invalida la evidencia.
- [ ] #3 La promoción exige evidencia del mismo commit y verifica el bundle en su frontera; se rechazan evidencia ausente, obsoleta o de otro trabajo.
- [ ] #4 Prueba negativa con bundle válido de otro SHA y política diferente, más un caso positivo usando la ruta real sin LLM.
<!-- AC:END -->
