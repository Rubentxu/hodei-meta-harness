---
id: TASK-25
title: '[MH-A02] Cerrar la frontera de confianza del lifecycle y execd'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-18T11:49:00Z'
updated_date: '2026-11-18T21:38:00Z'
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
- [x] #1 Las peticiones no autorizadas no crean, enumeran, ejecutan ni eliminan entornos; puertos no expuestos a LAN por defecto.
- [x] #2 Desde un sandbox se bloquea acceso directo no autorizado (IPv4, IPv6 cuando disponible, DNS y host); proxy permitido funciona.
- [x] #3 Configuración insegura, política desconocida o límites obligatorios ausentes fallan antes de ejecutar el agente.
- [x] #4 Una prueba real negativa y positiva en backend soportado; no registrar credenciales.
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

Revised network approach after Docker experiments: internal bridge alone does not protect the host gateway and cannot publish execd ports on the tested backend. Investigate and implement isolated gateway mode with a separate loopback-only control relay and a restricted, separately connected egress proxy; verify isolation, REST/WebSocket compatibility, cleanup, and fail-closed behavior before calling the boundary complete. The earlier gateway-proxy assumptions are superseded by this evidence.

The shared isolated bridge has been proven to permit lateral container connections. Investigate per-sandbox isolated networks attached to a trusted restricted proxy, with unique subnets, fail-closed provisioning and cleanup; confirm Docker DNS alias and proxy reachability on the real backend before implementing this revised topology.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Continuation: fixed multiproject probe compilation, authenticated its budget requests and cleanup, and safely quoted the proxy URL. Added CIDR validation, verified existing and newly created network subnet/gateway, and enforced positive mandatory limits including configured PID limit. Targeted tests for hodei-mh, lifecycle-server and opensandbox and cargo fmt check pass. Remaining: real negative/positive trust probe, restrict access to other services on the internal gateway, adapt scripts/probes.sh and docs, and independent security review; acceptance criteria remain unchecked.

Security investigation: Docker internal bridge permits access to other host listeners on its gateway; the existing proxy also allows arbitrary CONNECT destinations, so the current topology does not satisfy AC-2. On Docker 29.1.3 a disposable internal bridge with gateway_mode_ipv4=isolated had no host gateway but Docker reported published 8080/tcp as null, even for a running container. Isolated mode therefore cannot be substituted blindly: a separate trusted relay/control topology is needed and must be tested. No existing network was modified.

Additional TDD fixes: replaced hand-written token comparison with subtle (length truncation regression reproduced), eliminated duplicate reserved execd environment variables, reject NaN/zero-rounded CPU and non-positive limits. Eight proxy policy/tunnel tests pass after restricting CONNECT to exact allowed HTTPS domains and globally routable resolved addresses. Docker isolated bridge still needs a separate control-plane relay; the current design must not be claimed secure or merged.

Real Docker diagnostic: profile-probe failed because execd endpoint resolution returned 404: the running sandbox on the existing internal bridge had Docker NetworkSettings.Ports 44772/tcp=null and 8080/tcp=null. Reproduced independently with docker run -p on both internal and isolated networks; a control on the default bridge published successfully. Unauthorized list returned 401; authorized sandbox creation returned 200 and cleanup returned 200. The real probe lane exited 1 and reported no residue. Avoid 60-second readiness retries when endpoint resolution fails.

Fail-fast profile-run now reports endpoint-resolution errors and attempts cleanup instead of waiting 60s on an empty endpoint. Added proxy URL validation/canonicalization and removed host shell interpolation; redacted control and execd credentials from Debug (regressions reproduced). Scoped Rust and proxy tests pass. Experiment verified that busybox and its musl loader extracted from the existing execd image run inside an isolated rust container; full stdio relay remains unproven.

A disposable Docker experiment successfully relayed HTTP/1.0 through `docker exec -i` to a sandbox-local server on an isolated bridge, using busybox plus its musl loader extracted from the existing execd image. This validates feasibility only; WebSocket, half-close, lifecycle cleanup, proxy isolation and production integration still require verification.

After fail-fast handling the real profile-probe still fails as expected on endpoint 404, but completes its checks in 5.83 seconds instead of 303.5 seconds; the lane reported sandboxes=0 and containers=0.

