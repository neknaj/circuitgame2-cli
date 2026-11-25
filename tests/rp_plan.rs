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

fn i2c_fixture() -> (String, String) {
    let names = |prefix, start, end| {
        (start..end)
            .map(|i| format!("{prefix}{i}"))
            .collect::<Vec<_>>()
    };
    let frame_inputs = names("r", 0, 13);
    let frame_outputs = names("o", 0, 33);
    let inputs = frame_inputs
        .iter()
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let outputs = frame_outputs
        .iter()
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let definitions = frame_outputs
        .iter()
        .map(|port| format!("(def {port} (nor button button))\n"))
        .collect::<String>();
    let circuit = format!(
        "(using nor)\n(module main (in {inputs} button) (out {outputs} indicator)\n{definitions}(def indicator (nor button button)))"
    );
    let board = format!(
        r#"board = "pico2"
[[gpio]]
direction = "input"
port = "button"
pin = 2
[[gpio]]
direction = "output"
port = "indicator"
pin = 3
initial_high = true

[i2c]
instance = 0
sda-pin = 0
scl-pin = 1
frequency-hz = 100000
timeout-micros = 20000
request-ready = "r0"
response-valid = "r1"
response-status = {:?}
read-data = {:?}
request-valid = "o0"
response-ready = "o1"
address = {:?}
write-length = {:?}
read-length = {:?}
write-data = {:?}
"#,
        &frame_inputs[2..5],
        &frame_inputs[5..],
        &frame_outputs[2..9],
        &frame_outputs[9..17],
        &frame_outputs[17..25],
        &frame_outputs[25..]
    );
    (circuit, board)
}

#[test]
fn exports_i2c_settings_and_ports_in_their_respective_orders() {
    let directory = tempdir().unwrap();
    let (circuit, board) = i2c_fixture();
    let output = plan(directory.path(), &circuit, PROJECT, &board);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut expected = String::from(
        "ncg2-rp-i2c-v1\nboard pico2\nperiod-us 2000\ni2c 0 0 1 100000 20000 1\ni2c-outputs",
    );
    for port in (0..33).rev() {
        expected.push_str(&format!(" {port}"));
    }
    expected.push('\n');
    for index in (0..13).rev() {
        expected.push_str(&format!("input-i2c {index}\n"));
    }
    expected.push_str("input 2\n");
    expected.push_str(&"gate 13 13\n".repeat(34));
    for wire in (14..47).rev().chain([47]) {
        expected.push_str(&format!("port {wire}\n"));
    }
    expected.push_str("output 33 3 1\n");
    assert_eq!(output.stdout, expected.as_bytes());
}

