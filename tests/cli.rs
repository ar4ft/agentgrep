use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agx"));
    command
        .current_dir(root)
        .env("XDG_CACHE_HOME", root.join(".test-cache"));
    command
}

#[test]
fn development_update_is_an_explicit_cli_command_and_never_an_auto_option() {
    let root = tempfile::tempdir().unwrap();
    let help = command(root.path())
        .args(["update-pre", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("unsigned development") && text.contains("--check"));
    let auto = command(root.path())
        .args(["update", "auto", "enable", "--unsigned"])
        .output()
        .unwrap();
    assert_eq!(auto.status.code(), Some(2));
    assert!(auto.stdout.is_empty());
    let unsupported = command(root.path())
        .args(["update-pre", "--team-id", "ABCDE12345"])
        .output()
        .unwrap();
    assert_eq!(unsupported.status.code(), Some(2));
}

#[test]
fn automatic_update_dry_run_renders_schedule_without_writing_configuration() {
    let root = tempfile::tempdir().unwrap();
    let team = option_env!("AGX_APPLE_TEAM_ID")
        .filter(|s| !s.is_empty())
        .unwrap_or("ABCDE12345");
    let result = invoke(
        root.path(),
        &["update", "auto", "enable", "--dry-run", "--team-id", team],
    );
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["enabled"], false);
    assert!(
        result["plist"]
            .as_str()
            .unwrap()
            .contains("<integer>86400</integer>")
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    let invalid = command(root.path())
        .args([
            "update",
            "auto",
            "enable",
            "--dry-run",
            "--team-id",
            team,
            "--interval-hours",
            "0",
        ])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
}
fn invoke(root: &Path, args: &[&str]) -> Value {
    let output = command(root).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn write(root: &Path, name: &str, content: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn exact_smart_case_literal_and_regex() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(root, "app.rs", "AuthToken\nauth_token\na.b\naXb\n");
    assert_eq!(invoke(root, &["search", "authtoken"])["matched_units"], 1);
    assert_eq!(invoke(root, &["search", "AUTHTOKEN"])["matched_units"], 0);
    assert_eq!(
        invoke(root, &["search", "a.b", "-F", "-C", "0"])["matched_units"],
        1
    );
    assert_eq!(
        invoke(root, &["search", "a.b", "-C", "0"])["matched_units"],
        2
    );
    assert_eq!(
        invoke(root, &["search", "authtoken", "--case-sensitive"])["matched_units"],
        0
    );
}

#[test]
fn ignore_hidden_binary_and_glob_scope() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(root, ".gitignore", "ignored/\n");
    write(root, "app.rs", "needle");
    write(root, "app.py", "needle");
    write(root, "ignored/a.rs", "needle");
    write(root, ".private.rs", "needle");
    write(root, "target/a.rs", "needle");
    write(root, "binary", "needle\0");
    write(root, "ascii.pdf", "%PDF-1.4 needle");
    std::fs::write(root.join("nonutf8"), [0xff, 0xfe]).unwrap();
    let result = invoke(root, &["search", "needle"]);
    assert_eq!(result["matched_units"], 2);
    assert_eq!(
        invoke(root, &["search", "needle", "-g", "*.rs"])["matched_units"],
        1
    );
    assert_eq!(
        invoke(root, &["search", "needle", "-g", "!*.py"])["matched_units"],
        1
    );
    assert_eq!(
        invoke(root, &["search", "needle", "--hidden"])["matched_units"],
        3
    );
}

#[test]
fn syntax_returns_smallest_enclosing_declaration_and_deduplicates() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(
        root,
        "auth.py",
        "class Auth:\n    def verify(self):\n        token = 'needle'\n        return token + 'needle'\n",
    );
    let result = invoke(root, &["search", "needle", "--mode", "symbol"]);
    assert_eq!(result["matched_units"], 1);
    let hit = &result["results"][0];
    assert_eq!(hit["symbol"], "verify");
    assert_eq!(hit["start_line"], 2);
    assert_eq!(hit["end_line"], 4);
    assert_eq!(hit["match_lines"], json!([3, 4]));
    assert!(
        hit["content"]
            .as_str()
            .unwrap()
            .starts_with("    def verify")
    );
}

