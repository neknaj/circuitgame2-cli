use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

const BIN: &str = env!("CARGO_BIN_EXE_circuitgame2");

fn cli(args: &[&str]) -> Output {
    Command::new(BIN)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .expect("CLI を起動できる")
}

#[test]
fn readme_nor_example_runs() {
    let checked = cli(&["check", "examples/nor.ncg2", "./examples/nor.ncg2"]);
    assert!(checked.status.success());
    assert_eq!(
        String::from_utf8(checked.stdout).unwrap().trim(),
        "検査成功（1 ファイル）"
    );

    for (input, expected) in [("00", "1"), ("01", "0"), ("10", "0"), ("11", "0")] {
        let output = cli(&[
            "eval",
            "--func",
            "::gate",
            "--input",
            input,
            "examples/nor.ncg2",
        ]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
}

#[test]
fn bad_inputs_exit_with_an_error_and_no_result() {
    for (name, value, diagnostic) in [
        ("::missing", "00", "宣言が見つからない"),
        ("gate", "00", "完全修飾名"),
        ("::gate", "0", "2 ビット必要"),
        ("::gate", "x", "2 ビット必要"),
        ("::gate", "0x", "0 と 1"),
    ] {
        let output = cli(&[
            "eval",
            "--func",
            name,
            "--input",
            value,
            "examples/nor.ncg2",
        ]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains(diagnostic)
        );
    }
}

#[test]
fn namespace_selection_and_diagnostics_preserve_file_boundaries() {
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let path = std::env::temp_dir().join(format!("ncg2-cli-test-{}", std::process::id()));
    fs::create_dir(&path).unwrap();
    let folder = Scratch(path);
    fs::write(folder.0.join("basis.ncg2"), "(using nor)").unwrap();
    fs::write(
        folder.0.join("logic.ncg2"),
        "(namespace lib (func wire (in (x (bits 3))) (out y) (def y (slice x 0 1))))",
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(BIN)
            .current_dir(&folder.0)
            .args(args)
            .output()
            .unwrap()
    };
    let output = run(&[
        "eval",
        "--func",
        "::lib::wire",
        "--input",
        "100",
        "basis.ncg2",
        "logic.ncg2",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "1");

    fs::write(folder.0.join("invalid.ncg2"), "(using nor").unwrap();
    let syntax = run(&["check", "invalid.ncg2"]);
    assert_eq!(syntax.status.code(), Some(1));
    let stderr = String::from_utf8(syntax.stderr).unwrap();
    assert!(stderr.contains("invalid.ncg2 ["));
    assert!(stderr.contains("UnclosedList"));

    fs::write(
        folder.0.join("invalid.ncg2"),
        "(using nor) (func broken (in x) (out y) (def y (unknown x)))",
    )
    .unwrap();
    let missing = run(&["check", "invalid.ncg2"]);
    assert_eq!(missing.status.code(), Some(1));
    let stderr = String::from_utf8(missing.stderr).unwrap();
    assert!(stderr.contains("invalid.ncg2 ["));
    assert!(stderr.contains("unknown が見つからない"));
}

#[test]
fn readme_latch_example_reports_reset_and_each_tick() {
    let trace = cli(&[
        "run",
        "--input",
        "11",
        "--input",
        "00",
        "examples/latch.ncg2",
    ]);
    assert!(
        trace.status.success(),
        "{}",
        String::from_utf8_lossy(&trace.stderr)
    );
    assert_eq!(
        String::from_utf8(trace.stdout).unwrap(),
        "0 00\n1 11\n2 10\n"
    );
    assert!(trace.stderr.is_empty());

    let reset = cli(&["run", "examples/latch.ncg2"]);
    assert!(
        reset.status.success(),
        "{}",
        String::from_utf8_lossy(&reset.stderr)
    );
    assert_eq!(String::from_utf8(reset.stdout).unwrap(), "0 00\n");
}

#[test]
fn run_rejects_a_bad_later_frame_before_reporting_a_trace() {
    for (frame, reason) in [("0", "2 ビット必要"), ("0x", "0 と 1")] {
        let invalid = cli(&[
            "run",
            "--input",
            "11",
            "--input",
            frame,
            "examples/latch.ncg2",
        ]);
        assert_eq!(invalid.status.code(), Some(1));
        assert!(invalid.stdout.is_empty());
        let stderr = String::from_utf8(invalid.stderr).unwrap();
        assert!(stderr.contains("tick 2"));
        assert!(stderr.contains(reason));
    }

    let invalid = cli(&["run", "--entry", "::gate", "examples/nor.ncg2"]);
    assert_eq!(invalid.status.code(), Some(1));
    assert!(invalid.stdout.is_empty());
    assert!(
        String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("module を指す")
    );
}

#[test]
fn run_accepts_empty_frames_for_an_entry_without_inputs() {
    let trace = cli(&[
        "run",
        "--entry",
        "::tests::oscillator",
        "--input=",
        "--input=",
        "tests/fixtures/clocked.ncg2",
    ]);
    assert!(
        trace.status.success(),
        "{}",
        String::from_utf8_lossy(&trace.stderr)
    );
    assert_eq!(String::from_utf8(trace.stdout).unwrap(), "0 0\n1 1\n2 0\n");
}

#[test]
fn run_reports_the_ram_call_that_exceeds_the_memory_limit() {
    let failure = cli(&[
        "run",
        "--entry",
        "::tests::capacity",
        "tests/fixtures/clocked.ncg2",
    ]);
    assert_eq!(failure.status.code(), Some(1));
    assert!(failure.stdout.is_empty());
    let stderr = String::from_utf8(failure.stderr).unwrap();
    assert!(stderr.contains("記憶領域が容量上限を超える"));
    let text = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/clocked.ncg2"),
    )
    .unwrap();
    let call = "(bank a d we)";
    let offset = text.find(call).unwrap();
    assert!(
        stderr.contains(&format!(
            "tests/fixtures/clocked.ncg2 [{offset}..{}]",
            offset + call.len()
        )),
        "{stderr}"
    );
}

#[test]
fn reports_the_slice_range_in_the_source_fragment() {
    let failure = cli(&[
        "run",
        "--entry",
        "::tests::bad-slice",
        "tests/fixtures/clocked.ncg2",
        "tests/fixtures/slice-part.ncg2",
    ]);
    assert_eq!(failure.status.code(), Some(1));
    assert!(failure.stdout.is_empty());
    let stderr = String::from_utf8(failure.stderr).unwrap();
    assert!(
        stderr.contains("tests/fixtures/slice-part.ncg2 ["),
        "{stderr}"
    );
    assert!(stderr.contains("添字 5..5 が入力幅 1 ビット"), "{stderr}");
    assert!(
        stderr.contains("入力の幅 tests/fixtures/clocked.ncg2 ["),
        "{stderr}"
    );
}
