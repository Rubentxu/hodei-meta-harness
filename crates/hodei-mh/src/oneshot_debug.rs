//! `hodei-mh oneshot-probe --server URL --exec "command" [--timeout SECS]`
//! Debug tool: run an arbitrary one-shot attach-first against the execd
//! endpoint of an existing sandbox (--endpoint host:port) and print the
//! outcome verbatim.

use std::time::Duration;

use opensandbox::execd::oneshot;

pub async fn run(args: &[String]) -> i32 {
    let flag = |name: &str, default: &str| -> String {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| default.to_string())
    };
    let endpoint = flag("--endpoint", "");
    let exec = flag("--exec", "");
    let timeout: u64 = flag("--timeout", "30").parse().unwrap_or(30);
    if endpoint.is_empty() || exec.is_empty() {
        eprintln!("usage: oneshot-probe --endpoint host:port --exec \"cmd\" [--timeout SECS]");
        return 2;
    }
    let outcome = oneshot::run(
        &format!("http://{endpoint}"),
        &exec,
        Duration::from_secs(timeout),
    )
    .await;
    println!(
        "{}",
        serde_json::json!({
            "exit_code": outcome.exit_code,
            "output": outcome.output,
            "succeeded": outcome.succeeded(),
            // The completeness of the output, measured against what the
            // execd says it produced.
            "expected_bytes": outcome.expected_bytes,
            "missing_bytes": outcome.missing_bytes,
            "replayed": outcome.replayed,
        })
    );
    if outcome.succeeded() { 0 } else { 1 }
}
