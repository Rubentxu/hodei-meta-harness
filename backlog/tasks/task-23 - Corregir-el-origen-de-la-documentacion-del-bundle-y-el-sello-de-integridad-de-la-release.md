---
id: TASK-23
title: >-
  Corregir el origen de la documentacion del bundle y el sello de integridad de
  la release
status: Done
assignee:
  - '@claude'
created_date: '2026-11-18T11:38:00Z'
updated_date: '2026-11-18T12:11:00Z'
labels: []
dependencies:
  - TASK-22
ordinal: 23000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
TASK-22 (verificacion de la release v0.1.0 como consumidor nuevo) encontro tres defectos reales en lo que la release entrega. D1: "hodei-mh package" toma docs/profiles.md de una ruta NO verificada relativa al directorio de trabajo, de modo que desde un directorio sin ese fichero falla con un error no documentado y desde un directorio con OTRO docs/profiles.md empaqueta la documentacion ajena sin avisar, con el manifiesto declarando que no hay nada no demostrado. D2: el bundle no lleva los digests de los binarios que contiene y su INSTALL.md no menciona el checksum, asi que el consumidor que solo tiene el bundle no puede verificar la integridad sin red. D3: el uso de --help apunta a docs/profiles.md, ruta que no existe junto al binario instalado (la documentacion esta en <prefix>/share/hodei-mh/docs/). Los tres son de la ruta de distribucion, no del runtime: el consumidor puede instalar y usar v0.1.0, pero el bundle no es una entrega con el sello puesto.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 El bundle se arma con la documentacion del PRODUCTO: el binario instalado usa la suya (<prefix>/share/hodei-mh/docs/profiles.md), el checkout usa la de su workspace, y si no hay ninguna fuente de confianza el empaquetado se RECHAZA con un motivo (no se empaqueta documentacion ajena ni se empaqueta sin ella)
- [x] #2 El manifiesto declara de donde salio la documentacion y con que digest, de modo que un bundle con documentacion ajena es visible en la propia salida del comando
- [x] #3 El bundle lleva los digests SHA-256 de los dos binarios que contiene y su INSTALL.md explica como verificarlos sin red
- [x] #4 El uso de --help apunta a la documentacion que el consumidor tiene de verdad en el prefix instalado
- [x] #5 Tests que cubren la resolucion de la documentacion (instalado, workspace y sin fuente) y el contenido del manifiesto
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Evidencia de los tres defectos de la ruta de distribucion (D1, D2, D3), medida antes y despues.

D1 — origen de la documentacion. ANTES: `package` tomaba `docs/profiles.md` de `repo.join("docs/profiles.md")`, con `repo` derivado de `CARGO_MANIFEST_DIR` (que no existe en runtime -> "." = el directorio de trabajo). Medido: desde un directorio sin ese fichero el empaquetado fallaba; desde un directorio con OTRO `docs/profiles.md` se empaquetaba la documentacion ajena en silencio. AHORA: `crates/hodei-mh/src/docs.rs` resuelve la documentacion SOLO desde fuentes de confianza (la instalada junto al binario en `<prefix>/share/hodei-mh/docs/profiles.md`, o la del workspace identificado por `crates/hodei-mh/Cargo.toml`) y el empaquetado se RECHAZA con motivo si no hay ninguna. Medido: desde `/tmp/prefix23` (instalado) con un `docs/profiles.md` ajeno en el directorio de trabajo, el manifiesto declara `origin: "installed"` y el documento empaquetado es el del producto ("# hodei-meta-harness: language profiles (Rust / JVM)").

D2 — sello de integridad. ANTES: el bundle no llevaba digests y su INSTALL.md no mencionaba el checksum. AHORA: el bundle lleva `SHA256SUMS` con el digest de cada fichero que embarca (los dos binarios y la documentacion), calculado sobre los bytes ya preparados, e INSTALL.md explica la verificacion offline. Medido: `LC_ALL=C sha256sum -c SHA256SUMS` -> `bin/hodei-mh: OK`, `bin/hodei-mh-lifecycle-server: OK`, `docs/profiles.md: OK`.

D3 — puntero de `--help`. ANTES: `documentation: docs/profiles.md`, ruta que no existe junto al binario instalado. AHORA: `crates/hodei-mh/src/main.rs` resuelve la ruta en tiempo de ejecucion. Medido: `/tmp/prefix23/bin/hodei-mh --help` -> `documentation: /tmp/prefix23/share/hodei-mh/docs/profiles.md`.

AC1-AC5 comprobados. Tests nuevos: `digest::tests` (4), `docs::tests` (8) y `package::tests` (5); 23 en hodei-mh y 251 en el workspace, `cargo fmt --all` y `cargo clippy --workspace --all-targets -- -D warnings` limpios.

**PUBLICADO EN v0.1.1.** El arreglo viaja en la release v0.1.1 (tag v0.1.1 -> commit `d444602`, assets publicados y verificados por un consumidor real). El bundle publicado lleva `SHA256SUMS` con el digest de los tres ficheros que embarca, su `INSTALL.md` explica la verificacion offline, el manifiesto declara `documentation.origin` y `documentation.sha256`, y `--help` nombra la documentacion del prefix instalado.

**Un efecto colateral medido y corregido:** la sonda `multiproject-probe` afirmaba un conjunto de contenidos del bundle sin `SHA256SUMS`, asi que fallo con el bundle nuevo (mas completo que lo que la sonda esperaba). La sonda se corrigio (PR #30) para aceptar el sello y ADEMAS VERIFICARLO (`sha256sum -c SHA256SUMS` sobre los bytes desempaquetados, sin red), que es una comprobacion mas fuerte que la anterior. Como el binario publicado debe llevar la sonda correcta, el tag v0.1.1 se reapunto a `d444602` ANTES de que existiera la release (no se publico ningun asset de v0.1.1 antes de ese reapuntado); la v0.1.0 publicada no se toco.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
El bundle ahora se arma con la documentacion del PRODUCTO (la instalada junto al binario, o la del workspace marcado por crates/hodei-mh/Cargo.toml) y el empaquetado se RECHAZA con motivo cuando no hay fuente de confianza; el manifiesto declara el origen y el digest de la documentacion que viaja; el bundle lleva SHA256SUMS con el digest de cada fichero que embarca e INSTALL.md explica la verificacion offline; y --help resuelve la ruta de la documentacion en tiempo de ejecucion. 17 tests nuevos (digest, docs, package), 256 en el workspace, fmt y clippy limpios. publicado en v0.1.1.
<!-- SECTION:FINAL_SUMMARY:END -->
