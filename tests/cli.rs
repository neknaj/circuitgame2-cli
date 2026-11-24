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
