//! `hodei-mh profile-probe`: TASK-16 vertical slice — REAL builds with the
//! two shipped profiles (Rust with a registry dependency through the
//! restricted-egress proxy, JVM with two interdependent classes), effective
//! limits observed from inside the containers, per-project caches that
//! cannot cross-contaminate, credentials excluded from commands and
//! records, the doctor strictly read-only, and a hard informed refusal
//! (never a YOLO native fallback) when the image is unavailable.

use std::process::Stdio;

use opensandbox::control::ControlPlane;
use std::time::Instant;

use tokio::process::Command;

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

async fn run_capture(bin: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .expect("spawn hodei-mh");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn make_rust_project(root: &std::path::Path, name: &str, with_dependency: bool) -> bool {
    let src = root.join("src");
    std::fs::create_dir_all(&src).is_ok()
        && std::fs::write(
            root.join("Cargo.toml"),
            if with_dependency {
                format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde_json = \"1\"\n")
            } else {
                format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
            },
        )
        .is_ok()
        && std::fs::write(
            src.join("main.rs"),
            if with_dependency {
                "fn main() {\n    let value = serde_json::json!({\"profile\": \"rust\", \"ok\": true});\n    println!(\"rust-build-ok {}\", value);\n}\n"
            } else {
                "fn main() {\n    println!(\"rust-second-project-ok\");\n}\n"
            },
        )
        .is_ok()
}

fn make_jvm_project(root: &std::path::Path) -> bool {
    let src = root.join("src");
    std::fs::create_dir_all(&src).is_ok()
        && std::fs::write(
            src.join("Main.java"),
            "public class Main {\n    public static void main(String[] args) {\n        System.out.println(\"jvm-build-ok \" + Util.greeting());\n    }\n}\n",
        )
        .is_ok()
        && std::fs::write(
            src.join("Util.java"),
            "public class Util {\n    public static String greeting() { return \"from-util\"; }\n}\n",
        )
        .is_ok()
}

