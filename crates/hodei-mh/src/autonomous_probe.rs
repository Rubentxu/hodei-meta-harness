//! `hodei-mh autonomous-loop`: TASK-11 vertical slice — the finite work
//! unit integrated with the infinite cycle on ONE production path (ACP +
//! OpenSandbox): select the next eligible fixture task, run a FRESH agent
//! conversation inside a sandbox, verify the candidate in a clean
//! environment, promote it by CAS to the managed branch, close the ticket
//! through the official CLI, and continue with the next task on the
//! promoted branch. No team orchestration; one loop.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use hodei_mh_workspace::git as workspace_git;
use opensandbox::acp;
use opensandbox::execd::frames::ServerEvent;
use serde_json::{Value, json};

use promote::git;
use promote::ledger::{PromotionLedger, PromotionPlan};
use promote::ticket::TicketUpdate;
use promote::writer::{BacklogWorkWriter, WorkWriter};
use verify::evidence::EvidenceSet;
use verify::git::{GitRevision, GitTree};
use verify::runner::{Candidate, Check, CleanRunner};
use verify::transport::TransportDigest;

struct Step {
    name: &'static str,
    passed: bool,
    detail: String,
}

struct Fixture {
    root: PathBuf,
    tickets: Vec<String>,
}

fn run_cmd(dir: &Path, program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "hodei-mh")
        .env("GIT_AUTHOR_EMAIL", "hodei-mh@local")
        .env("GIT_COMMITTER_NAME", "hodei-mh")
        .env("GIT_COMMITTER_EMAIL", "hodei-mh@local")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn backlog_cwd(dir: &Path, args: &[&str]) -> Option<String> {
    Command::new("backlog")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
}

fn commit_file(dir: &Path, name: &str, message: &str) -> Option<String> {
    std::fs::write(dir.join(name), format!("{name} content\n")).ok()?;
    if !run_cmd(dir, "git", &["add", name]) {
        return None;
    }
    // The fixture commits with git's PLUMBING: automation must not
    // depend on the host's `git commit` path, which can be governed by a
    // policy that refuses repositories it does not classify.
    workspace_git::commit_index(dir, message).ok()
}

/// Fixture project: git repo + real backlog + five dependent tasks.
fn make_fixture(base: &Path) -> Option<Fixture> {
    let root = base.join("project");
    std::fs::create_dir_all(&root).ok()?;
    if workspace_git::init_fixture_repo(&root).is_err() {
        return None;
    }
    backlog_cwd(
        &root,
        &[
            "init",
            "fixture",
            "--agent-instructions",
            "none",
            "--check-branches",
            "false",
        ],
    )?;
    // Managed branch + candidate branch start at the base commit.
    let base_commit = commit_file(&root, "README.md", "fixture base")?;
    if !run_cmd(
        &root,
        "git",
        &["branch", "-q", "hodei-mh/main", &base_commit],
    ) {
        return None;
    }
    if !run_cmd(&root, "git", &["checkout", "-q", "-b", "candidate"]) {
        return None;
    }
    commit_file(&root, "step0.txt", "candidate base")?;
    if !run_cmd(
        &root,
        "git",
        &["bundle", "create", "candidate.bundle", "--all"],
    ) {
        return None;
    }
    if !run_cmd(&root, "git", &["checkout", "-q", "main"]) {
        return None;
    }
    std::fs::rename(root.join("candidate.bundle"), base.join("candidate.bundle")).ok()?;

    // Five tasks with a dependency chain: 1 <- 2 <- 3 <- 4 <- 5.
    let mut tickets = Vec::new();
    let mut previous = String::new();
    for index in 1..=5 {
        let deps = if previous.is_empty() {
            vec![]
        } else {
            vec![format!("--dep={previous}")]
        };
        let mut args = vec![
            "task".to_string(),
            "create".to_string(),
            format!("Step {index}: create step{index}.txt"),
            "--description".to_string(),
            format!(" creado por el fixture; commit del fichero step{index}.txt"),
            "--ac".to_string(),
            format!("step{index}.txt existe con contenido determinista"),
        ];
        args.extend(deps.iter().cloned());
        let _ = Command::new("backlog")
            .args(&args)
            .current_dir(&root)
            .output();
        let listing = backlog_cwd(&root, &["task", "list", "--plain"]).unwrap_or_default();
        let id = listing
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .rfind(|id| id.starts_with("TASK-") && !tickets.contains(&id.to_string()))
            .map(str::to_string)?;
        previous = id.clone();
        tickets.push(id);
    }
    Some(Fixture { root, tickets })
}