#[test]
fn all_syntax_languages_and_plain_text_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for (name, source, symbol) in [
        ("auth.rs", "fn verify() { let needle = 1; }", "verify"),
        (
            "auth.ts",
            "function verify(): string { return 'needle'; }",
            "verify",
        ),
        (
            "auth.tsx",
            "function verify() { return <div>needle</div>; }",
            "verify",
        ),
        (
            "auth.js",
            "function verify() { return 'needle'; }",
            "verify",
        ),
        (
            "auth.go",
            "package main\nfunc verify() string { return \"needle\" }",
            "verify",
        ),
        (
            "auth.swift",
            "func verify() -> String { return \"needle\" }",
            "verify",
        ),
    ] {
        write(root, name, source);
        let result = invoke(root, &["search", "needle", "--mode", "symbol", "-g", name]);
        assert_eq!(result["results"][0]["symbol"], symbol, "{name}: {result}");
    }
    write(root, "notes.txt", "first\nneedle\nlast");
    assert_eq!(
        invoke(
            root,
            &["search", "needle", "--mode", "symbol", "-g", "notes.txt"]
        )["results"][0]["kind"],
        "lines"
    );
}

#[test]
fn source_budget_is_utf8_safe_and_omissions_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(root, "a.txt", "ééneedle");
    write(root, "b.txt", "needle");
    let result = invoke(root, &["search", "needle", "--budget-bytes", "3"]);
    assert_eq!(result["results"][0]["content"], "é");
    assert_eq!(result["truncated"], true);
    assert_eq!(result["results"][0]["excerpt_truncated"], true);
    assert_eq!(result["matched_units"], 2);
    assert_eq!(result["returned_units"], 1);
    let limited = invoke(root, &["search", "needle", "--limit", "1"]);
    assert_eq!(limited["truncated"], true);
    assert_eq!(limited["results"][0]["excerpt_truncated"], false);
}

#[test]
fn ranked_index_updates_edits_deletions_and_ignores() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(
        root,
        "auth.py",
        "def verifyToken():\n    return 'session authentication'\n",
    );
    write(root, "other.py", "def paint():\n    return 'canvas'\n");
    let found = invoke(
        root,
        &[
            "search",
            "where is session authentication",
            "--mode",
            "ranked",
        ],
    );
    assert_eq!(found["results"][0]["path"], "auth.py");
    write(root, "auth.py", "def paint():\n    return 'canvas'\n");
    assert_eq!(
        invoke(root, &["search", "authentication", "--mode", "ranked"])["matched_units"],
        0
    );
    write(
        root,
        "new.py",
        "def session():\n    return 'authentication'\n",
    );
    assert!(
        invoke(root, &["search", "authentication", "--mode", "ranked"])["matched_units"]
            .as_u64()
            .unwrap()
            > 0
    );
    write(root, ".gitignore", "new.py\n");
    assert_eq!(
        invoke(root, &["search", "authentication", "--mode", "ranked"])["matched_units"],
        0
    );
    std::fs::remove_file(root.join(".gitignore")).unwrap();
    std::fs::remove_file(root.join("new.py")).unwrap();
    assert_eq!(
        invoke(root, &["search", "authentication", "--mode", "ranked"])["matched_units"],
        0
    );
    assert_eq!(
        invoke(root, &["search", "verify token", "--mode", "ranked"])["matched_units"],
        0
    );
}

#[test]
fn identifier_splitting_and_cache_filter_isolation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(
        root,
        "auth.swift",
        "func verifyToken() -> Bool { return true }",
    );
    write(root, "auth.py", "def verify_token():\n    return True");
    let result = invoke(
        root,
        &[
            "search",
            "verify token",
            "--mode",
            "ranked",
            "-g",
            "*.swift",
        ],
    );
    assert_eq!(result["results"][0]["path"], "auth.swift");
    let result = invoke(
        root,
        &["search", "verify token", "--mode", "ranked", "-g", "*.py"],
    );
    assert_eq!(result["results"][0]["path"], "auth.py");
}

#[test]
fn invalid_queries_and_limits_are_errors_but_empty_results_succeed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for args in [
        vec!["search", "["],
        vec!["search", ""],
        vec!["search", "x", "--limit", "0"],
        vec!["search", "x", "--budget-bytes", "0"],
        vec!["search", "x", "--context", "101"],
        vec!["search", "x", "--mode", "hybrid"],
    ] {
        let output = command(root).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let _: Value = serde_json::from_slice(&output.stderr).unwrap();
    }
    assert_eq!(invoke(root, &["search", "missing"])["matched_units"], 0);
}

