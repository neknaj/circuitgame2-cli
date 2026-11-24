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
fn project_sources_are_relative_to_the_settings_file() {
    let project = "examples/project/circuitgame2.toml";
    let trace = cli(&["run", "--project", project, "--input", "0", "--input", "1"]);
    assert!(
        trace.status.success(),
        "{}",
        String::from_utf8_lossy(&trace.stderr)
    );
    assert_eq!(
        String::from_utf8(trace.stdout)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["0 0", "1 1", "2 0"]
    );

    let checked = cli(&["check", "--project", project]);
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert_eq!(
        String::from_utf8(checked.stdout).unwrap().trim(),
        "検査成功（2 ファイル）"
    );

    let evaluated = cli(&[
        "eval",
        "--func",
        "::demo::invert",
        "--input",
        "0",
        "--project",
        project,
    ]);
    assert!(
        evaluated.status.success(),
        "{}",
        String::from_utf8_lossy(&evaluated.stderr)
    );
    assert_eq!(String::from_utf8(evaluated.stdout).unwrap().trim(), "1");
}

#[test]
fn project_settings_errors_have_utf8_byte_ranges() {
    struct SettingsFile(PathBuf);

    impl Drop for SettingsFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    let path = std::env::temp_dir().join(format!("ncg2-cli-project-{}.toml", std::process::id()));
    let file = SettingsFile(path);
    let path = file.0.to_str().unwrap();
    let source = "# 日本語のコメント\nsources = [\"../escape.ncg2\"]\n";
    fs::write(&file.0, source).unwrap();
    let rejection = cli(&["run", "--project", path]);
    assert_eq!(rejection.status.code(), Some(1));
    assert!(rejection.stdout.is_empty());
    let start = source.find("\"../escape.ncg2\"").unwrap();
    let end = start + "\"../escape.ncg2\"".len();
    let error = String::from_utf8(rejection.stderr).unwrap();
    assert!(
        error.contains(&format!("{path}[{start}..{end}]")),
        "{error}"
    );

    fs::write(&file.0, b"sources = []\n# \xff").unwrap();
    let encoding = cli(&["run", "--project", path]);
    assert_eq!(encoding.status.code(), Some(1));
    assert!(encoding.stdout.is_empty());
    let error = String::from_utf8(encoding.stderr).unwrap();
    assert!(error.contains(&format!("{path}[15..16]")), "{error}");
    assert!(error.contains("不正な UTF-8"), "{error}");

    for (source, expected) in [
        ("sources = [\"main.ncg2\"]\nunknown = true\n", "unknown"),
        (
            "sources = [\"main.ncg2\"]\ntick-ns = {\n numerator = 1, denominator = 1\n}\n",
            "設定を解釈できない",
        ),
        (
            "sources = [\"main.ncg2\"]\n[[vram]]\nname = \"screen\"\nread-output = \"fb\"\n",
            "VRAM の公開に未対応",
        ),
        (
            "sources = [\"main.ncg2\"]\n[[device]]\nname = \"uart\"\nkind = \"uart-terminal\"\nbindings = {}\n",
            "外部デバイスの接続に未対応",
        ),
    ] {
        fs::write(&file.0, source).unwrap();
        let result = cli(&["run", "--project", path]);
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains(&format!("{path}[")), "{error}");
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn project_source_diagnostics_show_the_settings_location() {
    struct ProjectFixture(PathBuf);

    impl Drop for ProjectFixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.0.join("local.ncg2"));
            let _ = fs::remove_file(self.0.join("circuitgame2.toml"));
            let _ = fs::remove_dir(&self.0);
        }
    }

    let directory =
        std::env::temp_dir().join(format!("ncg2-cli-project-source-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let fixture = ProjectFixture(directory);
    let settings = fixture.0.join("circuitgame2.toml");
    fs::write(&settings, "sources = [\"local.ncg2\"]\n").unwrap();
    fs::write(fixture.0.join("local.ncg2"), "(using nor))").unwrap();
    let result = cli(&["check", "--project", settings.to_str().unwrap()]);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let error = String::from_utf8(result.stderr).unwrap();
    assert!(error.contains(settings.to_str().unwrap()), "{error}");
    assert!(error.contains("local.ncg2 [11..12]"), "{error}");
}

#[test]
fn project_argument_conflicts_take_precedence_over_file_errors() {
    let path = "missing-settings.toml";
    for extra in [&["--entry", "::main"][..], &["missing.ncg2"][..]] {
        let mut args = vec!["run", "--project", path];
        args.extend(extra);
        let result = cli(&args);
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains("併用できない"), "{error}");
    }
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
fn readme_ram_example_reports_the_previous_value_at_the_selected_address() {
    let trace = cli(&[
        "run",
        "--input",
        "1101",
        "--input",
        "1011",
        "--input",
        "1000",
        "--input",
        "0000",
        "examples/ram.ncg2",
    ]);
    assert!(
        trace.status.success(),
        "{}",
        String::from_utf8_lossy(&trace.stderr)
    );
    assert_eq!(
        String::from_utf8(trace.stdout).unwrap(),
        "0 00\n1 00\n2 10\n3 01\n4 00\n"
    );
    assert!(trace.stderr.is_empty());
}

#[test]
fn readme_hierarchy_ram_example_keeps_the_two_instances_independent() {
    let trace = cli(&[
        "run",
        "--input",
        "11100",
        "--input",
        "10111",
        "--input",
        "10000",
        "examples/hierarchy-ram.ncg2",
    ]);
    assert!(
        trace.status.success(),
        "{}",
        String::from_utf8_lossy(&trace.stderr)
    );
    assert_eq!(
        String::from_utf8(trace.stdout).unwrap(),
        "0 00\n1 00\n2 10\n3 01\n"
    );
    assert!(trace.stderr.is_empty());
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

fn fixture_location(path: &str, expression: &str, operand: &str) -> String {
    let absolute = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
    let text = fs::read_to_string(absolute).unwrap();
    let start = text.find(expression).unwrap() + expression.find(operand).unwrap();
    format!("{path} [{start}..{}]", start + operand.len())
}

#[test]
fn run_reports_every_reference_in_a_cycle_across_module_parts() {
    let owner = "tests/fixtures/clocked.ncg2";
    let part = "tests/fixtures/cycle-part.ncg2";
    let failure = cli(&["run", "--entry", "::tests::cycle", owner, part]);
    assert_eq!(failure.status.code(), Some(1));
    assert!(failure.stdout.is_empty());

    let stderr = String::from_utf8(failure.stderr).unwrap();
    assert!(stderr.contains("配線参照だけで循環している"), "{stderr}");
    assert!(
        stderr.contains(&fixture_location(owner, "(def x y)", "y")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&fixture_location(part, "(def y x)", "x")),
        "{stderr}"
    );
    assert!(stderr.contains("循環内の参照"), "{stderr}");
}

#[test]
fn run_reports_the_selected_operands_in_a_cycle_across_module_parts() {
    let owner = "tests/fixtures/clocked.ncg2";
    let part = "tests/fixtures/cycle-part.ncg2";
    let failure = cli(&["run", "--entry", "::tests::selection-cycle", owner, part]);
    assert_eq!(failure.status.code(), Some(1));
    assert!(failure.stdout.is_empty());

    let stderr = String::from_utf8(failure.stderr).unwrap();
    assert!(stderr.contains("配線参照だけで循環している"), "{stderr}");
    assert!(
        stderr.contains(&fixture_location(owner, "(bit right 0)", "right")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&fixture_location(part, "(slice left 0 1)", "left")),
        "{stderr}"
    );
    assert!(stderr.contains("循環内の参照"), "{stderr}");
}

#[test]
fn readme_concat_example_places_operands_from_low_bits() {
    let trace = cli(&["run", "--input", "010", "examples/concat.ncg2"]);
    assert!(
        trace.status.success(),
        "{}",
        String::from_utf8_lossy(&trace.stderr)
    );
    assert_eq!(String::from_utf8(trace.stdout).unwrap(), "0 0001\n1 0101\n");
    assert!(trace.stderr.is_empty());
}

#[test]
fn run_reports_operands_in_a_concat_and_selection_cycle() {
    let owner = "tests/fixtures/clocked.ncg2";
    let part = "tests/fixtures/cycle-part.ncg2";
    let failure = cli(&["run", "--entry", "::tests::concat-cycle", owner, part]);
    assert_eq!(failure.status.code(), Some(1));
    assert!(failure.stdout.is_empty());

    let stderr = String::from_utf8(failure.stderr).unwrap();
    assert!(stderr.contains("配線参照だけで循環している"), "{stderr}");
    assert!(
        stderr.contains(&fixture_location(owner, "(concat 0 y)", "y")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&fixture_location(part, "(bit x 1)", "x")),
        "{stderr}"
    );
    assert!(stderr.contains("循環内の参照"), "{stderr}");
}

#[test]
fn run_reports_concat_width_and_source_range() {
    let path = "tests/fixtures/clocked.ncg2";
    let failure = cli(&["run", "--entry", "::tests::concat-width", path]);
    assert_eq!(failure.status.code(), Some(1));
    assert!(failure.stdout.is_empty());
    let stderr = String::from_utf8(failure.stderr).unwrap();
    let expression = "(concat a b)";
    assert!(
        stderr.contains(&fixture_location(path, expression, expression)),
        "{stderr}"
    );
    assert!(
        stderr.contains("連接の幅 65537 ビットが上限 65536 ビットを超える"),
        "{stderr}"
    );
}
