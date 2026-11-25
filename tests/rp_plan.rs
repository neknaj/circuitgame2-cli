//! CLI の配線表に、検査済みのポート順とピン設定が現れることを確かめる。

#![cfg(not(target_os = "wasi"))]

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::tempdir;

const PROJECT: &str = r#"sources = ["main.ncg2"]
tick-ns = { numerator = 2000000, denominator = 1 }
"#;

const BOARD: &str = r#"board = "pico2"
[[gpio]]
direction = "input"
port = "b"
pin = 7
[[gpio]]
direction = "output"
port = "y"
pin = 3
initial_high = true
[[gpio]]
direction = "input"
port = "a"
pin = 2
[[gpio]]
direction = "output"
port = "y"
pin = 4
[[gpio]]
direction = "output"
port = "x"
pin = 5
"#;

fn plan(directory: &Path, project: &str, board: &str) -> Output {
    fs::write(
        directory.join("main.ncg2"),
        "
(using nor)
(module main
  (in a b)
  (out x y)
  (def x (nor a a))
  (def y (nor x b)))
",
    )
    .unwrap();
    fs::write(directory.join("circuitgame2.toml"), project).unwrap();
    fs::write(directory.join("board.toml"), board).unwrap();
    Command::new(env!("CARGO_BIN_EXE_ncg2"))
        .current_dir(directory)
        .args([
            "rp-plan",
            "--project",
            "circuitgame2.toml",
            "--board",
            "board.toml",
        ])
        .output()
        .unwrap()
}

#[test]
fn exports_declared_port_order_and_output_fanout() {
    let directory = tempdir().unwrap();
    let output = plan(directory.path(), PROJECT, BOARD);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ncg2-rp-gpio-v1\nboard pico2\nperiod-us 2000\ninput 2\ninput 7\ngate 0 0\ngate 2 1\nport 2\nport 3\noutput 0 5 0\noutput 1 3 1\noutput 1 4 0\n");
}

#[test]
fn invalid_period_and_pin_conflict_leave_stdout_empty() {
    let directory = tempdir().unwrap();
    let cases = [
        (
            PROJECT.replace("2000000", "2000001"),
            BOARD.to_owned(),
            "circuitgame2.toml[",
            "整数マイクロ秒",
        ),
        (
            PROJECT.to_owned(),
            BOARD.replace("pin = 5", "pin = 7"),
            "board.toml[",
            "GP7",
        ),
    ];
    for (project, board, location, reason) in cases {
        let output = plan(directory.path(), &project, &board);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains(location) && error.contains(reason),
            "{error}"
        );
        if reason == "GP7" {
            assert_eq!(error.matches(location).count(), 2);
        }
    }
}