Integrated loopback-only Docker-exec relay and isolated bridge policy. Real backend checks: execd session creation 201 with token, 401 without; oneshot WebSocket returned relay-websocket-ok with no missing output; sandbox and network cleaned. A dedicated dual-network restricted proxy blocked direct host and CONNECT to a private address (403); allowed HTTPS to github.com succeeded (200) when a public host-resolved address was supplied because this Docker bridge cannot resolve DNS in the current environment. The real profile-probe lane passed with zero reported sandbox/container residue before subsequent fail-closed and cleanup refinements, which must be re-tested.

Independent code exploration found that sandboxes share the same isolated bridge without an inter-container isolation control; lateral reachability is not yet tested or ruled out. The pre-build proxy observation is specific to profile-run and checks TCP reachability, not proxy identity or CONNECT policy; other agent execution paths bypass it. These limitations prevent claiming a complete trust boundary. Targeted Rust tests, proxy unit tests and shell syntax check passed after the latest refinements.

Repeated profile-probe after the latest changes: PASS, with sandboxes=0 and containers=0. A separate real Docker test on an internal isolated bridge confirmed lateral access: a second container connected to a listener in the first (LATERAL_REACHABLE); test-owned containers and network were removed. A shared isolated bridge alone cannot enforce sandbox-to-sandbox isolation. Documentation now states the actual topology and proxy verification limits.

Relay architecture selection now uses the sandbox image actually inspected by Docker and refuses an execd image with a different architecture before starting the sandbox; relay recovery no longer chooses a loader from the current default execd image. Lifecycle-server unit tests passed; real profile-probe rerun is in progress.

Real Docker topology experiment succeeded: one proxy container attached to two separate isolated internal networks was resolvable through the same Docker DNS alias in each network and reachable at each network-specific address. Temporary resources were removed. This validates topology feasibility, not isolation or proxy policy by itself.

After fixing loader architecture selection, the real profile-probe passed again on the existing shared-network topology, reporting sandboxes=0 and containers=0. This does not resolve the independently confirmed lateral access; per-sandbox network implementation is underway.

A regression test reproduced that the execd HTTP client followed a 307 redirect; disabled automatic redirects for both execd and control clients to avoid forwarding authenticated requests to another endpoint. Added an endpoint Display regression to prevent rendering URL userinfo, path, query or fragment; execd-specific tests pass.

Implemented dedicated Docker-allocated isolated network per sandbox and operator-configured proxy container attached by DNS alias. Targeted Rust suite passed (34 hodei-mh, 51 lifecycle-server, 45 opensandbox tests), proxy tests 8/8 and shell syntax passed. Real profile-probe passed with this topology (sandboxes=0, containers=0). Lateral isolation, network residue/failure recovery, proxy CONNECT and end-to-end negative cases remain under verification; the real probe did not establish those guarantees.

Updated the CI probe job to provision its own restricted proxy and generate a masked ephemeral control token; the updated workflow has not yet run on GitHub. Added a refusal for obsolete SANDBOX_NETWORK configuration rather than silently ignoring it. Network cleanup is being hardened for retry and proxy replacement.

Dedicated network cleanup now records the original proxy Docker ID and permits retry after sandbox removal. Independent implementation report: 52 lifecycle unit tests passed and a Docker 29 test verified that an unrelated attached endpoint blocks cleanup, remains intact, and a later DELETE succeeds without deleting the proxy. Old test networks lacking the proxy label require manual cleanup; orphan networks are not automatically rediscovered after a crash. The probe lane now checks managed network residue as well as containers.

After the cleanup changes the targeted Rust tests passed (52 lifecycle, one Docker-only test ignored in the ordinary run; 45 opensandbox; 34 hodei-mh). Clippy initially failed on field_reassign_with_default in a limit-policy test; fixed it and scoped lifecycle/opensandbox clippy now passes. The first attempt to select the ignored Docker test with --exact matched zero tests; it has been rerun without --exact and is pending.

The ignored Docker cleanup regression was executed explicitly and passed (1 test). The hodei-mh scoped clippy run initially found items_after_test_module in profile_run; moved the tests and reran clippy successfully. Changed the profile default proxy address from the obsolete shared-network IP to the dedicated-network Docker alias.

After the retryable cleanup changes the real profile-probe passed again on Docker 29 and the updated residue check reported sandboxes=0, containers=0, networks=0. The profile default was subsequently changed to the same hodei-proxy alias; its focused unit tests pass. A broader negative/positive boundary probe and security review are still pending.

