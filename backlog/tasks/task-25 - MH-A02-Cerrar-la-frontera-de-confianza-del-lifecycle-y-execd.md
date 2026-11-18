---
id: TASK-25
title: '[MH-A02] Cerrar la frontera de confianza del lifecycle y execd'
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-18T11:49:00Z'
updated_date: '2026-11-18T16:01:00Z'
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

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Fijar el contrato de la frontera medido en el backend real (Docker 29.1.3), no supuesto: una red Docker interna (internal=true, IPv6 off) deja al sandbox SIN ruta por defecto, sin DNS externo y sin acceso al bridge del host, pero ALCANZABLE al gateway de esa red (verificado: 1.1.1.1:443 BLOCKED, 172.17.0.1:8889 BLOCKED, DNS SERVFAIL, proxy en 172.30.0.1:8888 REACHABLE). El proxy de egreso es el unico camino autorizado.
2. Plano de control: token obligatorio por entorno (HODEI_MH_CONTROL_TOKEN) con comparacion en tiempo constante, middleware de axum sobre /v1, 401 con el sobre de error del contrato. El binario NO arranca sin token: configuracion insegura falla antes de servir nada.
3. Cliente tipado del plano de control (opensandbox::control::ControlPlane) que adjunta el token desde el entorno del proceso que lo requiere; migrar los 64 puntos de llamada para que la regla de autenticacion viva en un solo sitio.
4. Puertos: publicar el execd y el puerto HTTP en la direccion configurada (127.0.0.1 por defecto), nunca en todas las interfaces; una direccion no-loopback exige una aceptacion explicita. El endpoint resuelto usa el ORIGEN CONFIGURADO, no la cabecera Host de la peticion.
5. Politica de red efectiva: el servidor crea y VERIFICA la red interna (internal=true, IPv6 desactivado) y adjunta cada sandbox a ella; si la red existe sin esas propiedades o el backend no puede imponerlas, NO crea el entorno y lo dice con el motivo. La URL del proxy pasa a ser el gateway de la red interna.
6. Limites obligatorios: la politica de limites (memoria, CPU, pids) es obligatoria en el servidor; una peticion que los exceda se rechaza con 400 ANTES de que exista el contenedor; una configuracion sin limites no arranca.
7. Autenticacion del execd: token por sandbox generado en el servidor, entregado al contenedor por el entorno del proceso que lo requiere (EXECD_ACCESS_TOKEN) y devuelto al cliente autorizado en el contrato del endpoint (headers). El cliente lo usa en la API y en el WebSocket; el execd sin token devuelve 401 (verificado contra opensandbox/execd v1.1.0).
8. Sonda real (hodei-mh trust-probe): negativa (peticion no autorizada no crea, no enumera, no borra ningun entorno; desde dentro del sandbox el acceso directo IPv4, IPv6, DNS y host estan bloqueados) y positiva (el cliente autorizado crea, usa y libera; el camino del proxy funciona desde dentro del sandbox). Sin credenciales en los logs ni en el registro.
9. Verificar: gate local sobre los crates tocados, la pista de sondas en local (con el token en el entorno del carril) y el job integration en verde antes de fusionar. Documentar la frontera en docs/profiles.md.
<!-- SECTION:PLAN:END -->
