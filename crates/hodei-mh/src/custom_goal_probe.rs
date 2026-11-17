//! `hodei-mh custom-goal-probe`: TASK-14 vertical slice — a custom objective
//! with a verifiable termination criterion, completed WITHOUT any ticket
//! tracker (no backlog CLI involved anywhere in this process), finite
//! completion that creates nothing new, and a visible tracker failure that
//! never silently becomes a custom objective.

use std::process::Command;

use goal::source::{
    CustomDecision, CustomGoalState, CustomStatus, GoalSource, ScopeResolution,
    TerminationCriterion, advance_custom, resolve_scope,
};
use verify::result::ExecutionRecord;

struct ProbeCheck {
    name: &'static str,
    passed: bool,
    detail: String,
}

/// Run the user's criterion command locally; the result is the evidence.
fn run_criterion(command: &str, dir: &std::path::Path) -> ExecutionRecord {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .output();
    match output {
        Ok(out) => ExecutionRecord::terminal(
            command,
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout),
        ),
        Err(err) => ExecutionRecord::absent(command, &err.to_string()),
    }
}

pub fn run() -> i32 {
    let mut checks: Vec<ProbeCheck> = Vec::new();
    let work_dir = std::env::temp_dir().join(format!("hodei-custom-goal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir).ok();

    // NO tracker anywhere: this probe never spawns a backlog CLI, reads no
    // tickets, writes no tickets. The custom objective is self-contained.
    let criterion = TerminationCriterion {
        command: format!(
            "grep 'objetivo cumplido' {}/deliverable.txt",
            work_dir.display()
        ),
        expect_exit: 0,
    };
    let source = GoalSource::CustomObjective {
        edition: 1,
        criterion: criterion.clone(),
    };
    let mut state = CustomGoalState {
        edition: 1,
        status: CustomStatus::Active,
    };

    // AC-1a: criterion not yet met -> keep going, no side effects.
    let record = run_criterion(&criterion.command, &work_dir);
    let decision = advance_custom(&state, None, &record, &criterion);
    checks.push(ProbeCheck {
        name: "unmet-criterion-continues-without-side-effects",
        passed: decision == CustomDecision::Continue { edition: 1 } && record.exit_code != Some(0),
        detail: format!("exit={:?} -> Continue", record.exit_code),
    });

    // The work happens (a real deliverable appears), criterion re-checked.
    std::fs::write(work_dir.join("deliverable.txt"), "objetivo cumplido\n").ok();
    let record = run_criterion(&criterion.command, &work_dir);
    let decision = advance_custom(&state, None, &record, &criterion);
    checks.push(ProbeCheck {
        name: "verifiable-criterion-fulfills-with-evidence",
        passed: decision == CustomDecision::Fulfilled { edition: 1 }
            && record.exit_code == Some(0)
            && !record.output.is_empty(),
        detail: format!(
            "exit=0 evidence captured ({:?}...), NO tickets written",
            record.output.trim()
        ),
    });
    state.status = CustomStatus::Fulfilled;

    // AC-2: completion creates NOTHING new; only an explicit new edition
    // reactivates.
    let record = run_criterion(&criterion.command, &work_dir);
    let stays = advance_custom(&state, None, &record, &criterion);
    let reactivated = advance_custom(&state, Some(2), &record, &criterion);
    checks.push(ProbeCheck {
        name: "completion-creates-nothing-reactivation-explicit",
        passed: stays == CustomDecision::StayFulfilled { edition: 1 }
            && reactivated == CustomDecision::Reactivated { edition: 2 },
        detail: "no new objective auto-created; authorized edition 2 -> Reactivated".to_string(),
    });

    // AC-3: a tracker failure in the DEFAULT scope is visible, never
    // silently rewritten as a custom objective.
    let failed = resolve_scope(None, Err("backlog CLI not installed".into()));
    let explicit_custom = resolve_scope(Some(source.clone()), Err("tracker down".into()));
    checks.push(ProbeCheck {
        name: "tracker-failure-visible-never-silent-custom",
        passed: failed
            == ScopeResolution::TrackerFailure {
                reason: "backlog CLI not installed".into(),
            }
            && matches!(explicit_custom, ScopeResolution::Custom { .. }),
        detail: format!(
            "default scope fails loudly ({:?}); explicit custom ignores tracker health",
            match failed {
                ScopeResolution::TrackerFailure { reason } => reason,
                _ => String::new(),
            }
        ),
    });

    let cleaned = std::fs::remove_dir_all(&work_dir).is_ok();
    checks.push(ProbeCheck {
        name: "cleanup",
        passed: cleaned,
        detail: "work dir removed".into(),
    });

    let summary = serde_json::json!({
        "task": "TASK-14",
        "checks": checks.iter().map(|c| serde_json::json!({
            "name": c.name, "passed": c.passed, "detail": c.detail,
        })).collect::<Vec<_>>(),
        "all_passed": checks.iter().all(|c| c.passed),
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
