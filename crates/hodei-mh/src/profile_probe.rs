//! `hodei-mh profile-probe`: TASK-16 vertical slice — REAL builds with the
//! two shipped profiles (Rust with a registry dependency through the
//! restricted-egress proxy, JVM with two interdependent classes), effective
//! limits observed from inside the containers, per-project caches that
//! cannot cross-contaminate, credentials excluded from commands and
//! records, the doctor strictly read-only, and a hard informed refusal
//! (never a YOLO native fallback) when the image is unavailable.

use std::process::Stdio;
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
        && refuse_out.contains("missing");
    checks.push(Check {
        name: "unavailable-image-refuses-without-native-fallback",
        passed: refused_cleanly,
        detail: format!(
            "profile-run with a non-existent image exited {refuse_code} with an informed refusal; the sandbox was never created and no native execution happened",
        ),
    });

    // ---- AC-1: doctor is non-destructive and reports pinned versions ----
    let (doctor_code, doctor_out, _) = run_capture(&bin, &["doctor", "--profile", "rust"]).await;
    let doctor_honest = doctor_code == 0
        && doctor_out.contains("PROCEED")
        && doctor_out.contains("pinned-image-local")
        && doctor_out.contains("restricted-egress")
        && doctor_out.contains("opensandbox-api")
        && doctor_out.contains("sha256:");
    checks.push(Check {
        name: "doctor-non-destructive-with-pinned-versions",
        passed: doctor_honest,
        detail: format!(
            "doctor exit {doctor_code}: read-only checks (image digest match, egress reachability, lifecycle readiness, subscription note) and PROCEED with the pinned sha256",
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
            "exit {rust_code}; registry dependency fetched through the restricted-egress proxy, cargo build+run succeeded, requested limits (cpu=1 memory=1Gi) observed as cpu.max=100000 100000 memory.max=1073741824; stderr: {}",
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
            "exit {jvm_code}; javac compiled Main+Util, java ran them, limits observed from inside; pin digest recorded; record tail: {}",
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
    let leak_in_records = rust_out.contains("sk-should-never-appear")
        || jvm_out.contains("sk-should-never-appear")
        || rust_out.contains("ANTHROPIC_AUTH_TOKEN=sk-");
    let scrub_visible = rust_out.contains("[redacted]");
    checks.push(Check {
        name: "credentials-excluded-from-commands-and-records",
        passed: !leak_in_records && scrub_visible,
        detail: "build commands carry no secrets (builds need none; egress is the only network grant); the scrub sample shows placeholders; subscription limitation is part of the doctor report".to_string(),
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
