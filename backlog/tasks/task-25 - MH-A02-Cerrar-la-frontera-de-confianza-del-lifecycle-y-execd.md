---
id: TASK-25
title: '[MH-A02] Cerrar la frontera de confianza del lifecycle y execd'
status: To Do
assignee: []
created_date: '2026-11-18T11:49:00Z'
labels:
  - audit-v0.2
  - security
  - lifecycle
  - p0
dependencies: []
references:
  - crates/lifecycle-server/src/http.rs
  - crates/lifecycle-server/src/docker.rs
  - crates/lifecycle-server/src/compose.rs
  - crates/lifecycle-server/src/main.rs
  - crates/hodei-mh/src/profile_run.rs
priority: high
ordinal: 25000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
P0. HTTP sin autenticación; puertos Docker host_ip None; bridge por defecto; el perfil mide conectividad y no la impone. Asegurar modo local por defecto con autenticación de cliente/execd y política de red efectiva; usar origen configurado, no Host de la petición. Reusar red interna y proxy autorizado sin construir otro firewall. TASK-18. Mantener compatibilidad del subconjunto OpenSandbox que se usa; no implementar endpoints sin consumidor. Separar control e I/O del agente; no montar Docker socket dentro de sandbox. Comprobar qué soporte de red ofrece el backend real y denegar si no puede imponer la política.
Revisión de referencia: 3d33f85a63bc59a7e814f5f205e546758c943b61.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Las peticiones no autorizadas no crean, enumeran, ejecutan ni eliminan entornos; puertos no expuestos a LAN por defecto.
- [ ] #2 Desde un sandbox se bloquea acceso directo no autorizado (IPv4, IPv6 cuando disponible, DNS y host); proxy permitido funciona.
- [ ] #3 Configuración insegura, política desconocida o límites obligatorios ausentes fallan antes de ejecutar el agente.
- [ ] #4 Una prueba real negativa y positiva en backend soportado; no registrar credenciales.
<!-- AC:END -->
