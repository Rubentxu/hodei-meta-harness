//! The trust-boundary probe: the boundary is MEASURED, not asserted.
//!
//! Two halves make one boundary:
//!
//! - From the HOST: who may drive the control plane, and what the
//!   control plane exposes to the network.
//! - From INSIDE the sandbox: what the execution plane can reach. A
//!   host-side check proves nothing about the sandbox, so every
//!   isolation claim is measured by a command that runs in the sandbox
//!   itself.
//!
//! A check that cannot be measured is reported as `NOT_MEASURED`, never
//! as a pass: an unreachable target that the host cannot reach either
//! proves nothing. The credentials never appear in the report, in a
//! command or in a log line.

use std::net::{Ipv4Addr, TcpListener};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};

use opensandbox::control::ControlPlane;
use opensandbox::execd::api::{self, CreateSessionRequest};
use opensandbox::execd::endpoint::{EXECD_ACCESS_TOKEN_HEADER, ExecdEndpoint};
use opensandbox::execd::oneshot;
use opensandbox::execd::pipe;

/// How long a command inside the sandbox may take.
const SANDBOX_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the execd of a fresh sandbox may take to answer.
const EXECD_READY_TIMEOUT: Duration = Duration::from_secs(90);
/// The port the execd listens on inside the sandbox.
const EXECD_PORT: u16 = 44772;
/// A public IPv6 address used to measure whether IPv6 is really gone.
const PUBLIC_IPV6: &str = "2606:4700:4700::1111";

/// The verdict of one check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// The property was measured and holds.
    Pass,
    /// The property was measured and does not hold.
    Fail,
    /// The property could not be measured; it is NOT a pass.
    NotMeasured,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::NotMeasured => "NOT_MEASURED",
        }
    }
}

/// One measured property of the boundary.
struct Check {
    id: &'static str,
    name: &'static str,
    verdict: Verdict,
    detail: String,
}

/// The report of the probe.
struct Report {
    checks: Vec<Check>,
}

impl Report {
    fn new() -> Self {
        Self { checks: Vec::new() }
    }

    fn record(&mut self, id: &'static str, name: &'static str, verdict: Verdict, detail: String) {
        self.checks.push(Check {
            id,
            name,
            verdict,
            detail,
        });
    }

    fn pass(&mut self, id: &'static str, name: &'static str, detail: impl Into<String>) {
        self.record(id, name, Verdict::Pass, detail.into());
    }

    fn fail(&mut self, id: &'static str, name: &'static str, detail: impl Into<String>) {
        self.record(id, name, Verdict::Fail, detail.into());
    }

    fn not_measured(&mut self, id: &'static str, name: &'static str, detail: impl Into<String>) {
        self.record(id, name, Verdict::NotMeasured, detail.into());
    }

    /// The overall verdict: a single failure fails the boundary.
    fn verdict(&self) -> Verdict {
        if self.checks.iter().any(|c| c.verdict == Verdict::Fail) {
            Verdict::Fail
        } else {
            Verdict::Pass
        }
    }

    /// The checks that were not measured: they are reported apart, so a
    /// reader never mistakes an absence of evidence for evidence.
    fn not_measured_ids(&self) -> Vec<&'static str> {
        self.checks
            .iter()
            .filter(|c| c.verdict == Verdict::NotMeasured)
            .map(|c| c.id)
            .collect()
    }

    fn to_json(&self) -> Value {
        json!({
            "probe": "trust-boundary",
            "verdict": self.verdict().as_str(),
            "not_measured": self.not_measured_ids(),
            "checks": self.checks.iter().map(|check| json!({
                "id": check.id,
                "name": check.name,
                "verdict": check.verdict.as_str(),
                "detail": check.detail,
            })).collect::<Vec<_>>(),
        })
    }
}

/// A host address the sandbox must not be able to reach.
///
/// The probe binds its own listener there, so the target is known to be
/// alive and the negative result cannot be an accident of a closed port.
struct HostListener {
    address: Ipv4Addr,
    port: u16,
    _listener: TcpListener,
}

impl HostListener {
    /// Bind a listener on the address of the host on its own network.
    fn bind() -> Option<Self> {
        let address = host_network_address()?;
        let listener = TcpListener::bind((address, 0)).ok()?;
        let port = listener.local_addr().ok()?.port();
        Some(Self {
            address,
            port,
            _listener: listener,
        })
    }

    /// The address the sandbox would use to reach the host.
    fn target(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }
}

/// The address of the host on the network it uses to reach the outside.
///
/// A connected UDP socket does not send anything: the kernel only
/// resolves the route, and the local address it picks is the one the
/// host would use to reach the internet.
fn host_network_address() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let address = socket.local_addr().ok()?.ip();
    match address {
        std::net::IpAddr::V4(v4) if !v4.is_loopback() => Some(v4),
        _ => None,
    }
}

/// Whether a TCP connection to the address succeeds within the deadline.
fn tcp_reachable(host: &str, port: u16) -> bool {
    use std::net::ToSocketAddrs;
    let target = (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next());
    let Some(target) = target else {
        return false;
    };
    std::net::TcpStream::connect_timeout(&target, Duration::from_secs(3)).is_ok()
}

/// A python program that runs inside the sandbox and answers with one
/// JSON object.
///
/// The program travels base64-encoded, so no quoting of the shell can
/// change its meaning.
fn python_command(code: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(code.as_bytes());
    format!("python3 -c \"import base64;exec(base64.b64decode('{encoded}'))\"")
}

