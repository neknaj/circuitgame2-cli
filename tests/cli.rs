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
    assert!(stderr.contains("末尾添字 5 が入力幅 1 ビット"), "{stderr}");
    assert!(
        stderr.contains("入力の幅 tests/fixtures/clocked.ncg2 ["),
        "{stderr}"
    );
}

#[test]
fn readme_selection_example_routes_bit_and_slice_to_a_latch() {
    let trace = cli(&[
        "run",
        "--input",
        "0110",
        "--input",
        "1000",
        "examples/selected-latch.ncg2",
    ]);
    assert!(
        trace.status.success(),
        "{}",
        String::from_utf8_lossy(&trace.stderr)
    );
    assert_eq!(
        String::from_utf8(trace.stdout).unwrap(),
        "0 00\n1 01\n2 01\n"
    );
    assert!(trace.stderr.is_empty());
}

#[test]
fn run_describes_selection_range_and_port_width_in_call_arguments() {
    const PATH: &str = "tests/fixtures/selected-arguments.ncg2";
    let text = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(PATH)).unwrap();
    let location = |word: &str| {
        let start = text.find(word).unwrap();
        format!("{PATH} [{start}..{}]", start + word.len())
    };

    let outside = cli(&["run", "--entry", "::tests::outside", PATH]);
    assert_eq!(outside.status.code(), Some(1));
    assert!(outside.stdout.is_empty());
    let stderr = String::from_utf8(outside.stderr).unwrap();
    assert!(stderr.contains(&location("3 2")), "{stderr}");
    assert!(
        stderr.contains("末尾添字 4 が入力幅 4 ビットの範囲を外れる。開始添字は 3。"),
        "{stderr}"
    );
    let input_width = text.find("(bits 4)").unwrap() + "(bits ".len();
    assert!(
        stderr.contains(&format!(
            "入力の幅 {PATH} [{input_width}..{}]",
            input_width + 1
        )),
        "{stderr}"
    );

    let mismatch = cli(&["run", "--entry", "::tests::wrong-width", PATH]);
    assert_eq!(mismatch.status.code(), Some(1));
    assert!(mismatch.stdout.is_empty());
    let stderr = String::from_utf8(mismatch.stderr).unwrap();
    assert!(stderr.contains(&location("(bit data 1)")), "{stderr}");
    assert!(
        stderr.contains("幅は 1 ビットだが、入力の要求型は 2 ビット"),
        "{stderr}"
    );
    let type_width = text.find("(width 2)").unwrap() + "(width ".len();
    assert!(
        stderr.contains(&format!(
            "入力の型 {PATH} [{type_width}..{}]",
            type_width + 1
        )),
        "{stderr}"
    );
}

#[test]
fn run_reports_every_reference_in_a_cycle_across_module_parts() {
    let owner = "tests/fixtures/clocked.ncg2";
    let part = "tests/fixtures/cycle-part.ncg2";
    let failure = cli(&["run", "--entry", "::tests::cycle", owner, part]);
    assert_eq!(failure.status.code(), Some(1));
    assert!(failure.stdout.is_empty());

    let stderr = String::from_utf8(failure.stderr).unwrap();
    let location = |path: &str, definition: &str| {
        let absolute = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
        let text = fs::read_to_string(absolute).unwrap();
        let start = text.find(definition).unwrap() + definition.len() - 2;
        format!("{path} [{start}..{}]", start + 1)
    };
    assert!(stderr.contains("信号の直結だけで循環している"), "{stderr}");
    assert!(stderr.contains(&location(owner, "(def x y)")), "{stderr}");
    assert!(stderr.contains(&location(part, "(def y x)")), "{stderr}");
    assert!(stderr.contains("循環内の参照"), "{stderr}");
}
