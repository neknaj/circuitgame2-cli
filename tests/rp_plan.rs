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

const CIRCUIT: &str = "
(using nor)
(module main
  (in a b)
  (out x y)
  (def x (nor a a))
  (def y (nor x b)))
";

fn plan(directory: &Path, circuit: &str, project: &str, board: &str) -> Output {
    fs::write(directory.join("main.ncg2"), circuit).unwrap();
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
    let output = plan(directory.path(), CIRCUIT, PROJECT, BOARD);
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
        let output = plan(directory.path(), CIRCUIT, &project, &board);
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

const UART_CIRCUIT: &str = r#"(using nor)
(module main
  (in r7 r6 r5 r4 r3 r2 r1 r0 received framing overrun ready sent ok button)
  (out button ready r0 r1 r2 r3 r4 r5 r6 r7 received ok))
"#;

const UART_BOARD: &str = r#"board = "pico"
[[gpio]]
direction = "input"
port = "button"
pin = 2

[[gpio]]
direction = "output"
port = "button"
pin = 3

[uart]
instance = 0
tx-pin = 0
rx-pin = 1
baud = 115200
rx-data = ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7"]
rx-valid = "received"
rx-framing = "framing"
rx-overrun = "overrun"
rx-ready = "ready"
tx-data = ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7"]
tx-valid = "received"
tx-ready = "ready"
tx-done = "sent"
tx-ok = "ok"
tx-done-ready = "ok"
"#;

#[test]
fn exports_uart_and_gpio_in_declared_input_order() {
    let directory = tempdir().unwrap();
    let output = plan(directory.path(), UART_CIRCUIT, PROJECT, UART_BOARD);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let table = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = table.lines().collect();
    assert_eq!(
        &lines[..5],
        &[
            "ncg2-rp-uart-v1",
            "board pico",
            "period-us 2000",
            "uart 0 0 1 115200",
            "uart-outputs 1 2 3 4 5 6 7 8 9 10 11",
        ]
    );
    assert_eq!(lines[5], "input-uart rx-data 7");
    assert_eq!(lines[12], "input-uart rx-data 0");
    assert_eq!(lines[19], "input 2");
    assert_eq!(lines[20], "port 14");
    assert_eq!(lines.last().copied(), Some("output 0 3 0"));
}

#[test]
fn rejects_shared_gpio_and_uart_driver_before_writing() {
    let directory = tempdir().unwrap();
    let board = UART_BOARD.replacen("port = \"button\"", "port = \"r0\"", 1);
    let output = plan(directory.path(), UART_CIRCUIT, PROJECT, &board);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("同じ回路入力"), "{error}");
    assert_eq!(error.matches("board.toml[").count(), 2);
}