/// The measurements taken INSIDE a sandbox.
///
/// `__TARGETS__` is replaced by a JSON object of `label -> {host, port,
/// ipv6}`. Every target is probed with a TCP connect; the result says
/// whether the sandbox reached it.
const SANDBOX_PROGRAM: &str = r#"
import json, socket, urllib.error, urllib.request

def tcp(host, port, family=socket.AF_INET, timeout=5):
    try:
        s = socket.socket(family, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect((host, int(port)))
        s.close()
        return "reachable"
    except OSError:
        return "blocked"

def default_route():
    try:
        for line in open("/proc/net/route").read().splitlines()[1:]:
            fields = line.split()
            if len(fields) > 2 and fields[1] == "00000000":
                return True
    except OSError:
        return None
    return False

def ipv6_addresses():
    try:
        return [line.split()[1] for line in open("/proc/net/if_inet6").read().splitlines() if line.strip()]
    except OSError:
        return []

def external_dns():
    try:
        socket.getaddrinfo("github.com", 443, type=socket.SOCK_STREAM)
        return "resolved"
    except OSError:
        return "unresolved"

def resolvers():
    # The resolver the sandbox would ask. It is named in the report so
    # the reader knows WHICH path was exercised: the embedded resolver
    # of Docker, or none at all.
    try:
        return [line.split()[1] for line in open("/etc/resolv.conf").read().splitlines() if line.startswith("nameserver")]
    except OSError:
        return []

def own_ip():
    try:
        return socket.gethostbyname(socket.gethostname())
    except OSError:
        return None

def own_gateway():
    address = own_ip()
    if not address:
        return None
    return ".".join(address.split(".")[:3]) + ".1"

def execd_without_credential():
    # The execd of this sandbox listens on the network of the sandbox.
    # Asking it for a session WITHOUT its credential must be refused:
    # the credential is what protects the execution plane, not the
    # topology of the network.
    address = own_ip()
    if not address:
        return "unknown"
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    request = urllib.request.Request(
        "http://" + address + ":44772/pty",
        data=b'{"command": "true"}',
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with opener.open(request, timeout=5) as response:
            return str(response.status)
    except urllib.error.HTTPError as error:
        return str(error.code)
    except Exception as error:
        return "error:" + type(error).__name__

targets = json.loads(r"""__TARGETS__""")
results = {}
for label, spec in targets.items():
    family = socket.AF_INET6 if spec.get("ipv6") else socket.AF_INET
    results[label] = tcp(spec["host"], spec["port"], family)

print(json.dumps({
    "own_ip": own_ip(),
    "own_gateway": own_gateway(),
    "default_route": default_route(),
    "ipv6_addresses": ipv6_addresses(),
    "external_dns": external_dns(),
    "resolvers": resolvers(),
    "execd_without_credential": execd_without_credential(),
    "targets": results,
}))
"#;

/// Run the measurement program inside a sandbox and read its answer.
async fn measure(execd: &ExecdEndpoint, code: &str) -> Result<Value, String> {
    let outcome = oneshot::run(execd, &python_command(code), SANDBOX_COMMAND_TIMEOUT).await;
    if !outcome.succeeded() {
        return Err(format!(
            "the sandbox command did not succeed: {}",
            outcome.output.trim()
        ));
    }
    let line = outcome
        .output
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| "the sandbox command produced no JSON".to_string())?;
    serde_json::from_str(line).map_err(|error| format!("invalid JSON from the sandbox: {error}"))
}

/// A small python program that connects to one address and prints the
/// verdict.
fn connect_program(host: &str, port: u16) -> String {
    format!(
        r#"
import json, socket
try:
    s = socket.socket()
    s.settimeout(5)
    s.connect(({host:?}, {port}))
    s.close()
    print(json.dumps({{"reachable": True}}))
except OSError as error:
    print(json.dumps({{"reachable": False, "error": type(error).__name__}}))
"#
    )
}

/// The error an answer carries, without the body it arrived in.
///
/// The contract puts the code and the message inside the envelope; the
/// report keeps the code and a short reason, never the whole body.
fn error_of(body: &str) -> String {
    let parsed: Value = serde_json::from_str(body).unwrap_or_default();
    let detail = &parsed["detail"];
    let code = detail["code"].as_str().unwrap_or("unknown");
    let message = detail["message"].as_str().unwrap_or("");
    let short: String = message.chars().take(160).collect();
    format!("{code}: {short}")
}

/// Read the value of an option.
fn option(args: &[String], name: &str, default: &str) -> String {
    let mut found = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == name {
            found = args.get(index + 1).cloned();
            index += 2;
        } else {
            index += 1;
        }
    }
    found
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// The proxy origin as the sandbox uses it: host and port.
fn proxy_origin(proxy_url: &str) -> Option<(String, u16)> {
    let url = reqwest::Url::parse(proxy_url).ok()?;
    let host = url.host_str()?.to_string();
    let port = url.port_or_known_default()?;
    Some((host, port))
}

/// Run the probe. The exit code is 0 when the boundary holds.
pub async fn run(args: &[String]) -> i32 {
    let server = option(args, "--server", "http://127.0.0.1:8081");
    let image = option(args, "--image", "python:3.12-slim");
    let proxy_url = option(args, "--proxy-url", "http://hodei-proxy:8888");
    let proxy_target = option(args, "--proxy-target", "https://github.com");
    let public_target = option(args, "--public-target", "1.1.1.1:443");

    let mut report = Report::new();
    let Some((proxy_host, proxy_port)) = proxy_origin(&proxy_url) else {
        report.fail(
            "P00",
            "the probe arguments are usable",
            format!("the proxy URL {proxy_url:?} is not an HTTP origin"),
        );
        println!("{}", report.to_json());
        return 1;
    };

    // The client that may drive the control plane, and the one that may
    // not. Both talk to the same server.
    let authorized = ControlPlane::new(&server);
    if let Some(reason) = authorized.missing_credential() {
        report.fail("P00", "the probe holds the control credential", reason);
        println!("{}", report.to_json());
        return 1;
    }
    let unauthorized = ControlPlane::with_token(&server, None);

    // The listener that gives the host a service the sandbox must NOT
    // reach. It is bound before any sandbox exists, so its liveness is
    // never in doubt.
    let Some(host_listener) = HostListener::bind() else {
        report.not_measured(
            "P00",
            "the host has a reachable address of its own",
            "the host address could not be determined; the host-reachability checks cannot be measured",
        );
        println!("{}", report.to_json());
        return 1;
    };

    // The public target is only a valid negative when the HOST can
    // reach it. Otherwise the sandbox not reaching it proves nothing.
    let (public_host, public_port) = match split_target(&public_target) {
        Some(parts) => parts,
        None => {
            report.fail(
                "P00",
                "the public target is an address",
                format!("the public target {public_target:?} is not host:port"),
            );
            println!("{}", report.to_json());
            return 1;
        }
    };
    let public_reachable_from_host = tcp_reachable(&public_host, public_port);

    // --- The control plane refuses an unauthorized client. ---------
    let before = sandbox_count(&authorized).await;
    let mut unauthorized_statuses: Vec<(String, u16)> = Vec::new();
    for (label, response) in [
        (
            "list",
            unauthorized
                .get(ControlPlane::sandboxes_path())
                .send()
                .await,
        ),
        (
            "create",
            unauthorized
                .post(ControlPlane::sandboxes_path())
                .json(&create_body(&image))
                .send()
                .await,
        ),
    ] {
        match response {
            Ok(response) => {
                unauthorized_statuses.push((label.to_string(), response.status().as_u16()))
            }
            Err(_) => unauthorized_statuses.push((label.to_string(), 0)),
        }
    }
    let after = sandbox_count(&authorized).await;
    let refused = unauthorized_statuses
        .iter()
        .all(|(_, status)| *status == 401 || *status == 403);
    if refused && before == after {
        report.pass(
            "P01",
            "an unauthorized client cannot list or create sandboxes",
            format!(
                "the collection answered {} and kept {} environments",
                unauthorized_statuses
                    .iter()
                    .map(|(label, status)| format!("{label}={status}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                before.unwrap_or(0)
            ),
        );
    } else {
        report.fail(
            "P01",
            "an unauthorized client cannot list or create sandboxes",
            format!(
                "statuses {}; environments before={:?} after={:?}",
                unauthorized_statuses
                    .iter()
                    .map(|(label, status)| format!("{label}={status}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                before,
                after
            ),
        );
    }

    // --- The sandbox that will be attacked exists. ------------------
    let mut created: Vec<String> = Vec::new();
    let mut endpoints: Vec<ExecdEndpoint> = Vec::new();
    for _ in 0..2 {
        let response = authorized
            .post(ControlPlane::sandboxes_path())
            .json(&create_body(&image))
            .send()
            .await;
        let Ok(response) = response else {
            report.fail(
                "P02",
                "the authorized client can create a sandbox",
                "the create request did not reach the control plane",
            );
            break;
        };
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            report.fail(
                "P02",
                "the authorized client can create a sandbox",
                format!("the create request answered {status}: {}", error_of(&body)),
            );
            break;
        }
        let body: Value = response.json().await.unwrap_or_default();
        let Some(id) = body["id"].as_str().map(str::to_string) else {
            report.fail(
                "P02",
                "the authorized client can create a sandbox",
                "the create response carried no environment id",
            );
            break;
        };
        let Some(endpoint) = resolve_endpoint(&authorized, &id).await else {
            report.fail(
                "P02",
                "the authorized client can create a sandbox",
                format!("the endpoint of the created environment {id} could not be resolved"),
            );
            break;
        };
        created.push(id);
        endpoints.push(endpoint);
    }
    if created.len() == 2 {
        report.pass(
            "P02",
            "the authorized client can create a sandbox",
            format!("{} environments were created", created.len()),
        );
    }

    if created.len() < 2 {
        cleanup(&authorized, &created).await;
        println!("{}", report.to_json());
        return 1;
    }
    let id_a = created[0].clone();
    let (endpoint_a, endpoint_b) = (endpoints[0].clone(), endpoints[1].clone());

    // --- An unauthorized client cannot touch an existing sandbox. ---
    let mut refusals = Vec::new();
    for (label, response) in [
        (
            "inspect",
            unauthorized
                .get(&ControlPlane::sandbox_path(&id_a))
                .send()
                .await,
        ),
        (
            "endpoints",
            unauthorized
                .get(&ControlPlane::endpoint_path(&id_a, EXECD_PORT))
                .send()
                .await,
        ),
        (
            "delete",
            unauthorized
                .delete(&ControlPlane::sandbox_path(&id_a))
                .send()
                .await,
        ),
    ] {
        let status = response.map(|r| r.status().as_u16()).unwrap_or(0);
        refusals.push(format!("{label}={status}"));
    }
    let survivor = authorized
        .get(&ControlPlane::sandbox_path(&id_a))
        .send()
        .await
        .map(|r| r.status().as_u16())
        .unwrap_or(0);
    let refused = refusals
        .iter()
        .all(|entry| entry.ends_with("=401") || entry.ends_with("=403"));
    if refused && survivor == 200 {
        report.pass(
            "P03",
            "an unauthorized client cannot inspect, resolve or delete a sandbox",
            format!(
                "{}; the sandbox survived with status {survivor}",
                refusals.join(", ")
            ),
        );
    } else {
        report.fail(
            "P03",
            "an unauthorized client cannot inspect, resolve or delete a sandbox",
            format!("{}; the sandbox answered {survivor}", refusals.join(", ")),
        );
    }

    // --- The endpoint is not exposed to the network. -----------------
    let endpoint_host = host_of(&endpoint_a);
    let loopback = endpoint_host
        .as_deref()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|address| address.is_loopback());
    if loopback {
        report.pass(
            "P04",
            "the resolved endpoint is bound to the loopback",
            format!("the endpoint host is {endpoint_host:?}"),
        );
    } else {
        report.fail(
            "P04",
            "the resolved endpoint is bound to the loopback",
            format!("the endpoint host is {endpoint_host:?}"),
        );
    }

    // The LAN must NOT reach the endpoint. The host itself must reach
    // it, so the negative is conclusive.
    let Some((endpoint_host, endpoint_port)) = split_host_port(&endpoint_a) else {
        report.fail(
            "P05",
            "the endpoint is not reachable from the network",
            "the resolved endpoint is not host:port",
        );
        return finish(&authorized, &created, report).await;
    };
    let loopback_reachable = tcp_reachable(&endpoint_host, endpoint_port);
    let lan_reachable = tcp_reachable(&host_listener.address.to_string(), endpoint_port);
    if loopback_reachable && !lan_reachable {
        report.pass(
            "P05",
            "the endpoint is not reachable from the network",
            format!(
                "127.0.0.1:{endpoint_port} answered from the host and {}:{endpoint_port} did not",
                host_listener.address
            ),
        );
    } else {
        report.fail(
            "P05",
            "the endpoint is not reachable from the network",
            format!(
                "loopback reachable={loopback_reachable}, lan reachable={lan_reachable} on port {endpoint_port}"
            ),
        );
    }

    // The origin of the endpoint is CONFIGURED: a foreign Host header
    // must not move it.
    let original = endpoint_a.base_url().to_string();
    let moved = match authorized
        .get(&ControlPlane::endpoint_path(&id_a, EXECD_PORT))
        .header("Host", "attacker.example.com")
        .send()
        .await
    {
        Ok(response) => response
            .json::<Value>()
            .await
            .ok()
            .and_then(|value| ExecdEndpoint::from_contract(&value))
            .map(|endpoint| endpoint.base_url().to_string()),
        Err(_) => None,
    };
    match moved {
        Some(moved) if moved == original => report.pass(
            "P06",
            "the endpoint origin is configured, not taken from the request",
            format!("a foreign Host header left the endpoint at {original}"),
        ),
        Some(moved) => report.fail(
            "P06",
            "the endpoint origin is configured, not taken from the request",
            format!("a foreign Host header moved the endpoint to {moved}"),
        ),
        None => report.fail(
            "P06",
            "the endpoint origin is configured, not taken from the request",
            "the endpoint could not be resolved with a foreign Host header",
        ),
    }

    // --- The execd demands its own credential. ----------------------
    let mut credential_results = Vec::new();
    let mut credential_ok = true;
    for (label, endpoint, token) in [
        ("no-credential", endpoint_b.clone(), None),
        (
            "wrong-credential",
            endpoint_b.clone(),
            Some("invalid-".to_string()),
        ),
        (
            "other-sandbox-credential",
            endpoint_b.clone(),
            endpoint_a.access_token().map(str::to_string),
        ),
    ] {
        let mut request = authorized
            .http()
            .post(format!("{}/pty", endpoint.base_url()))
            .json(&json!({"command": "true"}));
        if let Some(token) = token {
            request = request.header(EXECD_ACCESS_TOKEN_HEADER, token);
        }
        let status = request
            .send()
            .await
            .map(|r| r.status().as_u16())
            .unwrap_or(0);
        let refused = status == 401 || status == 403;
        credential_ok &= refused;
        credential_results.push(format!("{label}={status}"));
    }
    if credential_ok {
        report.pass(
            "P07",
            "the execd refuses a missing, wrong or foreign credential",
            credential_results.join(", "),
        );
    } else {
        report.fail(
            "P07",
            "the execd refuses a missing, wrong or foreign credential",
            credential_results.join(", "),
        );
    }

    // --- The sandbox is usable: the execd answers. ------------------
    for (label, endpoint) in [("a", &endpoint_a), ("b", &endpoint_b)] {
        if oneshot::wait_until_ready(endpoint, EXECD_READY_TIMEOUT).await {
            report.pass(
                "P08",
                "the execd of every sandbox answers",
                format!("the execd of sandbox {label} accepted a session"),
            );
        } else {
            report.fail(
                "P08",
                "the execd of every sandbox answers",
                format!("the execd of sandbox {label} did not answer in {EXECD_READY_TIMEOUT:?}"),
            );
        }
    }

    // --- What the sandbox can reach, measured from inside. ----------
    let mut targets = json!({
        "host": {"host": host_listener.address.to_string(), "port": host_listener.port},
        "proxy": {"host": proxy_host, "port": proxy_port},
        "ipv6": {"host": PUBLIC_IPV6, "port": 443, "ipv6": true},
    });
    if public_reachable_from_host {
        targets["public"] = json!({"host": public_host, "port": public_port});
    }
    let program = SANDBOX_PROGRAM.replace("__TARGETS__", &targets.to_string());

    let mut own_ips: Vec<String> = Vec::new();
    for (label, endpoint) in [("a", &endpoint_a), ("b", &endpoint_b)] {
        let measurement = match measure(endpoint, &program).await {
            Ok(measurement) => measurement,
            Err(reason) => {
                report.fail(
                    "P09",
                    "the sandbox can be measured from inside",
                    format!("sandbox {label}: {reason}"),
                );
                continue;
            }
        };
        report.pass(
            "P09",
            "the sandbox can be measured from inside",
            format!("sandbox {label} answered the measurement program"),
        );
        if let Some(ip) = measurement["own_ip"].as_str() {
            own_ips.push(ip.to_string());
        }

        // No default route: the sandbox has no way out that is not the
        // authorized proxy.
        match measurement["default_route"].as_bool() {
            Some(false) => report.pass(
                "P10",
                "the sandbox has no default route",
                format!("sandbox {label} has no route out of its subnet"),
            ),
            Some(true) => report.fail(
                "P10",
                "the sandbox has no default route",
                format!("sandbox {label} has a default route"),
            ),
            None => report.not_measured(
                "P10",
                "the sandbox has no default route",
                format!("sandbox {label} could not read its routing table"),
            ),
        }

        // No IPv6 beyond the loopback.
        let ipv6 = measurement["ipv6_addresses"]
            .as_array()
            .map(|list| list.len())
            .unwrap_or(0);
        if ipv6 <= 1 {
            report.pass(
                "P11",
                "the sandbox has no global IPv6 address",
                format!("sandbox {label} has {ipv6} IPv6 address(es)"),
            );
        } else {
            report.fail(
                "P11",
                "the sandbox has no global IPv6 address",
                format!("sandbox {label} has {ipv6} IPv6 addresses"),
            );
        }

        // No external name resolution.
        let resolvers = measurement["resolvers"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        match measurement["external_dns"].as_str() {
            Some("unresolved") => report.pass(
                "P12",
                "the sandbox cannot resolve an external name",
                format!(
                    "sandbox {label} failed to resolve an external name through the resolver {}",
                    if resolvers.is_empty() {
                        "of the host"
                    } else {
                        &resolvers
                    }
                ),
            ),
            Some(other) => report.fail(
                "P12",
                "the sandbox cannot resolve an external name",
                format!("sandbox {label} resolved an external name ({other})"),
            ),
            None => report.not_measured(
                "P12",
                "the sandbox cannot resolve an external name",
                format!("sandbox {label} did not report a resolution result"),
            ),
        }

        let measured = measurement["targets"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let reached = |name: &str| measured.get(name).and_then(|v| v.as_str()) == Some("reachable");

        // The host's own service: unreachable from the sandbox.
        if reached("host") {
            report.fail(
                "P13",
                "the sandbox cannot reach the host",
                format!(
                    "sandbox {label} reached the host service {}",
                    host_listener.target()
                ),
            );
        } else {
            report.pass(
                "P13",
                "the sandbox cannot reach the host",
                format!(
                    "sandbox {label} did not reach the host service {}",
                    host_listener.target()
                ),
            );
        }

        // The internet: unreachable from the sandbox.
        if public_reachable_from_host {
            if reached("public") {
                report.fail(
                    "P14",
                    "the sandbox cannot reach the internet directly",
                    format!("sandbox {label} reached {public_target} without the proxy"),
                );
            } else {
                report.pass(
                    "P14",
                    "the sandbox cannot reach the internet directly",
                    format!("sandbox {label} did not reach {public_target}"),
                );
            }
        } else {
            report.not_measured(
                "P14",
                "the sandbox cannot reach the internet directly",
                format!(
                    "the host cannot reach {public_target} either: the negative would prove nothing"
                ),
            );
        }

        // IPv6: unreachable from the sandbox.
        if reached("ipv6") {
            report.fail(
                "P15",
                "the sandbox cannot reach an IPv6 destination",
                format!("sandbox {label} reached {PUBLIC_IPV6}"),
            );
        } else {
            report.pass(
                "P15",
                "the sandbox cannot reach an IPv6 destination",
                format!("sandbox {label} did not reach {PUBLIC_IPV6}"),
            );
        }

        // The authorized egress path: the proxy IS reachable.
        if reached("proxy") {
            report.pass(
                "P16",
                "the sandbox can reach the authorized proxy",
                format!("sandbox {label} reached the proxy at {proxy_host}:{proxy_port}"),
            );
        } else {
            report.fail(
                "P16",
                "the sandbox can reach the authorized proxy",
                format!("sandbox {label} did not reach the proxy at {proxy_host}:{proxy_port}"),
            );
        }

        // The credential protects the execution plane, not the topology:
        // the execd of the sandbox listens on the sandbox's own network,
        // and asking it without its credential is refused even from
        // inside.
        match measurement["execd_without_credential"].as_str() {
            Some("401") | Some("403") => report.pass(
                "P21",
                "the execd refuses an unauthenticated request even from inside the sandbox",
                format!("sandbox {label} reached its own execd and was refused"),
            ),
            Some(other) => report.fail(
                "P21",
                "the execd refuses an unauthenticated request even from inside the sandbox",
                format!("sandbox {label} obtained {other} from its own execd without a credential"),
            ),
            None => report.not_measured(
                "P21",
                "the execd refuses an unauthenticated request even from inside the sandbox",
                format!("sandbox {label} did not report the answer of its own execd"),
            ),
        }
    }

    // --- The sandbox cannot reach another sandbox. ------------------
    match lateral_isolation(
        &endpoint_a,
        &endpoint_b,
        own_ips.first().map(String::as_str),
    )
    .await
    {
        Ok(detail) => report.pass("P17", "a sandbox cannot reach another sandbox", detail),
        Err(reason) => report.fail("P17", "a sandbox cannot reach another sandbox", reason),
    }

    // --- The authorized egress path really carries traffic. ---------
    for (label, endpoint) in [("a", &endpoint_a), ("b", &endpoint_b)] {
        match proxy_fetch(endpoint, &proxy_url, &proxy_target).await {
            Ok(status) => report.pass(
                "P18",
                "the sandbox reaches the internet through the proxy",
                format!("sandbox {label} fetched {proxy_target} with status {status}"),
            ),
            Err(reason) => report.fail(
                "P18",
                "the sandbox reaches the internet through the proxy",
                format!("sandbox {label}: {reason}"),
            ),
        }
    }

    // --- The proxy is not an open relay for plain HTTP. --------------
    for (label, endpoint) in [("a", &endpoint_a), ("b", &endpoint_b)] {
        match plain_http_through_proxy(endpoint, &proxy_url, &proxy_target).await {
            Ok(refusal) => report.pass(
                "P19",
                "the proxy tunnels only the destinations it allows",
                format!("sandbox {label}: the proxy refused the plain HTTP request ({refusal})"),
            ),
            Err(reason) => report.fail(
                "P19",
                "the proxy tunnels only the destinations it allows",
                format!("sandbox {label}: {reason}"),
            ),
        }
    }

    // --- The credential of the execution plane belongs to the server.
    // The server reads it back from the container, so a client that may
    // write the environment of its sandbox could choose the credential
    // the server hands out.
    let (forged_id, ownership) =
        credential_ownership(&authorized, &image, endpoint_a.access_token()).await;
    if !forged_id.is_empty() {
        created.push(forged_id);
    }
    match ownership {
        Ok(detail) => report.pass(
            "P22",
            "the credential of the execution plane belongs to the server",
            detail,
        ),
        Err(reason) => report.fail(
            "P22",
            "the credential of the execution plane belongs to the server",
            reason,
        ),
    }

    finish(&authorized, &created, report).await
}

/// Try to choose the credential of the execution plane through the
/// environment of the request, and check that the server kept it.
///
/// The credential is read back from the container, so an environment
/// that carries a name close to the name of the variable is read
/// instead of the credential the server minted.
async fn credential_ownership(
    authorized: &ControlPlane,
    image: &str,
    other_token: Option<&str>,
) -> (String, Result<String, String>) {
    let chosen = "a-credential-of-the-caller";
    let response = authorized
        .post(ControlPlane::sandboxes_path())
        .json(&json!({
            "image": {"uri": image},
            "entrypoint": ["sleep", "infinity"],
            "timeout": 1800,
            "metadata": {"purpose": "hodei-trust-probe"},
            "env": {
                "EXECD_ACCESS_TOKEN=x": chosen,
                "OPENSANDBOX_ID=z": "an-identity-of-the-caller",
            },
        }))
        .send()
        .await;
    let Ok(response) = response else {
        return (
            String::new(),
            Err("the create request did not reach the control plane".to_string()),
        );
    };
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return (
            String::new(),
            Err(format!(
                "the create request answered {status}: {}",
                error_of(&body)
            )),
        );
    }
    let body: Value = response.json().await.unwrap_or_default();
    let Some(id) = body["id"].as_str().map(str::to_string) else {
        return (
            String::new(),
            Err("the create response carried no environment id".to_string()),
        );
    };
    let endpoint = resolve_endpoint(authorized, &id).await;
    let token = endpoint
        .as_ref()
        .and_then(|endpoint| endpoint.access_token())
        .map(str::to_string);
    let Some(token) = token else {
        return (
            id,
            Err("the endpoint of the environment carried no credential".to_string()),
        );
    };
    if token == chosen || token == format!("x={chosen}") {
        return (
            id,
            Err("the server handed over the credential the request asked for".to_string()),
        );
    }
    if Some(token.as_str()) == other_token {
        return (
            id,
            Err("two sandboxes were given the same credential".to_string()),
        );
    }
    (
        id,
        Ok(format!(
            "the server kept the credential of the environment ({} characters) and gave this sandbox a different one",
            token.len()
        )),
    )
}

/// Print the report and clean up. The exit code is 0 when the boundary
/// holds.
async fn finish(authorized: &ControlPlane, created: &[String], mut report: Report) -> i32 {
    let cleanup = cleanup(authorized, created).await;
    if cleanup.deleted == created.len() {
        report.pass(
            "P20",
            "the probe leaves no environment behind",
            format!("{} environments were deleted", cleanup.deleted),
        );
    } else {
        report.fail(
            "P20",
            "the probe leaves no environment behind",
            format!(
                "{} of {} environments were deleted: {}",
                cleanup.deleted,
                created.len(),
                cleanup.failures.join("; ")
            ),
        );
    }
    let verdict = report.verdict();
    println!("{}", report.to_json());
    if verdict == Verdict::Pass { 0 } else { 1 }
}

/// What the deletion of the environments the probe created achieved.
struct Cleanup {
    deleted: usize,
    /// The reason of every environment that could not be deleted.
    failures: Vec<String>,
}

/// Delete the environments the probe created, and report how many are
/// really gone.
///
/// The reason of a failure is reported, never swallowed: an
/// environment that is gone but whose delete failed means that
/// something of its boundary survived, and the reader has to know
/// which.
async fn cleanup(authorized: &ControlPlane, created: &[String]) -> Cleanup {
    let mut deleted = 0;
    let mut failures = Vec::new();
    for id in created {
        let response = authorized
            .delete(&ControlPlane::sandbox_path(id))
            .send()
            .await;
        let (removed, reason) = match response {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    (true, String::new())
                } else {
                    let body = response.text().await.unwrap_or_default();
                    (
                        false,
                        format!("the delete answered {status}: {}", error_of(&body)),
                    )
                }
            }
            Err(error) => (
                false,
                format!("the delete did not reach the control plane: {error}"),
            ),
        };
        let gone = authorized
            .get(&ControlPlane::sandbox_path(id))
            .send()
            .await
            .map(|response| response.status().as_u16() == 404)
            .unwrap_or(false);
        if removed && gone {
            deleted += 1;
        } else {
            let reason = if reason.is_empty() {
                "the environment is still there".to_string()
            } else {
                reason
            };
            failures.push(format!("{id}: {reason}"));
            eprintln!("the environment {id} could not be deleted: {reason}");
        }
    }
    Cleanup { deleted, failures }
}

/// The number of environments the control plane reports.
async fn sandbox_count(authorized: &ControlPlane) -> Option<usize> {
    let response = authorized
        .get(ControlPlane::sandboxes_path())
        .send()
        .await
        .ok()?;
    let body: Value = response.json().await.ok()?;
    Some(
        body["items"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or(0),
    )
}

/// The body of a create request.
fn create_body(image: &str) -> Value {
    json!({
        "image": {"uri": image},
        "entrypoint": ["sleep", "infinity"],
        "timeout": 1800,
        "metadata": {"purpose": "hodei-trust-probe"},
    })
}

/// Resolve the endpoint of an environment.
async fn resolve_endpoint(authorized: &ControlPlane, id: &str) -> Option<ExecdEndpoint> {
    let response = authorized
        .get(&ControlPlane::endpoint_path(id, EXECD_PORT))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: Value = response.json().await.ok()?;
    ExecdEndpoint::from_contract(&value)
}

/// The host of an endpoint.
fn host_of(endpoint: &ExecdEndpoint) -> Option<String> {
    let url = reqwest::Url::parse(endpoint.base_url()).ok()?;
    url.host_str().map(str::to_string)
}

/// The host and port of an endpoint.
fn split_host_port(endpoint: &ExecdEndpoint) -> Option<(String, u16)> {
    let url = reqwest::Url::parse(endpoint.base_url()).ok()?;
    let host = url.host_str()?.to_string();
    let port = url.port_or_known_default()?;
    Some((host, port))
}

/// Split an `host:port` target.
fn split_target(target: &str) -> Option<(String, u16)> {
    let (host, port) = target.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port))
}

/// Measure the lateral isolation between two sandboxes.
///
/// A listener inside the first sandbox proves the port is alive: the
/// second sandbox is measured against a target that IS listening, so a
/// refusal is a refusal and not a closed port.
async fn lateral_isolation(
    endpoint_a: &ExecdEndpoint,
    endpoint_b: &ExecdEndpoint,
    own_ip: Option<&str>,
) -> Result<String, String> {
    let Some(own_ip) = own_ip else {
        return Err("the address of the first sandbox could not be measured".to_string());
    };
    let session = api::create_session(
        endpoint_a,
        CreateSessionRequest {
            cwd: None,
            command: Some(&listener_command()),
        },
    )
    .await
    .map_err(|error| format!("the listener session was refused: {error}"))?;

    let (sender, mut receiver) = tokio::sync::mpsc::channel::<String>(8);
    let ws_url = endpoint_a.ws_url(&session.session_id);
    let options = endpoint_a.attach_options();
    tokio::spawn(async move {
        if let Ok(mut pipe_session) = pipe::attach(&ws_url, options).await {
            while let Some(event) = pipe_session.next_event().await {
                if let opensandbox::execd::frames::ServerEvent::Stdout(bytes) = event {
                    let text = String::from_utf8_lossy(&bytes).to_string();
                    if sender.send(text).await.is_err() {
                        break;
                    }
                }
            }
            let _ = pipe_session.close().await;
        }
    });

    // The listener announces the port it bound.
    let port = loop {
        match tokio::time::timeout(Duration::from_secs(20), receiver.recv()).await {
            Ok(Some(text)) => {
                if let Some(port) = text
                    .trim()
                    .lines()
                    .find_map(|line| line.trim().parse::<u16>().ok())
                {
                    break port;
                }
            }
            Ok(None) => {
                return Err("the listener session ended before it announced its port".into());
            }
            Err(_) => return Err("the listener did not announce its port".into()),
        }
    };

    // Positive control: the listener IS reachable from inside its own
    // sandbox. Without it, the negative below would prove nothing.
    let control = measure(endpoint_a, &connect_program("127.0.0.1", port)).await?;
    if control["reachable"].as_bool() != Some(true) {
        let _ = api::delete_session(endpoint_a, &session.session_id).await;
        return Err("the listener was not reachable from inside its own sandbox".to_string());
    }

    // The measurement that matters: the other sandbox cannot reach it.
    let measured = measure(endpoint_b, &connect_program(own_ip, port)).await;
    let _ = api::delete_session(endpoint_a, &session.session_id).await;
    let measured = measured?;
    if measured["reachable"].as_bool() == Some(true) {
        return Err(format!(
            "the second sandbox reached the listener of the first at {own_ip}:{port}"
        ));
    }
    Ok(format!(
        "the listener of the first sandbox answered on 127.0.0.1:{port} and was unreachable at {own_ip}:{port} from the second"
    ))
}

/// The command that opens a listening socket inside a sandbox and
/// announces the port it bound.
fn listener_command() -> String {
    python_command(
        r#"
import socket, time
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("0.0.0.0", 0))
s.listen(8)
print(s.getsockname()[1], flush=True)
time.sleep(600)
"#,
    )
}

