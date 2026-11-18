---
id: TASK-20
title: Hacer deterministas los one-shot contra un execd que todavía no responde
status: In Progress
assignee: []
created_date: '2026-11-18T06:12:00Z'
updated_date: '2026-11-18T06:30:00Z'
labels: []
dependencies: []
ordinal: 20000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
El probe de release falla de forma INTERMITENTE contra la infra viva (3 de 3 ejecuciones recientes con checks en rojo), siempre en los checks que dependen de un one-shot (idle-release, loss-recovery, un-exported-work). Hipótesis a verificar: el execd de un sandbox recién creado puede rechazar las primeras tentativas (el contenedor aún arranca y el execd aún no escucha), y ni el producto (profile_run) ni los probes esperan a que esté listo: el intento se registra como si el COMANDO hubiera fallado. Agravante: los details de esos checks son literales estáticos que no dicen QUÉ falló, así que el fallo es indistinguible de un fallo real del comando. Alcance: esperar el execd con un límite ACOTADO antes de ejecutar el comando real (el comando nunca debe ejecutarse dos veces), reportar los hechos crudos en los details, y dejar los probes verdes de forma REPETIBLE (varias ejecuciones seguidas), no de forma accidental.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 El execd se espera con un límite acotado antes de ejecutar cualquier comando real, y el comando real se ejecuta exactamente una vez (un comando que ya corrió y falló NO se reintenta)
- [ ] #2 Los details de los checks affected reportan los hechos crudos (exit code y salida del one-shot, y el id del sandbox creado o su ausencia) en lugar de literales estáticos: un fallo debe ser diagnosticable desde el informe
- [ ] #3 cargo test --workspace en verde, cargo fmt --check y cargo clippy --workspace --all-targets -D warnings limpios, y los cuatro probes (supervisor, profile, release, multiproject) verdes en ejecuciones REPETIBLES seguidas contra la infra viva
- [ ] #4 Documentado el comportamiento: qué garantiza el producto (espera acotada del execd, reintento solo de fallos de transporte) y qué no
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Reproducir: el release-probe falla de forma intermitente contra la infra viva (3/3 ejecuciones en rojo), siempre en los checks que dependen de un one-shot.
2. Confirmar la causa con los hechos crudos en los details del probe (no con literales estáticos).
3. Adapter (crates/opensandbox): distinguir 'el execd rechazó la sesión' (el comando NUNCA corrió → se puede repetir) de 'el comando corrió y no dio estado' (no se repite).
4. Adapter: espera ACOTADA de disponibilidad del execd, con un probe inofensivo, de modo que el comando real se ejecute EXACTAMENTE UNA vez.
5. Producto (profile_run): esperar la disponibilidad del execd tras crear el sandbox; si el sandbox nunca es usable, fallo explícito (exit 1) y limpieza.
6. Probes y runner de verificación: usar la espera acotada en lugar del one-shot directo.
7. Gates: fmt, clippy -D warnings, suite del workspace, y los cuatro probes re-ejecutados VARIADAS VECES seguidas contra la infra viva.
8. Documentar el comportamiento en docs/profiles.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Causa CONFIRMADA empíricamente (no por lectura): con la espera desactivada, todos los one-shot lanados justo después de crear un sandbox fallan con 'create failed: error sending request' y end=NotAccepted — el puerto del execd se publica antes de que el servidor dentro acepte sesiones. Con la espera acotada, 3/3 ejecuciones del release-probe quedan en verde.
<!-- SECTION:NOTES:END -->
