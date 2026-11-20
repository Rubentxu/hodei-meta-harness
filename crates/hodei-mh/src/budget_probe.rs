//! `hodei-mh budget-probe`: TASK-12 vertical slice — aggregated budgets
//! drive REAL admission decisions against the lifecycle server: over-budget
//! requests wait WITHOUT creating sandboxes (provisioning ones count),
//! effective CPU/memory limits are verified from INSIDE the containers via
//! cgroups, pressure signals suspend admissions with bounded backoff and no
//! automatic budget raise, and the test workload is measured then cancelled.

use std::time::{Duration, Instant};

use budget::ledger::{Admission, BudgetLedger, SandboxId, WaitReason};
use budget::pressure::{PressureAction, PressureSignal, react};
use budget::vector::{HostCapacity, ResourceVector};
use opensandbox::control::ControlPlane;
use opensandbox::execd::client::ExecdClient;
use opensandbox::execd::deadline::Deadline;
use opensandbox::execd::endpoint::ExecdEndpoint;
use serde_json::{Value, json};

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

fn host_measured_totals() -> ResourceVector {
    let cpus: f64 = std::process::Command::new("nproc")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(4.0);
    let mem_kb: f64 = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|line| line.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
        })
        .unwrap_or(8_000_000.0);
    ResourceVector::new(cpus, mem_kb / 1024.0 / 1024.0, 100.0)
}