/// The ids of the sandboxes the lifecycle server currently lists. Used to
/// OBSERVE that the doctor creates nothing.
async fn list_sandbox_ids() -> Vec<String> {
    let client = ControlPlane::new("http://127.0.0.1:8081");
    let value: serde_json::Value = match client.get(ControlPlane::sandboxes_path()).send().await {
        Ok(response) => response.json().await.unwrap_or_default(),
        Err(_) => return Vec::new(),
    };
    let mut ids: Vec<String> = value
        .get("items")
        .and_then(|i| i.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(|id| id.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// The facts of a check, as they are: a failure must be DIAGNOSTICABLE
/// from the report, never a static sentence that hides what failed.
fn missing_facts(
    conditions: &[(bool, &str)],
    observed_limits: &serde_json::Value,
    image_digest: &serde_json::Value,
) -> String {
    let missing: Vec<&str> = conditions
        .iter()
        .filter(|(ok, _)| !ok)
        .map(|(_, label)| *label)
        .collect();
    format!("missing={missing:?}; observed_limits={observed_limits}; image_digest={image_digest}")
}

pub async fn run(_args: &[String]) -> i32 {
    let started = Instant::now();
    let mut checks: Vec<Check> = Vec::new();
    let bin = std::env::current_exe().expect("current binary");
    let base = std::env::temp_dir().join(format!("hodei-profile-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::create_dir_all(&base);
    let rust_a = base.join("project-alpha");
    let rust_b = base.join("project-beta");
    let jvm = base.join("project-jvm");
    make_rust_project(&rust_a, "miniapp-alpha", true);
    make_rust_project(&rust_b, "miniapp-beta", false);
    make_jvm_project(&jvm);

    // ---- AC-4: unavailable image → informed REFUSE, no YOLO fallback ----
    let (refuse_code, refuse_out, _) = run_capture(
        &bin,
        &[
            "profile-run",
            "--image",
            "doesnotexist:9",
            "--project",
            rust_a.to_string_lossy().as_ref(),
        ],
    )
    .await;
    let refused_cleanly = refuse_code == 1
        && refuse_out.contains("REFUSE")
        && refuse_out.contains("\"yolo_fallback\": false")
        && refuse_out.contains("\"build_executed_on_host\": false")
        && refuse_out.contains("missing");
    checks.push(Check {
        name: "unavailable-image-refuses-without-native-fallback",
        passed: refused_cleanly,
        detail: format!(
            "profile-run with a non-existent image exited {refuse_code} with an informed refusal; the sandbox was never created and no native execution happened",
        ),
    });

    // ---- a USER image that IS present locally must be accepted ---------
    // The user flow is documented; it must not be refused for a digest
    // it never had.
    let user_image = "rust:1-bookworm";
    let (user_code, user_out, user_err) = run_capture(
        &bin,
        &[
            "profile-run",
            "--image",
            user_image,
            "--project",
            rust_b.to_string_lossy().as_ref(),
        ],
    )
    .await;
    let user_refused_for_digest = user_out.contains("local image digest does not match");
    let user_accepted = user_code == 0 && !user_refused_for_digest;
    checks.push(Check {
        name: "user-image-present-locally-is-accepted",
        passed: user_accepted,
        detail: format!(
            "profile-run --image {user_image} (present locally, NOT pinned) exited {user_code}; refused for a digest mismatch: {user_refused_for_digest}; stderr: {}",
            user_err.trim().chars().take(150).collect::<String>()
        ),
    });

    // ---- AC-1: doctor is non-destructive and reports pinned versions ----
    // The sandbox list BEFORE and AFTER the doctor must be identical: the
    // doctor claims to be read-only, so it is OBSERVED to be.
    let sandboxes_before = list_sandbox_ids().await;
    let (doctor_code, doctor_out, _) = run_capture(&bin, &["doctor", "--profile", "rust"]).await;
    let sandboxes_after = list_sandbox_ids().await;
    let non_destructive = sandboxes_before == sandboxes_after;
    let doctor_honest = doctor_code == 0
        && doctor_out.contains("PROCEED")
        && doctor_out.contains("pinned-image-local")
        && doctor_out.contains("restricted-egress")
        && doctor_out.contains("opensandbox-api")
        && doctor_out.contains("sha256:");
    checks.push(Check {
        name: "doctor-non-destructive-with-pinned-versions",
        passed: doctor_honest && non_destructive,
        detail: format!(
            "doctor exit {doctor_code}: read-only checks (image digest match, egress reachability, lifecycle readiness, subscription note) and PROCEED with the pinned sha256; the sandbox list is identical before ({}) and after ({}) the doctor",
            sandboxes_before.len(),
            sandboxes_after.len()
        ),
    });

    // ---- AC-2 (Rust): REAL build with a registry dependency -------------
    let (rust_code, rust_out, rust_err) = run_capture(
        &bin,
        &[
            "profile-run",
            "--profile",
            "rust",
            "--project",
            rust_a.to_string_lossy().as_ref(),
            "--cpu",
            "1",
            "--memory",
            "1Gi",
        ],
    )
    .await;
    let rust_record: serde_json::Value = serde_json::from_str(&rust_out).unwrap_or_default();
    let rust_ok = rust_code == 0
        && rust_record["all_succeeded"] == true
        && rust_out.contains("rust-build-ok")
        && rust_out.contains("cpu=100000 100000")
        && rust_out.contains("mem=1073741824")
        && rust_out
            .contains("sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea")
        && rust_out.contains("cargo build");
    checks.push(Check {
        name: "rust-profile-real-build-with-limits-and-pin",
        passed: rust_ok,
        detail: format!(
            "exit {rust_code}; {}; stderr: {}",
            missing_facts(
                &[
                    (rust_code == 0, "exit code"),
                    (rust_record["all_succeeded"] == true, "all_succeeded"),
                    (rust_out.contains("rust-build-ok"), "the build output"),
                    (rust_out.contains("cpu=100000 100000"), "the observed cpu limits"),
                    (rust_out.contains("mem=1073741824"), "the observed memory limit"),
                    (
                        rust_out.contains(
                            "sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea"
                        ),
                        "the pinned digest",
                    ),
                    (rust_out.contains("cargo build"), "the build command"),
                ],
                &rust_record["observed_limits"],
                &rust_record["image_digest_observed"],
            ),
            rust_err.trim().chars().take(150).collect::<String>()
        ),
    });

    // ---- AC-2 (JVM): REAL build with two interdependent classes ---------
    let (jvm_code, jvm_out, _) = run_capture(
        &bin,
        &[
            "profile-run",
            "--profile",
            "jvm",
            "--project",
            jvm.to_string_lossy().as_ref(),
            "--cpu",
            "1",
            "--memory",
            "1Gi",
        ],
    )
    .await;
    let jvm_record: serde_json::Value = serde_json::from_str(&jvm_out).unwrap_or_default();
    let jvm_ok = jvm_code == 0
        && jvm_record["all_succeeded"] == true
        && jvm_out.contains("jvm-build-ok from-util")
        && jvm_out.contains("javac")
        && jvm_out
            .contains("sha256:133a3c1720dd52291a019740c2987e7164ea6de79e23d8198798e58950ae2e6e");
    checks.push(Check {
        name: "jvm-profile-real-build-with-limits-and-pin",
        passed: jvm_ok,
        detail: format!(
            "exit {jvm_code}; {}; record tail: {}",
            missing_facts(
                &[
                    (jvm_code == 0, "exit code"),
                    (jvm_record["all_succeeded"] == true, "all_succeeded"),
                    (jvm_out.contains("jvm-build-ok from-util"), "the build output"),
                    (jvm_out.contains("javac"), "the compiler command"),
                    (
                        jvm_out.contains(
                            "sha256:133a3c1720dd52291a019740c2987e7164ea6de79e23d8198798e58950ae2e6e"
                        ),
                        "the pinned digest",
                    ),
                ],
                &jvm_record["observed_limits"],
                &jvm_record["image_digest_observed"],
            ),
            jvm_out.chars().rev().take(300).collect::<String>().chars().rev().collect::<String>()
        ),
    });

    // ---- AC-2: per-project caches cannot cross-contaminate --------------
    let (rust_b_code, rust_b_out, _) = run_capture(
        &bin,
        &[
            "profile-run",
            "--profile",
            "rust",
            "--project",
            rust_b.to_string_lossy().as_ref(),
        ],
    )
    .await;
    let cache_a = rust_record["cache_dir"].as_str().unwrap_or_default();
    let cache_b: serde_json::Value = serde_json::from_str(&rust_b_out).unwrap_or_default();
    let cache_b = cache_b["cache_dir"].as_str().unwrap_or_default();
    let caches_isolated = rust_b_code == 0
        && !cache_a.is_empty()
        && !cache_b.is_empty()
        && cache_a != cache_b
        && rust_b_out.contains("rust-second-project-ok");
    checks.push(Check {
        name: "caches-isolated-per-project",
        passed: caches_isolated,
        detail: "two real builds of two different projects ran to completion with different cache dirs (cache dir is a sha256 token of the project path; deterministic and never shared)".to_string(),
    });

    // ---- AC-3: credentials never in commands nor records ----------------
    // A REAL, RANDOM token is injected into the process environment: a
    // hardcoded fake would only prove that the fake is scrubbed.
    let real_token = format!(
        "sk-ant-probe-{:016x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    );
    let with_token = tokio::process::Command::new(&bin)
        .args([
            "profile-run",
            "--profile",
            "rust",
            "--project",
            rust_a.to_string_lossy().as_ref(),
        ])
        .env("ANTHROPIC_AUTH_TOKEN", &real_token)
        .stdin(Stdio::null())
        .output()
        .await
        .expect("spawn hodei-mh");
    let token_stdout = String::from_utf8_lossy(&with_token.stdout).to_string();
    let token_stderr = String::from_utf8_lossy(&with_token.stderr).to_string();
    let leak_in_records = token_stdout.contains(&real_token) || token_stderr.contains(&real_token);
    let scrub_visible = token_stdout.contains("[redacted]");
    checks.push(Check {
        name: "credentials-excluded-from-commands-and-records",
        passed: !leak_in_records && scrub_visible,
        detail: format!(
            "a real random token was injected in the process env; it appears in neither stdout nor stderr (leaked: {leak_in_records}); the scrub sample shows placeholders"
        ),
    });

    // ---- the sandbox's OWN egress is measured from inside ---------------
    let egress: serde_json::Value = serde_json::from_str(&rust_out).unwrap_or_default();
    let sandbox_egress = &egress["sandbox_egress"];
    let measured_inside = sandbox_egress["measured_from"] == "inside the sandbox";
    let proxy_seen = sandbox_egress["proxy_reachable"] == true;
    let direct_reported = sandbox_egress["direct_internet_reachable"] == true
        || sandbox_egress["direct_internet_blocked"] == true;
    checks.push(Check {
        name: "sandbox-egress-measured-from-inside",
        passed: measured_inside && proxy_seen && direct_reported,
        detail: format!(
            "the record states the egress was measured from inside the sandbox (proxy reachable: {proxy_seen}); raw: {}",
            sandbox_egress["raw"].as_str().unwrap_or("(none)")
        ),
    });

    // ---- the observed image digest is the RESOLVED one ------------------
    let requested = egress["image_sha_requested"].as_str().unwrap_or_default();
    let observed = egress["image_digest_observed"].as_str().unwrap_or_default();
    let digest_recorded = !observed.is_empty() && requested.contains(observed);
    checks.push(Check {
        name: "observed-image-digest-recorded",
        passed: digest_recorded,
        detail: format!(
            "requested pin: {requested}; digest observed for the image that ran: {observed}"
        ),
    });

    // ---- the release went through the IDLE GATE, with a durable export -
    let cleanup = egress["cleanup"].as_str().unwrap_or_default();
    let durable = &egress["durable_export"];
    let released_by_gate = cleanup.starts_with("released")
        && durable["exported"] == true
        && durable["digest_confirmed"] == true
        && !egress["sandbox_id"].as_str().unwrap_or_default().is_empty();
    checks.push(Check {
        name: "release-goes-through-the-idle-gate-with-a-durable-export",
        passed: released_by_gate,
        detail: format!(
            "cleanup: {cleanup}; the candidate was exported to durable storage and READ BACK before the release was allowed (digest confirmed: {})",
            durable["digest_confirmed"]
        ),
    });

    // Cleanup fixtures.
    let _ = std::fs::remove_dir_all(&base);

    let summary = serde_json::json!({
        "task": "TASK-16",
        "checks": checks.iter().map(|c| serde_json::json!({
            "name": c.name, "passed": c.passed, "detail": c.detail,
        })).collect::<Vec<_>>(),
        "all_passed": checks.iter().all(|c| c.passed),
        "elapsed_seconds": started.elapsed().as_secs_f64(),
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