#[test]
fn i2c_diagnostics_preserve_the_offending_setting_element() {
    let directory = tempdir().unwrap();
    let (circuit, board) = i2c_fixture();
    let board = format!(
        "# 日本語の前置き\n{}",
        board.replace("\"o4\"", "\"不存在\"")
    );
    let result = plan(directory.path(), &circuit, PROJECT, &board);
    let start = board.find("\"不存在\"").unwrap();
    let end = start + "\"不存在\"".len();
    let error = String::from_utf8(result.stderr).unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert!(
        error.contains(&format!("board.toml[{start}..{end}]")),
        "{error}"
    );
    assert!(error.contains("ポートが見つからない"), "{error}");

    let (_, board) = i2c_fixture();
    let board = board.replace("request-ready = \"r0\"", "request-ready = \"button\"");
    let result = plan(directory.path(), &circuit, PROJECT, &board);
    let first = board.find("[[gpio]]").unwrap();
    let later = board.rfind("\"button\"").unwrap();
    let error = String::from_utf8(result.stderr).unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert!(
        error.contains(&format!(
            "board.toml[{later}..{}]",
            later + "\"button\"".len()
        )),
        "{error}"
    );
    assert!(error.contains(&format!("board.toml[{first}..")), "{error}");
    assert!(error.contains("同じ回路入力を複数の接続が駆動"), "{error}");
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

const SPI_CIRCUIT: &str = include_str!("fixtures/rp-spi/main.ncg2");
const SPI_PROJECT: &str = include_str!("fixtures/rp-spi/circuitgame2.toml");
const SPI_BOARD: &str = include_str!("fixtures/rp-spi/board.toml");

#[test]
fn exports_spi_for_both_boards_in_the_firmware_format() {
    let directory = tempdir().unwrap();
    for (board, order) in [("pico2", "lsb-first"), ("pico", "msb-first")] {
        let settings = SPI_BOARD
            .replace("pico2", board)
            .replace("lsb-first", order);
        let output = plan(directory.path(), SPI_CIRCUIT, SPI_PROJECT, &settings);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut expected = format!(
            "ncg2-rp-spi-v1\nboard {board}\nperiod-us 1000\nspi 0 2 3 4 5 1000000 3 {order} 20000 1\n"
        );
        expected.push_str(
            "spi-outputs 1 1 1 2 2 2 2 2 2 2 2 1 2 2 1 2 2 2\ninput-spi 1\ninput 6\ninput-spi 0\n",
        );
        for index in 2..13 {
            expected.push_str(&format!("input-spi {index}\n"));
        }
        expected.push_str(
            "gate 1 1\ngate 14 14\ngate 18 18\ngate 19 19\nport 15\nport 16\nport 17\nport 7\noutput 3 7 0\n",
        );
        assert_eq!(output.stdout, expected.as_bytes());
    }
}

#[test]
fn spi_reports_precise_setting_ranges_and_related_locations() {
    let directory = tempdir().unwrap();
    let cases = [
        ("mode = 3", "mode = 4", "4", "SPI のモード", None),
        (
            "frequency-hz = 1000000",
            "frequency-hz = 2000",
            "2000",
            "2307〜75000000 Hz",
            None,
        ),
        (
            "cs-pin = 5",
            "cs-pin = 7",
            "7",
            "GP7 を複数",
            Some("先に指定した接続"),
        ),
        (
            "request-ready = \"request-ready\"",
            "request-ready = \"button-signal\"",
            "\"button-signal\"",
            "ポートの方向が逆",
            Some("回路側のポート"),
        ),
    ];
    for (original, replacement, token, reason, related) in cases {
        let board = format!("# 配線の確認\n{}", SPI_BOARD.replace(original, replacement));
        let start = board.find(replacement).unwrap() + replacement.rfind(token).unwrap();
        let output = plan(directory.path(), SPI_CIRCUIT, SPI_PROJECT, &board);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(
            message.contains(&format!("board.toml[{start}..{}]", start + token.len())),
            "{message}"
        );
        assert!(message.contains(reason), "{message}");
        if let Some(related) = related {
            assert!(message.contains(related), "{message}");
        }
    }
}

#[test]
fn spi_rejects_other_peripheral_tables_with_both_locations() {
    let directory = tempdir().unwrap();
    let (_, i2c) = i2c_fixture();
    let uart = &UART_BOARD[UART_BOARD.find("[uart]").unwrap()..];
    let i2c = &i2c[i2c.find("[i2c]").unwrap()..];
    let board = format!("{SPI_BOARD}\n{i2c}\n{uart}");
    let first = board.find("[spi]").unwrap();
    let second = board.find("[i2c]").unwrap();
    let output = plan(directory.path(), SPI_CIRCUIT, SPI_PROJECT, &board);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let message = String::from_utf8(output.stderr).unwrap();
    let mut lines = message.lines();
    let later = lines.next().unwrap();
    let earlier = lines.next().unwrap();
    assert!(
        later.contains(&format!("board.toml[{second}..")),
        "{message}"
    );
    assert!(later.contains("SPI と I²C"), "{message}");
    assert!(
        earlier.contains(&format!("board.toml[{first}..")),
        "{message}"
    );
}