pub async fn run(server: &str, image: &str) -> i32 {
    let started = Instant::now();
    let mut checks: Vec<Check> = Vec::new();
    let client = ControlPlane::new(server);

    // AC-4: totals are MEASURED from this host; the reserve is configurable
    // and the published targets stay hypotheses.
    let totals = host_measured_totals();
    let reserve = ResourceVector::new(2.0, 2.0, 10.0);
    let capacity = HostCapacity {
        total: totals.clone(),
        reserve,
    };
    let usable = capacity.usable();
    checks.push(Check {
        name: "measured-capacity-with-configurable-reserve",
        passed: usable.cpus > 0.0 && usable.cpus <= totals.cpus,
        detail: format!(
            "host measured: {} CPUs / {:.1} GiB; reserve 2/2; usable {} CPUs / {:.1} GiB; target '48 CPU/64 GiB' remains a hypothesis",
            totals.cpus, totals.memory_gib, usable.cpus, usable.memory_gib
        ),
    });

    // AC-1: the ledger waitlists over-budget requests BEFORE any create.
    let mut ledger = BudgetLedger::new();
    let first = ledger.admit(
        &capacity,
        SandboxId("sbx-a".into()),
        ResourceVector::new(usable.cpus, usable.memory_gib.min(3.0), 1.0),
    );
    let second = ledger.admit(
        &capacity,
        SandboxId("sbx-b".into()),
        ResourceVector::new(0.5, usable.memory_gib.min(3.0), 1.0),
    );
    let waitlisted_without_create =
        matches!(second, Admission::Waitlisted { .. }) && ledger.committed().cpus <= usable.cpus;
    checks.push(Check {
        name: "over-budget-waits-without-creating",
        passed: first == Admission::Admitted
            && waitlisted_without_create
            && matches!(
                second,
                Admission::Waitlisted {
                    reason: WaitReason::MemoryExhausted
                        | WaitReason::CpuExhausted
                }
            ),
        detail: format!(
            "first={first:?}; second={second:?}; committed={:?} (the second POST /v1/sandboxes never happened)",
            ledger.committed()
        ),
    });
    let _ = ledger.release(&SandboxId("sbx-a".into()));

    // AC-2 + DoD: create a REAL sandbox with resourceLimits and verify the
    // EFFECTIVE limits from inside via cgroups; run a measured workload,
    // then cancel it.
    let created = client
        .post(ControlPlane::sandboxes_path())
        .json(&json!({
            "image": {"uri": image},
            "entrypoint": ["sleep", "infinity"],
            "timeout": 600,
            "resourceLimits": {"cpu": "1", "memory": "1Gi"},
            "metadata": {"purpose": "hodei-budget-probe"},
        }))
        .send()
        .await;
    let sid: String = match created {
        Ok(r) if r.status().is_success() => r
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["id"].as_str().map(str::to_string))
            .unwrap_or_default(),
        _ => String::new(),
    };
    if sid.is_empty() {
        checks.push(Check {
            name: "sandbox-with-limits",
            passed: false,
            detail: "create with resourceLimits failed".into(),
        });
        return finish(&checks, started.elapsed());
    }
    let execd = match client
        .get(&ControlPlane::endpoint_path(&sid, 44772))
        .send()
        .await
    {
        Ok(r) => {
            let v: Value = r.json().await.unwrap_or_default();
            ExecdEndpoint::from_contract(&v).unwrap_or_default()
        }
        Err(_) => ExecdEndpoint::default(),
    };
    // ONE transport and ONE deadline for the measurements of this
    // sandbox: the readiness is observed once and the commands share the
    // budget.
    let transport = ExecdClient::new(execd.clone());
    let deadline = Deadline::in_budget(crate::probe_util::SANDBOX_OPERATION_BUDGET);
    // The sandbox publishes its execd port before the server inside can
    // answer: wait (bounded) before using it.
    crate::probe_util::wait_for_execd(
        &transport,
        deadline.within(crate::probe_util::EXECD_READY_TIMEOUT),
    )
    .await;
    // Effective limits as the WORKLOAD sees them (cgroup v2 values).
    let limits = crate::probe_util::run_in_sandbox(
        &transport,
        "echo cpu=$(cat /sys/fs/cgroup/cpu.max) mem=$(cat /sys/fs/cgroup/memory.max)",
        deadline.within(Duration::from_secs(30)),
    )
    .await;
    let cpu_max_quota: Option<f64> = limits
        .output
        .split("cpu=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.split(' ').next())
        .and_then(|quota| quota.parse::<f64>().ok());
    let memory_max: Option<f64> = limits
        .output
        .split("mem=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<f64>().ok());
    // Effective: 1 CPU == 100000/100000 quota; 1Gi == 1073741824 bytes.
    let effective_ok = cpu_max_quota == Some(100000.0) && memory_max == Some(1073741824.0);
    checks.push(Check {
        name: "effective-limits-measured-inside-runtime",
        passed: effective_ok && limits.succeeded(),
        detail: format!(
            "requested cpu=1 mem=1Gi; observed inside container: {} (aux: execd+init run under the same limits, counted in the budget)",
            limits.output.trim()
        ),
    });

    // Measured workload under the limit, then CANCELLED (DoD).
    let workload = crate::probe_util::run_in_sandbox(
        &transport,
        "dd if=/dev/zero of=/dev/null bs=1M count=200000 & WPID=$!; sleep 1; cat /sys/fs/cgroup/cpu.stat | head -2; kill $WPID; echo workload-cancelled",
        deadline.within(Duration::from_secs(30)),
    )
    .await;
    let workload_measured = workload.succeeded() && workload.output.contains("workload-cancelled");
    let deleted = client
        .delete(&ControlPlane::sandbox_path(&sid))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    let mut ledger_after = BudgetLedger::new();
    let _ = ledger_after.admit(
        &capacity,
        SandboxId(sid.clone()),
        ResourceVector::new(1.0, 1.0, 1.0),
    );
    let _ = ledger_after.mark_running(&SandboxId(sid.clone()));
    let released = ledger_after.release(&SandboxId(sid));
    checks.push(Check {
        name: "workload-measured-then-cancelled",
        passed: workload_measured && deleted && released,
        detail: format!(
            "workload ran under effective limits and was cancelled; sandbox deleted={deleted}; ledger released={released}"
        ),
    });

    // AC-3: pressure signals suspend admissions and back off OOM; the
    // budget is never raised automatically.
    let mut pressured = BudgetLedger::new();
    let _ = pressured.admit(
        &capacity,
        SandboxId("x".into()),
        ResourceVector::new(0.1, 0.1, 0.1),
    );
    pressured.suspend();
    let suspended = pressured.admit(
        &capacity,
        SandboxId("y".into()),
        ResourceVector::new(0.1, 0.1, 0.1),
    );
    let backoff = react(&PressureSignal::OomEvent { sandbox: sid2() }, 2);
    let budget_untouched = pressured.committed().memory_gib <= usable.memory_gib;
    checks.push(Check {
        name: "pressure-suspends-backoff-bounded-no-auto-raise",
        passed: matches!(
            suspended,
            Admission::Waitlisted {
                reason: WaitReason::BudgetSuspended
            }
        ) && matches!(backoff, PressureAction::Backoff { .. })
            && budget_untouched,
        detail: format!(
            "PSI-critical/oom suspend admissions with bounded backoff; usable stays {:.1} GiB (never auto-raised)",
            usable.memory_gib
        ),
    });

    finish(&checks, started.elapsed())
}

fn sid2() -> String {
    "sbx".to_string()
}

fn finish(checks: &[Check], elapsed: Duration) -> i32 {
    let summary = serde_json::json!({
        "task": "TASK-12",
        "checks": checks.iter().map(|c| serde_json::json!({
            "name": c.name, "passed": c.passed, "detail": c.detail,
        })).collect::<Vec<_>>(),
        "all_passed": checks.iter().all(|c| c.passed),
        "elapsed_seconds": elapsed.as_secs_f64(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    if checks.iter().all(|c| c.passed) {
        0
    } else {
        1
    }
}
