//! The system gate end to end (systems.rs): a borg-ai session whose prompts, touched docs and final
//! message name Orion, Lumen, Beacon and SR3 gets none of their scoped lessons, early or at Stop; a
//! Orion repo still gets Orion lessons. No test makes a model call (BORG_MATCH=bm25).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn setup(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("borg-system-gate-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = root.join("store/collectives/corp");
    std::fs::create_dir_all(&store).unwrap();
    for d in ["borg-ai", "harbor_orion", "home"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let lessons = [
        json!({"id": "Mreset", "origin_ms": 1, "repo": "lumen_handbook", "source": "import", "scope": ["Orion", "harbor_orion"],
            "question": "Does the Orion net drain the per-turn budget place with an explicit consumer instead of a reset arc?",
            "consequence": "a reset arc silently drops the budget token"}),
        json!({"id": "Pref", "origin_ms": 1, "repo": "lumen_handbook", "source": "import", "scope": ["Lumen", "ticket-triage-agent"],
            "question": "Does the Lumen refinement size the story by risk, not by the lesson budget?", "consequence": "wrong size"}),
        json!({"id": "Asr3", "origin_ms": 1, "repo": "lumen_handbook", "source": "import", "scope": ["Orion", "harbor_orion", "Beacon", "SR3"],
            "question": "Does the Beacon or SR3 review summary prompt keep the budget of the lesson flood test?", "consequence": "the summary drifts"}),
        json!({"id": "Gall", "origin_ms": 1, "question": "Is the flood test run with the lesson budget pinned?", "consequence": "flaky numbers"}),
    ];
    std::fs::write(store.join("lessons.jsonl"), lessons.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("borg-ai"), root.join("harbor_orion")], "share": "local"}]}).to_string(),
    )
    .unwrap();
    root
}

fn borg(root: &Path, event: &str, input: &Value, env: &[(&str, &str)]) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(["hook", "claude", event])
        .env_clear()
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        // No fake model here: a model call fails open instead of reaching a real CLI on PATH.
        .env("BORG_PREDICT_MODEL", "off")
        .env("BORG_LEARN_MODEL", "off")
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_MATCH", "bm25")
        .env("BORG_LEARN", "off")
        .envs(env.iter().copied())
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into()
}

/// Every lesson id that reached the drone in `session`: early deliveries and Stop questions.
fn delivered(root: &Path, session: &str) -> Vec<String> {
    let st = std::fs::read_to_string(root.join(format!("store/collectives/corp/state/{session}.ctx"))).unwrap_or_default();
    let mut ids: Vec<String> = st
        .lines()
        .filter_map(|l| l.strip_prefix("#borg-early ").or_else(|| l.strip_prefix("#borg-asked ")))
        // A marker ends in the check that routed it (` @<check>`, hook.rs).
        .flat_map(|ids| ids.split(" @").next().unwrap_or_default().split(','))
        .filter(|i| !i.is_empty())
        .map(String::from)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// The flood: prompts, a touched doc and the final message all name Orion, Lumen, Beacon and SR3.
fn flood(root: &Path, session: &str, env: &[(&str, &str)]) -> Vec<String> {
    let cwd = root.join("borg-ai");
    for p in [
        "Orion and Lumen lessons flood this borg session: check the reset arc budget lesson",
        "Now the Beacon and SR3 review summary lessons also arrive; the budget of the flood test matters",
    ] {
        borg(root, "prompt", &json!({"session_id": session, "cwd": cwd, "prompt": p}), env);
    }
    let doc = cwd.join("skills/lumen-knowledge/orion-net.md");
    borg(root, "post_tool", &json!({"session_id": session, "cwd": cwd, "tool_name": "Read", "tool_input": {"file_path": doc}}), env);
    let stop = json!({"session_id": session, "cwd": cwd, "stop_hook_active": false,
        "last_assistant_message": "Done: Orion reset arc budget, Lumen refinement, Beacon and SR3 summary lessons no longer flood the lesson budget test."});
    borg(root, "stop", &stop, env);
    delivered(root, session)
}

#[test]
fn a_borg_session_naming_other_systems_gets_none_of_their_scoped_lessons() {
    let root = setup("flood");
    let got = flood(&root, "f1", &[]);
    assert_eq!(got, ["Gall"], "only the unscoped lesson: {got:?}");
    // Control: without the gate, today's rule lets the conversation's systems in.
    let control = flood(&root, "f0", &[("BORG_SYSTEM_GATE", "off")]);
    assert!(control.iter().any(|i| i == "Mreset") && control.iter().any(|i| i == "Asr3"), "{control:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_orion_repo_still_gets_orion_lessons() {
    let root = setup("orion");
    let cwd = root.join("harbor_orion");
    let out = borg(&root, "prompt", &json!({"session_id": "m1", "cwd": cwd, "prompt": "Drain the per-turn budget place in the Orion net"}), &[]);
    assert!(out.contains("explicit consumer instead of a reset arc"), "{out}");
    let stop = json!({"session_id": "m1", "cwd": cwd, "stop_hook_active": false,
        "last_assistant_message": "The Beacon and SR3 review summary prompt keeps its budget; the Lumen refinement is untouched."});
    let out = borg(&root, "stop", &stop, &[]);
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert!(v["reason"].as_str().unwrap().contains("SR3 review summary"), "{out}");
    let got = delivered(&root, "m1");
    assert!(got.contains(&"Mreset".to_string()) && got.contains(&"Asr3".to_string()), "{got:?}");
    assert!(!got.contains(&"Pref".to_string()), "Lumen is not this repo's system: {got:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_users_own_map_replaces_the_shipped_example() {
    let root = setup("usermap");
    // systems.json next to the collectives config knows only borg: Orion, Beacon and SR3 are no longer
    // known systems, so the gate leaves their lessons to today's rule.
    std::fs::write(root.join("systems.json"), json!({"systems": {"borg": ["borg"]}, "repos": {}}).to_string()).unwrap();
    let got = flood(&root, "u1", &[]);
    assert!(got.iter().any(|i| i == "Mreset") && got.iter().any(|i| i == "Asr3"), "{got:?}");
    // BORG_SYSTEMS names a file that does not exist: the shipped example applies.
    let none = root.join("none.json");
    let got = flood(&root, "u2", &[("BORG_SYSTEMS", none.to_str().unwrap())]);
    assert_eq!(got, ["Gall"], "{got:?}");
    let _ = std::fs::remove_dir_all(root);
}