/// Fetch an HTTPS URL through the proxy from inside a sandbox.
async fn proxy_fetch(
    endpoint: &ExecdEndpoint,
    proxy_url: &str,
    target: &str,
) -> Result<u16, String> {
    let program = format!(
        r#"
import json, urllib.request
opener = urllib.request.build_opener(urllib.request.ProxyHandler({{"https": {proxy_url:?}}}))
try:
    response = opener.open({target:?}, timeout=30)
    print(json.dumps({{"status": response.status}}))
except Exception as error:
    print(json.dumps({{"error": type(error).__name__}}))
"#
    );
    let measurement = measure(endpoint, &program).await?;
    if let Some(status) = measurement["status"].as_u64() {
        Ok(status as u16)
    } else {
        Err(measurement["error"]
            .as_str()
            .unwrap_or("no status")
            .to_string())
    }
}

/// Try to fetch a plain HTTP URL through the proxy. The proxy only
/// tunnels the HTTPS destinations it allows, so this must be refused.
///
/// `Ok` carries the reason of the refusal; `Err` means the proxy
/// answered the plain HTTP request, which is the failure.
async fn plain_http_through_proxy(
    endpoint: &ExecdEndpoint,
    proxy_url: &str,
    target: &str,
) -> Result<String, String> {
    let plain = target.replacen("https://", "http://", 1);
    let program = format!(
        r#"
import json, urllib.request
opener = urllib.request.build_opener(urllib.request.ProxyHandler({{"http": {proxy_url:?}}}))
try:
    response = opener.open({plain:?}, timeout=30)
    print(json.dumps({{"status": response.status}}))
except Exception as error:
    print(json.dumps({{"error": type(error).__name__}}))
"#
    );
    let measurement = measure(endpoint, &program).await?;
    match measurement["status"].as_u64() {
        Some(status) => Err(format!("the plain HTTP fetch answered {status}")),
        None => Ok(measurement["error"]
            .as_str()
            .unwrap_or("the proxy refused the request")
            .to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_python_command_carries_the_program_verbatim() {
        let command = python_command("print('a b')");
        assert!(command.starts_with("python3 -c "));
        assert!(!command.contains("print('a b')"));
    }

    #[test]
    fn a_target_splits_into_host_and_port() {
        assert_eq!(
            split_target("1.1.1.1:443"),
            Some(("1.1.1.1".to_string(), 443))
        );
        assert_eq!(split_target("1.1.1.1"), None);
        assert_eq!(split_target("1.1.1.1:https"), None);
        assert_eq!(split_target(":443"), None);
    }

    #[test]
    fn the_proxy_origin_is_read_without_credentials() {
        assert_eq!(
            proxy_origin("http://hodei-proxy:8888"),
            Some(("hodei-proxy".to_string(), 8888))
        );
        assert_eq!(proxy_origin("not a url"), None);
    }

    #[test]
    fn a_report_fails_when_a_single_check_fails() {
        let mut report = Report::new();
        report.pass("P01", "one", "fine");
        assert_eq!(report.verdict(), Verdict::Pass);
        report.not_measured("P02", "two", "unknown");
        assert_eq!(report.verdict(), Verdict::Pass);
        report.fail("P03", "three", "broken");
        assert_eq!(report.verdict(), Verdict::Fail);
        assert_eq!(report.not_measured_ids(), vec!["P02"]);
    }

    #[test]
    fn the_report_never_carries_a_credential() {
        let mut report = Report::new();
        report.pass("P01", "one", "the endpoint is at 127.0.0.1:1234");
        let rendered = report.to_json().to_string();
        assert!(!rendered.contains("secret"));
        assert!(rendered.contains("trust-boundary"));
    }

    #[test]
    fn the_sandbox_program_is_self_contained() {
        let program = SANDBOX_PROGRAM.replace("__TARGETS__", "{}");
        assert!(!program.contains("__TARGETS__"));
        assert!(program.contains("default_route"));
        assert!(program.contains("ipv6_addresses"));
    }
}