Independent static exploration identified that Docker port bindings were still requested despite the relay; disabled auto-publication for 44772 and 8080. It also identified unencrypted non-loopback control-plane listening; HOST now refuses anything except localhost/IPv4 loopback. Other findings: global concurrent-admission race belongs to existing TASK-27; unauthenticated loopback relay sockets can consume 32 slots and cause local denial of service, which is not yet resolved. Review was not a formal Goldfish run: host-enforced read-only/source isolation was unavailable; the specialized reviewer also returned incomplete without its required binding.

The independent boundary-test implementation did not return after an extended run and was stopped; its untracked draft was retained for inspection rather than treated as a result. Corrected its proxy test to use HTTPS CONNECT and added unauthorized create/delete checks. It has only passed Python syntax validation so far.

The first live boundary-probe attempt failed before creating sandboxes: the test process could not bind host TCP port 53 (PermissionError), so its DNS control target was not reachable and the negative result would have been inconclusive. Re-running with a disposable container listener on TCP/53 and a separate host listener; this tests TCP connectivity to port 53, not the DNS protocol itself.

Frontier closed and measured on the real backend (Docker 29.1.3). One internal bridge PER SANDBOX (internal=true, enable_ipv6=false, gateway_mode_ipv4=isolated), subnet allocated by Docker from its own pools, container attached to that network alone; the creation verifies the attachment, the applied limits and the real container state and FAILS CLOSED instead of reporting Running. The execd listens on 0.0.0.0:44772 of its own sandbox network, so the topology is not the barrier: the barrier is the per-sandbox credential, minted by the server, injected in the container and delivered only in the endpoint contract of that sandbox; the relay is loopback-only, per sandbox and carries NO credential of its own. The operator's proxy is attached by immutable container id under the alias hodei-proxy, speaks CONNECT to an exact allow-list, validates every DNS answer, refuses a client network broader than a /12 and bounds its open tunnels.