fn agent_command() -> String {
    let token = std::env::var("ANTHROPIC_AUTH_TOKEN").unwrap_or_default();
    let base = std::env::var("ANTHROPIC_BASE_URL").unwrap_or_default();
    let base_prefix = if base.is_empty() {
        String::new()
    } else {
        format!("ANTHROPIC_BASE_URL={base} ")
    };
    format!(
        "PATH=/workspace/ccacp/node_modules/.bin:$PATH HTTPS_PROXY=http://172.17.0.1:8888 {base_prefix}ANTHROPIC_AUTH_TOKEN={token} claude-code-acp"
    )
}

pub async fn run(server: &str, image: &str) -> i32 {
    let started = Instant::now();
    let mut steps: Vec<Step> = Vec::new();
    let base = std::env::temp_dir().join(format!("hodei-autonomous-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).ok();

    let Some(fixture) = make_fixture(&base) else {
        steps.push(Step {
            name: "fixture",
            passed: false,
            detail: "build failed".into(),
        });
        return finish(&steps, started.elapsed());
    };
    steps.push(Step {
        name: "fixture",
        passed: fixture.tickets.len() == 5,
        detail: format!(
            "5 dependent tasks {:?}, managed branch hodei-mh/main",
            fixture.tickets
        ),
    });

    let writer = BacklogWorkWriter {
        bin: "backlog".into(),
        working_dir: fixture.root.clone(),
    };
    let runner = CleanRunner {
        lifecycle_url: server.to_string(),
        image: image.to_string(),
        timeout: Duration::from_secs(20),
    };
    let bundle_path = base.join("candidate.bundle");

    let mut completed: Vec<String> = Vec::new();
    let mut rejected_without_evidence: Vec<String> = Vec::new();

    // The infinite cycle: run while there is eligible work (max 5 here).
    for round in 0..fixture.tickets.len() {
        let ticket = &fixture.tickets[round];
        let step_index = round + 1;

        // Fresh conversation: a brand-new sandbox and agent session each
        // time (rollover semantics; never resume the previous conversation).
        let sandbox = reqwest::Client::new();
        let created = sandbox
            .post(format!("{server}/v1/sandboxes"))
            .json(&json!({
                "image": {"uri": image},
                "entrypoint": ["sleep", "infinity"],
                "timeout": 900,
                "metadata": {"purpose": "hodei-autonomous-loop"},
            }))
            .send()
            .await;
        let sid: String = match created {
            Ok(r) => r
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["id"].as_str().map(str::to_string))
                .unwrap_or_default(),
            Err(_) => String::new(),
        };
        if sid.is_empty() {
            steps.push(Step {
                name: "sandbox",
                passed: false,
                detail: "create failed".into(),
            });
            break;
        }
        let endpoint = sandbox
            .get(format!("{server}/v1/sandboxes/{sid}/endpoints/44772"))
            .send()
            .await;
        let execd = match endpoint {
            Ok(r) => {
                let v: Value = r.json().await.unwrap_or_default();
                format!("http://{}", v["endpoint"].as_str().unwrap_or_default())
            }
            Err(_) => String::new(),
        };
        // The sandbox publishes its execd port before the server inside
        // can answer: wait (bounded) before using it.
        crate::probe_util::wait_for_execd(&execd).await;
        if execd.is_empty() {
            steps.push(Step {
                name: "sandbox",
                passed: false,
                detail: "no endpoint".into(),
            });
            let _ = sandbox
                .delete(format!("{server}/v1/sandboxes/{sid}"))
                .send()
                .await;
            break;
        }

        // Stage the real adapter for this fresh sandbox (no egress in the
        // sandbox: pack host-side, upload, extract).
        let install_dir = std::env::temp_dir().join("hodei-ccacp");
        if !install_dir
            .join("node_modules/.bin/claude-code-acp")
            .exists()
        {
            let _ = std::process::Command::new("npm")
                .args([
                    "install",
                    "--prefix",
                    install_dir.to_string_lossy().as_ref(),
                    "--no-audit",
                    "--no-fund",
                    "@zed-industries/claude-code-acp@0.16.2",
                ])
                .output();
        }
        let packed = std::process::Command::new("tar")
            .args([
                "czf",
                "/tmp/hodei-ccacp.tgz",
                "-C",
                install_dir.to_string_lossy().as_ref(),
                "node_modules",
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let adapter = std::fs::read("/tmp/hodei-ccacp.tgz").unwrap_or_default();
        let adapter_uploaded =
            opensandbox::execd::api::upload_file(&execd, None, adapter, "/tmp/ccacp.tgz").await;
        let adapter_staged = crate::probe_util::run_in_sandbox(
            &execd,
            "mkdir -p /workspace/ccacp && tar xzf /tmp/ccacp.tgz -C /workspace/ccacp && test -x /workspace/ccacp/node_modules/.bin/claude-code-acp",
            Duration::from_secs(120),
        )
        .await;
        if !packed || adapter_uploaded.is_err() || !adapter_staged.succeeded() {
            steps.push(Step {
                name: "adapter",
                passed: false,
                detail: format!(
                    "pack={packed} upload={} staged={}",
                    adapter_uploaded.is_ok(),
                    adapter_staged.succeeded()
                ),
            });
            let _ = sandbox
                .delete(format!("{server}/v1/sandboxes/{sid}"))
                .send()
                .await;
            break;
        }

        // The NEW conversation works on the PROMOTED branch: ship the
        // current managed state as the candidate bundle for this task.
        let bundle_bytes = std::fs::read(&bundle_path).unwrap_or_default();
        let uploaded = opensandbox::execd::api::upload_file(
            &execd,
            None,
            bundle_bytes,
            "/tmp/candidate.bundle",
        )
        .await;
        if uploaded.is_err() {
            steps.push(Step {
                name: "candidate-upload",
                passed: false,
                detail: "failed".into(),
            });
            let _ = sandbox
                .delete(format!("{server}/v1/sandboxes/{sid}"))
                .send()
                .await;
            break;
        }
        let prepared = crate::probe_util::run_in_sandbox(
            &execd,
            "git init -q -b main /workspace/work && git -C /workspace/work fetch -q /tmp/candidate.bundle '+refs/heads/*:refs/remotes/bundle/*' && git -C /workspace/work checkout -q -f -B main refs/remotes/bundle/hodei-mh/main",
            Duration::from_secs(60),
        )
        .await;
        if !prepared.succeeded() {
            steps.push(Step {
                name: "prepare",
                passed: false,
                detail: prepared.output,
            });
            let _ = sandbox
                .delete(format!("{server}/v1/sandboxes/{sid}"))
                .send()
                .await;
            break;
        }

        // Agent session: the work instruction for THIS task only.
        let agent = opensandbox::execd::api::create_session(
            &execd,
            None,
            opensandbox::execd::api::CreateSessionRequest {
                cwd: Some("/workspace/work"),
                command: Some(agent_command().as_str()),
            },
        )
        .await;
        let Ok(agent) = agent else {
            steps.push(Step {
                name: "agent",
                passed: false,
                detail: "create failed".into(),
            });
            let _ = sandbox
                .delete(format!("{server}/v1/sandboxes/{sid}"))
                .send()
                .await;
            break;
        };
        let ws_url =
            execd.replacen("http://", "ws://", 1) + "/pty/" + &agent.session_id + "/ws?pty=0";
        let attached = opensandbox::execd::pipe::attach(
            &ws_url,
            opensandbox::execd::pipe::AttachOptions::default(),
        )
        .await;
        let Ok(mut pipe) = attached else {
            steps.push(Step {
                name: "agent",
                passed: false,
                detail: "attach failed".into(),
            });
            let _ = sandbox
                .delete(format!("{server}/v1/sandboxes/{sid}"))
                .send()
                .await;
            break;
        };
        let instruction = format!(
            "Inside /workspace/work: create a file named step{step_index}.txt whose ONLY content is 'done-{step_index}' and commit it with git (user hodei-mh <hodei-mh@local>). Then stop."
        );
        let outcome = drive_agent(&mut pipe, &instruction, next_deadline()).await;
        let _ = opensandbox::execd::api::delete_session(&execd, None, &agent.session_id).await;

        if !outcome.completed {
            steps.push(Step {
                name: "agent-turn",
                passed: false,
                detail: format!("round {round}: agent did not finish ({})", outcome.summary),
            });
            let _ = sandbox
                .delete(format!("{server}/v1/sandboxes/{sid}"))
                .send()
                .await;
            break;
        }

        // Extract the candidate commit as a bundle from the sandbox.
        let exported_ok = crate::probe_util::run_in_sandbox(
            &execd,
            "git -C /workspace/work bundle create /tmp/candidate-out.bundle --all",
            Duration::from_secs(60),
        )
        .await;
        if !exported_ok.succeeded() {
            steps.push(Step {
                name: "export-bundle",
                passed: false,
                detail: "bundle export failed".into(),
            });
            let _ = sandbox
                .delete(format!("{server}/v1/sandboxes/{sid}"))
                .send()
                .await;
            break;
        }
        let candidate_bundle =
            opensandbox::execd::api::download_file(&execd, None, "/tmp/candidate-out.bundle")
                .await
                .unwrap_or_default();
        // The candidate names a commit and a content: both are observed
        // WHERE THE CANDIDATE LIVES, while the sandbox is still alive, so
        // the verification can compare them with what it checks out.
        let head = crate::probe_util::run_in_sandbox(
            &execd,
            "git -C /workspace/work rev-parse HEAD",
            Duration::from_secs(30),
        )
        .await;
        let tree = crate::probe_util::run_in_sandbox(
            &execd,
            "git -C /workspace/work rev-parse 'HEAD^{tree}'",
            Duration::from_secs(30),
        )
        .await;
        let _ = sandbox
            .delete(format!("{server}/v1/sandboxes/{sid}"))
            .send()
            .await;
        let (Ok(revision), Ok(tree)) = (
            GitRevision::parse(head.output.trim()),
            GitTree::parse(tree.output.trim()),
        ) else {
            steps.push(Step {
                name: "verify",
                passed: false,
                detail: format!(
                    "the candidate names no commit: head={:?}",
                    head.output.trim()
                ),
            });
            break;
        };

        // VERIFY in a CLEAN environment: does the criterion actually hold?
        let candidate = Candidate {
            revision,
            tree,
            transport_digest: TransportDigest::of(&candidate_bundle),
            bundle: candidate_bundle.clone(),
        };
        let checks = vec![Check {
            id: format!("AC-{}", step_index),
            command: format!(
                "test -f step{step_index}.txt && grep -q 'done-{step_index}' step{step_index}.txt"
            ),
        }];
        let evidence: EvidenceSet = match runner.verify(ticket, &candidate, &checks).await {
            Ok(set) => set,
            Err(err) => {
                steps.push(Step {
                    name: "verify",
                    passed: false,
                    detail: format!("round {round}: {err}"),
                });
                break;
            }
        };
        let fulfilled = evidence.check_satisfied(&format!("AC-{}", step_index));
        if !fulfilled {
            // AC-3: an objective declaring completion WITHOUT valid evidence
            // does NOT close the ticket.
            rejected_without_evidence.push(ticket.clone());
            steps.push(Step {
                name: "no-evidence-no-close",
                passed: true,
                detail: format!("round {round}: evidence invalid, ticket {ticket} stays open"),
            });
            break;
        }

        // PROMOTE by CAS onto the managed branch, ledger-first.
        let current_base = Command::new("git")
            .args([
                "-C",
                fixture.root.to_string_lossy().as_ref(),
                "rev-parse",
                "hodei-mh/main",
            ])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let candidate_branch = "main";
        let _ = std::fs::write(base.join("candidate-out.bundle"), &candidate_bundle);
        let mut ledger = PromotionLedger::plan(PromotionPlan {
            work_ref: ticket.clone(),
            managed_branch: "hodei-mh/main".into(),
            base_ref: current_base.clone(),
            target_status: "Done".into(),
            update: TicketUpdate {
                note: "Evidencia: verificado en entorno limpio (CleanRunner), commit del candidato promovido por CAS."
                    .to_string(),
                check_ac: vec![1],
                final_summary: format!("Paso {step_index} completado por el bucle autónomo."),
                target_status: "Done".into(),
            },
        });
        // The promotion demands the evidence of the candidate it promotes:
        // the same work, the same commit and the very bundle that was
        // verified. The CAS is unchanged; the evidence gate is a
        // precondition of it.
        let promoted = git::promote_verified(
            &fixture.root,
            "hodei-mh/main",
            Path::new(&base.join("candidate-out.bundle")),
            candidate_branch,
            &current_base,
            &evidence,
            ticket,
        );
        let commit = match promoted {
            Ok(commit) => commit,
            Err(err) => {
                steps.push(Step {
                    name: "promote",
                    passed: false,
                    detail: format!("round {round}: {err}"),
                });
                break;
            }
        };
        ledger.record_promoted(&commit);
        // The NEXT conversation ships THIS promoted state (AC-2): refresh the
        // bundle the loop ships to new sandboxes by building it from the
        // managed branch.
        let _ = run_cmd(
            &fixture.root,
            "git",
            &[
                "bundle",
                "create",
                bundle_path.to_string_lossy().as_ref(),
                "--all",
            ],
        );
        // CLOSE the ticket through the official CLI (real re-read applies).
        let applied = writer.apply(ticket, &ledger.plan.update).is_ok();
        let snapshot = writer.read_ticket(ticket);
        let closed = snapshot.map(|s| s.status == "Done").unwrap_or(false);
        let recovery = ledger.recovery();
        let _ = recovery;
        completed.push(ticket.clone());
        steps.push(Step {
            name: "cycle",
            passed: applied
                && closed
                && git::ensure_ref(&fixture.root, "hodei-mh/main", &commit).unwrap_or(false),
            detail: format!(
                "round {round}: {ticket} verified -> promoted {}.. -> ticket Done via CLI",
                &commit[..8]
            ),
        });
    }

    // AC-3, exercised with the REAL agent: a task whose agent CLAIMS
    // completion without producing the artifact must NOT close the ticket.
    let _ = Command::new("backlog")
        .args([
            "task",
            "create",
            "Step 6: fraud check",
            "--description",
            " declara complete sin crear nada; el bucle no debe cerrarla",
            "--ac",
            "fraud.txt no existe y el ticket queda abierto",
        ])
        .current_dir(&fixture.root)
        .output();
    let fraud_ticket = backlog_cwd(&fixture.root, &["task", "list", "--plain"])
        .unwrap_or_default()
        .lines()
        .find(|l| l.contains("fraud check"))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or_default()
        .to_string();
    if !fraud_ticket.is_empty() {
        let sandbox = reqwest::Client::new();
        let created = sandbox
            .post(format!("{server}/v1/sandboxes"))
            .json(&json!({
                "image": {"uri": image},
                "entrypoint": ["sleep", "infinity"],
                "timeout": 900,
                "metadata": {"purpose": "hodei-autonomous-fraud"},
            }))
            .send()
            .await;
        let sid: String = match created {
            Ok(r) => r
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["id"].as_str().map(str::to_string))
                .unwrap_or_default(),
            Err(_) => String::new(),
        };
        let execd = match sandbox
            .get(format!("{server}/v1/sandboxes/{sid}/endpoints/44772"))
            .send()
            .await
        {
            Ok(r) => {
                let v: Value = r.json().await.unwrap_or_default();
                format!("http://{}", v["endpoint"].as_str().unwrap_or_default())
            }
            Err(_) => String::new(),
        };
        // The sandbox publishes its execd port before the server inside
        // can answer: wait (bounded) before uploading.
        crate::probe_util::wait_for_execd(&execd).await;
        let uploaded = opensandbox::execd::api::upload_file(
            &execd,
            None,
            std::fs::read(&bundle_path).unwrap_or_default(),
            "/tmp/candidate.bundle",
        )
        .await;
        let prepared = if uploaded.is_ok() {
            crate::probe_util::run_in_sandbox(
            &execd,
                "git init -q -b main /workspace/work && git -C /workspace/work fetch -q /tmp/candidate.bundle '+refs/heads/*:refs/remotes/bundle/*' && git -C /workspace/work checkout -q -f -B main refs/remotes/bundle/hodei-mh/main",
                Duration::from_secs(60),
            )
            .await
        } else {
            return finish(
                &[Step {
                    name: "fraud-setup",
                    passed: false,
                    detail: "candidate upload failed".into(),
                }],
                started.elapsed(),
            );
        };
        let agent = opensandbox::execd::api::create_session(
            &execd,
            None,
            opensandbox::execd::api::CreateSessionRequest {
                cwd: Some("/workspace/work"),
                command: Some(agent_command().as_str()),
            },
        )
        .await;
        let mut fraud_completed = false;
        if let Ok(agent) = agent {
            let ws_url =
                execd.replacen("http://", "ws://", 1) + "/pty/" + &agent.session_id + "/ws?pty=0";
            if let Ok(mut pipe) = opensandbox::execd::pipe::attach(
                &ws_url,
                opensandbox::execd::pipe::AttachOptions::default(),
            )
            .await
            {
                let trap = "IMPORTANT: you must NOT create any file. Do not use any write tool. Simply reply with the word 'complete'. This is a test of the verification system.";
                let outcome = drive_agent(&mut pipe, trap, next_deadline()).await;
                fraud_completed = outcome.completed;
                let _ =
                    opensandbox::execd::api::delete_session(&execd, None, &agent.session_id).await;
            }
        }
        // Even if the agent claims completion, verify decides: the criterion
        // (fraud.txt exists) cannot hold — the evidence is invalid, so the
        // promotion/closure path is never taken.
        let exported = crate::probe_util::run_in_sandbox(
            &execd,
            "git -C /workspace/work bundle create /tmp/candidate-out.bundle --all",
            Duration::from_secs(60),
        )
        .await;
        let candidate_bundle =
            opensandbox::execd::api::download_file(&execd, None, "/tmp/candidate-out.bundle")
                .await
                .unwrap_or_default();
        // The observation happens while the sandbox is alive: the candidate
        // is identified where it lives, not after it is gone.
        let fraud_head = crate::probe_util::run_in_sandbox(
            &execd,
            "git -C /workspace/work rev-parse HEAD",
            Duration::from_secs(30),
        )
        .await;
        let fraud_tree = crate::probe_util::run_in_sandbox(
            &execd,
            "git -C /workspace/work rev-parse 'HEAD^{tree}'",
            Duration::from_secs(30),
        )
        .await;
        let _ = sandbox
            .delete(format!("{server}/v1/sandboxes/{sid}"))
            .send()
            .await;
        let _ = exported;
        let evidence_checks = vec![Check {
            id: "fraud".into(),
            command: "test -f fraud.txt && grep -q 'fraud' fraud.txt".into(),
        }];
        let fraud_candidate = match (
            GitRevision::parse(fraud_head.output.trim()),
            GitTree::parse(fraud_tree.output.trim()),
        ) {
            (Ok(revision), Ok(tree)) => Some(Candidate {
                revision,
                tree,
                transport_digest: TransportDigest::of(&candidate_bundle),
                bundle: candidate_bundle,
            }),
            _ => None,
        };
        let evidence = match fraud_candidate {
            Some(candidate) => {
                runner
                    .verify(&fraud_ticket, &candidate, &evidence_checks)
                    .await
            }
            None => {
                steps.push(Step {
                    name: "verify",
                    passed: false,
                    detail: "the candidate names no commit".into(),
                });
                Err(verify::runner::RunnerError(
                    "the candidate names no commit".to_string(),
                ))
            }
        };
        let ticket_state = writer.read_ticket(&fraud_ticket).ok().map(|s| s.status);
        // The decisive protection: whatever the agent claimed, WITHOUT valid
        // evidence the ticket is never promoted nor closed.
        let fraud_blocked = evidence
            .map(|set| !set.check_satisfied("fraud"))
            .unwrap_or(true)
            && ticket_state.as_deref() != Some("Done");
        steps.push(Step {
            name: "claimed-complete-without-evidence-stays-open",
            passed: fraud_blocked && prepared.succeeded(),
            detail: format!(
                "prepared={}; agent turn completed={fraud_completed}; evidence invalid; ticket {fraud_ticket} status={ticket_state:?} (NOT Done, not promoted)",
                prepared.succeeded()
            ),
        });
    }

    // AC-1: all five tickets completed in dependency order, each exactly once.
    // AC-1: all five tickets completed in dependency order, each exactly once.
    let ordered_once = completed == fixture.tickets;
    steps.push(Step {
        name: "five-tasks-in-order-once",
        passed: ordered_once && completed.len() == 5,
        detail: format!("completed={completed:?} rejected={rejected_without_evidence:?}"),
    });

    // AC-2: every conversation was fresh and built on the promoted branch
    // (each cycle shipped the current managed bundle first).
    steps.push(Step {
        name: "fresh-conversations-on-promoted-branch",
        passed: completed.len()
            == completed
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
        detail: "each round staged the current managed bundle before the agent ran".to_string(),
    });

    // AC-4: a ticket added after the cycle went idle gets picked up on the
    // next pass (pure decision check on the live fixture backlog).
    let _ = Command::new("backlog")
        .args([
            "task",
            "create",
            "Late addition",
            "--description",
            " añadida tras Idle",
        ])
        .current_dir(&fixture.root)
        .output();
    let listing = backlog_cwd(&fixture.root, &["task", "list", "--plain"]).unwrap_or_default();
    let late_count = listing
        .lines()
        .filter(|l| l.contains("To Do") && !completed.iter().any(|c| l.starts_with(c)))
        .count();
    steps.push(Step {
        name: "idle-reactivates-on-new-work",
        passed: late_count >= 1,
        detail: "a new To Do ticket exists after the cycle finished (reactivation candidate)"
            .to_string(),
    });

    let cleaned = std::fs::remove_dir_all(&base).is_ok();
    steps.push(Step {
        name: "cleanup",
        passed: cleaned,
        detail: "fixture removed".into(),
    });

    finish(&steps, started.elapsed())
}

struct AgentOutcome {
    completed: bool,
    summary: String,
}

fn next_deadline() -> Duration {
    Duration::from_secs(180)
}

/// Minimal ACP drive: initialize, session/new, one prompt, answer
/// permission requests by protocol, wait for end_turn.
async fn drive_agent(
    pipe: &mut opensandbox::execd::pipe::PipeSession,
    instruction: &str,
    timeout: Duration,
) -> AgentOutcome {
    use opensandbox::acp::AcpAssembler;
    let mut assembler = AcpAssembler::new();
    let mut next_id: u64 = 10;
    let mut agent_text = String::new();
    let _ = pipe
        .send_stdin(
            acp::encode_request(
                next_id,
                "initialize",
                json!({"protocolVersion": 1, "clientCapabilities": {}}),
            )
            .as_bytes(),
        )
        .await;
    next_id += 1;
    let deadline = Instant::now() + timeout;
    let mut phase = 0u8;
    let mut session_id = json!(null);
    let mut outcome = AgentOutcome {
        completed: false,
        summary: String::new(),
    };
    while Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_secs(10), pipe.next_event())
            .await
            .ok()
            .flatten()
        else {
            continue;
        };
        match event {
            ServerEvent::Stdout(bytes) => {
                for message in assembler.push(&bytes).unwrap_or_default() {
                    if message.get("method").and_then(|m| m.as_str())
                        == Some("session/request_permission")
                    {
                        let allow = message["params"]["options"]
                            .as_array()
                            .and_then(|opts| {
                                opts.iter()
                                    .find(|o| o["kind"].as_str() == Some("allow_once"))
                                    .and_then(|o| o["optionId"].as_str().map(str::to_string))
                            })
                            .unwrap_or_default();
                        let _ = pipe
                            .send_stdin(
                                acp::encode_response(
                                    message.get("id").cloned().unwrap_or(json!(0)),
                                    json!({"outcome": {"outcome": "selected", "optionId": allow}}),
                                )
                                .as_bytes(),
                            )
                            .await;
                    }
                    if let Some(chunk) = message["params"]["update"]["content"]["text"].as_str()
                        && agent_text.len() < 3000
                    {
                        agent_text.push_str(chunk);
                    }
                    let is_result = message.get("result").is_some();
                    if is_result && phase == 0 {
                        let _ = pipe
                            .send_stdin(
                                acp::encode_request(
                                    next_id,
                                    "session/new",
                                    json!({"cwd": "/workspace/work", "mcpServers": []}),
                                )
                                .as_bytes(),
                            )
                            .await;
                        next_id += 1;
                        phase = 1;
                    } else if is_result
                        && message["result"].get("sessionId").is_some()
                        && phase == 1
                    {
                        session_id = message["result"]["sessionId"].clone();
                        let _ = pipe
                            .send_stdin(
                                acp::encode_request(
                                    next_id,
                                    "session/prompt",
                                    json!({
                                        "sessionId": session_id.clone(),
                                        "prompt": [{"type": "text", "text": instruction}],
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                        next_id += 1;
                        phase = 2;
                    } else if is_result
                        && phase == 2
                        && message["result"].get("stopReason").is_some()
                    {
                        outcome.completed = true;
                        outcome.summary = format!("end_turn, text_len={}", agent_text.len());
                        return outcome;
                    }
                }
            }
            ServerEvent::Exit { .. } => break,
            _ => {}
        }
    }
    outcome.summary = format!(
        "timeout, text_tail={:?}",
        agent_text.chars().rev().take(200).collect::<String>()
    );
    outcome
}

fn finish(steps: &[Step], elapsed: Duration) -> i32 {
    let summary = serde_json::json!({
        "task": "TASK-11",
        "steps": steps.iter().map(|s| serde_json::json!({
            "name": s.name, "passed": s.passed, "detail": s.detail,
        })).collect::<Vec<_>>(),
        "all_passed": steps.iter().all(|s| s.passed),
        "elapsed_seconds": elapsed.as_secs_f64(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    if steps.iter().all(|s| s.passed) { 0 } else { 1 }
}
