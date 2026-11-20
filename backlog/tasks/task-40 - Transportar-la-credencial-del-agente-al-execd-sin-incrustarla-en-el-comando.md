---
id: TASK-40
title: Transportar la credencial del agente al execd sin incrustarla en el comando
status: To Do
assignee: []
created_date: '2026-11-20T10:16:00Z'
updated_date: '2026-11-20T10:21:00Z'
labels:
  - descubrimiento
dependencies: []
priority: high
ordinal: 40000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Descubrimiento de la revisión independiente de TASK-34 (linea base f3dcec5). El agente ACP recibe ANTHROPIC_AUTH_TOKEN incrustado en el comando de create_session del execd (crates/hodei-mh/src/live/agent.rs:299-318; mismo patrón heredado en main.rs:523-535, interact_probe.rs:43, autonomous_probe.rs:163), de modo que el secreto queda en el cuerpo del comando y en la tabla de procesos del sandbox aunque los logs se redacten. TASK-28 lo dejó explícitamente como follow-up de TASK-34. Investigar si el execd acepta un mapa env en /pty; si no, diseñar el handshake por el fd de stdin sin partir el stream ACP. Criterio: secret_leak_positions vacío sobre el comando y sobre el estado persistido, y el agente recibe la credencial solo por el entorno de su proceso.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 El comando de create_session y el estado o log persistido no contienen la credencial: secret_leak_positions vacío sobre ambos y una prueba determinista lo impide.
- [ ] #2 El proceso del agente recibe ANTHROPIC_AUTH_TOKEN solo por su entorno; ninguna ruta de producto lo incluye en argv, archivo persistido, evento ni handoff.
- [ ] #3 Los consumidores heredados (main.rs, interact_probe, autonomous_probe) usan el transporte nuevo o quedan fuera de alcance con justificación registrada; las probes existentes siguen verdes.
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Mapa de hechos verificado (solo lectura, base f3dcec5). Transporte actual: Credentials::from_env lee ANTHROPIC_AUTH_TOKEN/ANTHROPIC_BASE_URL del entorno del controlador (live/agent.rs:81-90); agent_command construye un único string shell con PATH/HTTPS_PROXY/[BASE_URL]/AUTH_TOKEN (live/agent.rs:301-318), enviado como CreateSessionRequest{cwd, command} por SandboxSession::attach_agent (live/session.rs:194-213). El comando no se loguea (Debug redacta, agent.rs:70-77; test secret_leak_positions==[0], agent.rs:620-632) pero sí viaja en el cuerpo de la petición execd y en la tabla de procesos del sandbox. Mismo patrón en main.rs:523-538, interact_probe.rs:29-46, autonomous_probe.rs:151-166. Execd: CreateSessionRequest solo {cwd, command} (execd/api.rs:27-33); frames de control solo stdin/signal/ping (execd/frames.rs:82-114), sin mecanismo env; stdin es NDJSON ACP desde el primer byte (opensandbox/src/acp.rs:1-2; agent.rs:472-478). Lifecycle: CreateSandboxRequest.env: Option<BTreeMap> (lifecycle-server/src/model.rs:17), build_env filtra OPENSANDBOX_*/EXECD_* y añade identidad + EXECD_ACCESS_TOKEN (compose.rs:239-266,291-324), aplicado como env de contenedor (docker.rs:509-585); creationKey hashea la intención con env como label (compose.rs:215-221). Invariante TASK-28: CredentialChannel::ProcessEnvOnly (profiles/src/credentials.rs:1-16); TASK-28 dejó explícitamente como follow-up el token-en-comando de main.rs y probes. Diseños evaluados: (A) env en la creación del sandbox, contrato existente pero visible a todos los procesos del contenedor (docker inspect, /proc); (B) wrapper que lee una línea de stdin, exporta la variable y hace exec del adaptador, con el token enviado como frame stdin antes del ACP: cumple 'solo por el entorno de su proceso' pero el wrapper debe consumir exactamente una línea y el secreto transita el pipe/replay; (C) fichero 0600: descartado, viola TASK-28. Siguiente paso: implementar B (fallback A si el handshake con el adaptador no es fiable), con prueba determinista de que el comando y el estado persistido no contienen el token.
<!-- SECTION:NOTES:END -->
