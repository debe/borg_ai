//! The binary's command line: `--version` prints and exits, it never starts the server.

use std::process::{Command, Stdio};

#[test]
fn version_prints_one_line_and_starts_no_server() {
    for spelling in ["--version", "-V"] {
        // A port in use or a server left running would hang or fail here: the version returns first.
        let out = Command::new(env!("CARGO_BIN_EXE_borg-web")).args([spelling, "--port", "1"]).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "borg-web {spelling}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("borg-web {}\n", env!("CARGO_PKG_VERSION")));
    }
}