Real probe (hodei-mh trust-probe, 22 checks) measures from the host AND from inside the sandbox: unauthorized list/create/inspect/endpoints/delete are refused with 401 and no side effect; the endpoint is loopback-only and does not move with a foreign Host header; the execd refuses a missing, wrong or foreign credential and refuses an unauthenticated request even from INSIDE the sandbox; from inside there is no default route, no global IPv6, no external name resolution (measured through Docker's embedded resolver 127.0.0.11), no route to the host, to the internet, to an IPv6 destination or to another sandbox, while the authorized proxy IS reachable and carries traffic (2xx) and refuses plain HTTP. The probe leaves no residue and no credential in its report.

Defect found by the independent risk review and FIXED before closing: the environment protected the reserved names by exact match while the credential was read back from the container by PREFIX, so a request with the key 'EXECD_ACCESS_TOKEN=x' produced an entry the server read as the credential and handed over to its own client (reproduced live: the endpoint answered X-EXECD-ACCESS-TOKEN: x=y). Now the whole prefix is reserved, a request may not write into the EXECD_/OPENSANDBOX_ namespaces at all, and the credential is read by EXACT name, with an ambiguous one refused. The probe measures this boundary as P22 and the check was verified to have teeth: against the pre-fix server the probe answers FAIL and the lane fails.

Verification: cargo test --workspace green (the Docker-only test also passes when run explicitly with -- --ignored), cargo fmt --check clean, cargo clippy --all-targets clean, 12/12 proxy tests, and the full lane (release, supervisor, profile, multiproject, verify, trust) PASS with sandboxes=0 containers=0 networks=0 on a clean host, run twice in a row. The boundary is documented in docs/profiles.md.

LIMITES CONOCIDOS de la frontera entregada (documentados, no resueltos en esta tarea, con su destino): (1) el presupuesto global de entornos cuenta y reserva sin atomicidad: dos altas simultaneas pueden admitir mas de lo que el presupuesto permite — pertenece a TASK-27 (MH-A04); (2) la descarga de imagenes (docker pull) ocurre en el HOST, fuera del proxy restringido: el host es de confianza, pero es una ruta de egreso que conviene decidir; (3) el listener del plano de control no acota el tamano del cuerpo ni fija un timeout de lectura por peticion; (4) la validacion de direcciones globales del proxy tiene un borde con los prefijos NAT64; (5) el contrato de entorno del execd vive en la imagen opensandbox/execd:v1.1.0, que NO esta en el repositorio: no se puede probar que variables honra, asi que la reserva de los namespaces EXECD_/OPENSANDBOX_ cierra lo que el codigo ve y no lo que el execd haga con variables que nadie conoce.

Frontier hardening in this cycle. The subnet Docker allocated is read back and verified against the pool the operator declares (SANDBOX_ADDRESS_POOL, default 172.16.0.0/12, the same range the trusted proxy accepts clients from): a subnet outside it is refused, and SANDBOX_NETWORK/SANDBOX_SUBNET are refused outright because the networks are per sandbox. The image is inspected and refused when it declares the EXECD_/OPENSANDBOX_ namespace, because the daemon merges the environment of the image with the one of the container; the names are compared over bytes, so a name that is not ASCII cannot stop the server (regression test). The credential read back from the container is compared with the one this server minted, and a foreign one is refused instead of delivered.

The network of a creation is now OWNED by that creation: a guard removes it when the creation fails, is cancelled or is abandoned, which closes the leak of a bridge nobody owned (the lane had caught one). The rollback reports its failures instead of discarding them, every retry of a removal makes sure the trusted proxy is really detached (that endpoint is what the daemon refuses the removal for), the release of a sandbox is logged with what it removed, and the orphan sweep drops from ten minutes to two. Shortening that margin made an assumption load-bearing, so the creations in flight are now RECORDED and the sweep never takes the network of one of them: the age of a network says nothing about how long the creation takes. The relay bounds the life of a connection (2 hours) and reports its refusals by kind, never with the output of the daemon.

The trust probe now measures the egress over UDP (P23), reports a check it could not measure as NOT_MEASURED (which FAILS the probe, because the absence of evidence is not evidence), verifies the credential against the REAL execd (401 for the token the caller chose, 2xx for the one the server minted) and the identity from inside the sandbox, and finds the environments it could not name by the marker it left in their metadata. The lane reports the residue as the difference it caused, not as the state of the host.

Verification: cargo test --workspace green (64 lifecycle-server, 41 hodei-mh and the rest of the crates; the two Docker-only tests pass when run with -- --ignored), cargo fmt --all --check clean, cargo clippy --all-targets clean, scripts/fast-check.sh --all PASS, and the lane (scripts/probes.sh --rounds 2) PASS with sandboxes=0 containers=0 networks=0, repeated with the final code.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
The control plane and the execution plane are separated by a boundary that is enforced and MEASURED against real Docker (29.1.3).

Each sandbox lives in its own internal bridge (internal=true, IPv6 off, gateway_mode_ipv4=isolated) whose subnet Docker allocates and the server verifies against the pool the operator declares (SANDBOX_ADDRESS_POOL, default 172.16.0.0/12). The only way out is the operator's restricted egress proxy, attached by immutable container id under the alias hodei-proxy. The execd listens on 0.0.0.0 of that bridge, so the topology is not the barrier: the barrier is the per-sandbox credential, minted by the server, injected in the container, read back by exact name, compared with the one minted and delivered only in the endpoint contract. A request may not write into the EXECD_/OPENSANDBOX_ namespaces, and neither may the image. Without a control token, with an unknown policy, a malformed pool or missing limits, the server does not start.

The creation owns its network: a creation that fails, is cancelled or is abandoned takes its bridge with it, the creations in flight are recorded so the sweep never takes their bridge away, the rollback and every retry report what they could not remove, and the orphan sweep is down to two minutes. The relay bounds the life of a connection and reports its refusals by kind.

hodei-mh trust-probe measures 23 properties from the host and from inside two real sandboxes: unauthorized list/create/inspect/resolve/delete are refused with no side effect, the endpoint is loopback-only and does not move with a foreign Host header, the execd refuses a missing, wrong or foreign credential even from inside the sandbox, and from inside there is no default route, no global IPv6, no external resolution, no route to the host, to the internet, to an IPv6 destination or to another sandbox, while the authorized proxy works and refuses plain HTTP. A check that could not be measured is reported NOT_MEASURED and FAILS the probe; the report carries no credential and the environments it creates are deleted, including the one whose identifier it never received.

Verified: cargo test --workspace green (including both Docker-only tests), cargo fmt and clippy clean, fast-check --all PASS, and the probe lane PASS with sandboxes=0 containers=0 networks=0, repeated with the final code. The boundary is documented in docs/profiles.md.

Open limits (documented, with their destination): the global environment budget is not atomic (TASK-27), the image pull happens on the host outside the restricted proxy, the control listener does not bound the body size nor a read timeout, and the execd's own environment contract lives in an image that is not in this repository.
<!-- SECTION:FINAL_SUMMARY:END -->