#[test]
fn read_range_and_root_confinement() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(root, "a.txt", "first\nsecond\nthird");
    assert_eq!(
        invoke(root, &["read", "a.txt", "--start", "2", "--end", "9"])["content"],
        "second\nthird"
    );
    let clipped = invoke(root, &["read", "a.txt", "--budget-bytes", "3"]);
    assert_eq!(clipped["content"], "fir");
    assert_eq!(clipped["excerpt_truncated"], true);
    assert_eq!(clipped["end_line"], 3);
    for path in ["../outside.txt", "/etc/passwd"] {
        assert!(
            !command(root)
                .args(["read", path])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    assert!(
        !command(root)
            .args(["read", "a.txt", "--start", "4"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn broken_cache_recovers_with_current_evidence_and_warning() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "auth.py",
        "def verify():\n    return 'authentication'",
    );
    let index = invoke(root.path(), &["index"]);
    let cache = Path::new(index["cache"].as_str().unwrap());
    for entry in std::fs::read_dir(cache).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("index-")
        {
            std::fs::write(path, "broken").unwrap();
        }
    }
    let result = invoke(
        root.path(),
        &["search", "authentication", "--mode", "ranked"],
    );
    assert_eq!(result["results"][0]["path"], "auth.py");
    assert!(!result["warnings"].as_array().unwrap().is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(cache).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for entry in std::fs::read_dir(cache).unwrap() {
            assert_eq!(
                entry.unwrap().metadata().unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn symlink_outside_root_is_not_searched_or_read() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "secret", "needle");
    std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("link")).unwrap();
    let result = invoke(root.path(), &["search", "needle"]);
    assert_eq!(result["matched_units"], 0);
    assert_eq!(result["incomplete"], false);
    assert!(
        !command(root.path())
            .args(["read", "link"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn large_files_are_skipped() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "large.txt", &"needle".repeat(400000));
    let result = invoke(root.path(), &["search", "needle"]);
    assert_eq!(result["matched_units"], 0);
    assert_eq!(result["incomplete"], true);
    assert!(result["warnings"][0].as_str().unwrap().contains("2 MiB"));
}

#[test]
fn skills_preserve_custom_content_and_harness_instructions() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "AGENTS.md", "Existing instructions");
    let output = command(root.path()).args(["skill"]).output().unwrap();
    assert!(output.status.success());
    for name in [
        ".agents/skills/agentgrep/SKILL.md",
        ".claude/skills/agentgrep/SKILL.md",
    ] {
        assert!(
            std::fs::read_to_string(root.path().join(name))
                .unwrap()
                .contains("name: agentgrep")
        );
    }
    write(root.path(), ".claude/skills/agentgrep/SKILL.md", "Custom");
    assert!(
        !command(root.path())
            .args(["skill"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("AGENTS.md")).unwrap(),
        "Existing instructions"
    );
}

#[test]
fn mcp_handshake_notifications_search_read_and_errors() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "auth.py", "def verify():\n    return 'needle'");
    let mut child = command(root.path())
        .args(["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "not-json").unwrap();
        writeln!(stdin,"{}",json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})).unwrap();
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        for (id, method, params) in [
            (1, "tools/list", json!({})),
            (
                2,
                "tools/call",
                json!({"name":"agx_search","arguments":{"query":"needle","mode":"symbol"}}),
            ),
            (
                3,
                "tools/call",
                json!({"name":"agx_read","arguments":{"path":"auth.py","start_line":1,"end_line":2}}),
            ),
            (
                4,
                "tools/call",
                json!({"name":"agx_read","arguments":{"path":"/etc/passwd","start_line":1,"end_line":2}}),
            ),
            (
                5,
                "tools/call",
                json!({"name":"agx_search","arguments":{"query":"needle","unexpected":true}}),
            ),
            (6, "missing", json!({})),
            (7, "ping", json!({})),
            (8, "tools/call", json!({"name":"agx_map"})),
        ] {
            writeln!(
                stdin,
                "{}",
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
            )
            .unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    let responses: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(responses.len(), 10);
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert_eq!(responses[1]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(responses[2]["result"]["tools"].as_array().unwrap().len(), 3);
    assert_eq!(
        responses[3]["result"]["structuredContent"]["results"][0]["symbol"],
        "verify"
    );
    assert_eq!(responses[4]["result"]["isError"], false);
    assert_eq!(responses[5]["result"]["isError"], true);
    assert_eq!(responses[6]["result"]["isError"], true);
    assert_eq!(responses[7]["error"]["code"], -32601);
    assert_eq!(responses[8]["result"], json!({}));
    assert_eq!(responses[9]["result"]["structuredContent"]["files"], 1);
}
