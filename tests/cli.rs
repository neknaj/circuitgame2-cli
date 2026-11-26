use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const BIN: &str = env!("CARGO_BIN_EXE_ncg2");

fn cli(args: &[&str]) -> Output {
    cli_in(Path::new(env!("CARGO_MANIFEST_DIR")), args)
}

fn cli_in(directory: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .current_dir(directory)
        .args(args)
        .output()
        .expect("CLI を起動できる")
}

#[test]
fn project_can_override_the_simulation_state_capacity() {
    let folder = tempfile::tempdir().unwrap();
    fs::write(
        folder.path().join("main.ncg2"),
        "(using nor)\n(memory bank (address 15) (data (bits 64)))\n\
         (module main (in (address (bits 15))) (out (value (bits 64)))\n\
           (def value (bank address #x0000000000000000 0)))\n",
    )
    .unwrap();
    let settings = folder.path().join("circuitgame2.toml");
    let args = ["run", "--project", "circuitgame2.toml"];
    fs::write(&settings, "sources = [\"main.ncg2\"]\n").unwrap();
    let default = cli_in(folder.path(), &args);
    assert!(default.status.success(), "{default:?}");

    fs::write(&settings, "sources = [\"main.ncg2\"]\nstate-bits = 128\n").unwrap();
    let limited = cli_in(folder.path(), &args);
    assert!(!limited.status.success());
    assert!(limited.stdout.is_empty());
    let stderr = String::from_utf8(limited.stderr).unwrap();
    assert!(stderr.contains("記憶領域が容量上限を超える"), "{stderr}");
}

#[test]
fn check_reads_ncg2m_directly_and_through_project_settings() {
    let example = "examples/helpers.ncg2m";
    let result = cli(&["check", example]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stderr.is_empty());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "NCG2M の静的検査成功（1 ファイル）\n"
    );

    let folder = tempfile::tempdir().unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(example),
        folder.path().join("helpers.ncg2m"),
    )
    .unwrap();
    let settings = folder.path().join("circuitgame2.toml");
    fs::write(&settings, "sources = [\"helpers.ncg2m\"]\n").unwrap();
    let path = settings.to_str().unwrap();
    let result = cli(&["check", "--format", "json", "--project", path]);
    assert!(result.status.success(), "{result:?}");
    assert!(result.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"version": 1, "ok": true, "project": path, "files": 1, "diagnostics": []})
    );
    let result = cli(&["run", "--project", path]);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("NCG2M の実行に未対応")
    );
}

#[test]
fn eval_meta_selects_a_checked_helper_and_respects_the_fuel_budget() {
    let folder = tempfile::tempdir().unwrap();
    let dir = folder.path();
    let settings = dir.join("circuitgame2.toml");
    fs::write(
        &settings,
        "sources = [\"main.ncg2m\", \"helper.ncg2m\"]\n[[device]]\nname = \"uart\"\nkind = \"uart-terminal\"\nbindings = {}\n",
    )
    .unwrap();
    fs::write(
        dir.join("main.ncg2m"),
        "(use ::tools::choose choose) (defn entry (as-type (Fn (Bool Int Int) Int) (fn (flag yes no) (choose flag yes no))))",
    )
    .unwrap();
    fs::write(
        dir.join("helper.ncg2m"),
        "(namespace tools (export choose)
            (defn choose (as-type (Fn (Bool Int Int) Int) (fn (flag yes no) (if flag yes no))))
            (defn private (as-type (Fn () Bool) (fn () true))))",
    )
    .unwrap();
    let invoke = |name: &str, options: &[&str]| {
        let mut command = vec!["eval-meta", "--defn", name];
        command.extend_from_slice(options);
        command.extend(["--project", settings.to_str().unwrap()]);
        cli(&command)
    };
    let arguments = [
        "--arg",
        "true",
        "--arg",
        "Int:-9223372036854775808",
        "--arg",
        "Int:42",
    ];
    let output = invoke("::entry", &arguments);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"-9223372036854775808\n");
    assert!(output.stderr.is_empty());

    let mut limited = arguments.to_vec();
    limited.extend(["--fuel", "1"]);
    let output = invoke("::entry", &limited);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let output = invoke(
        "::entry",
        &["--arg", "true", "--arg", "Nat:0", "--arg", "Int:42"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("main.ncg2m") && error.contains("2 番目の引数は Int"));

    let output = invoke("::tools::private", &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("helper.ncg2m") && error.contains("非公開"));
}


#[test]
fn eval_meta_runs_fold_body_with_local_bindings_and_resumes_across_slices() {
    let evaluate = |count, fuel| {
        cli(&[
            "eval-meta", "--defn", "::last-index", "--arg", count, "--arg", "Nat:7",
            "--fuel", fuel, "--steps", "examples/helpers.ncg2m",
        ])
    };

    let empty = evaluate("Nat:0", "3");
    assert!(empty.status.success(), "{empty:?}");
    assert_eq!(empty.stdout, b"7\n");
    assert_eq!(empty.stderr, b"steps=3\n");

    let suspended = evaluate("Nat:2048", "4098");
    assert_eq!(suspended.status.code(), Some(2), "{suspended:?}");
    assert!(suspended.stdout.is_empty());
    assert!(String::from_utf8_lossy(&suspended.stderr).ends_with("steps=4098\n"));

    let completed = evaluate("Nat:2048", "4099");
    assert!(completed.status.success(), "{completed:?}");
    assert_eq!(completed.stdout, b"2047\n");
    assert_eq!(completed.stderr, b"steps=4099\n");
}
#[test]
fn eval_meta_runs_nested_fold_arithmetic_with_scoped_bindings() {
    let filename = "examples/helpers.ncg2m";
    let evaluate = |count: &str, initial: &str, fuel: &str| {
        cli(&[
            "eval-meta", "--defn", "::fold-sum", "--arg", count, "--arg", initial,
            "--fuel", fuel, "--steps", filename,
        ])
    };

    let short = evaluate("Nat:4", "Nat:7", "1000");
    assert!(short.status.success(), "{short:?}");
    assert_eq!(short.stdout, b"27\n");

    let many = evaluate("Nat:2048", "Nat:7", "100000");
    assert!(many.status.success(), "{many:?}");
    assert_eq!(many.stdout, b"4196359\n");
    let report = String::from_utf8(many.stderr).unwrap();
    let steps: usize = report.trim().strip_prefix("steps=").unwrap().parse().unwrap();
    assert!(steps > 4096);
    let stopped = evaluate("Nat:2048", "Nat:7", &(steps - 1).to_string());
    assert_eq!(stopped.status.code(), Some(2), "{stopped:?}");
    assert!(stopped.stdout.is_empty());
    let ready = evaluate("Nat:2048", "Nat:7", &steps.to_string());
    assert!(ready.status.success(), "{ready:?}");
    assert_eq!(ready.stdout, b"4196359\n");

    let overflow = evaluate("Nat:1", "Nat:18446744073709551615", "1000");
    assert_eq!(overflow.status.code(), Some(1), "{overflow:?}");
    assert!(overflow.stdout.is_empty());
    let expression = "(add acc (mul (sub (add index 2) 1) 2))";
    let source = include_str!("../examples/helpers.ncg2m");
    let start = source.find(expression).unwrap();
    let message = String::from_utf8(overflow.stderr).unwrap();
    assert!(message.contains(&format!("{filename} [{start}..{}]", start + expression.len())));
    assert!(message.contains("`add` の計算結果は Nat の範囲外"));
}

#[test]
fn eval_meta_reports_integer_arithmetic_at_the_original_helper() {
    let folder = tempfile::tempdir().unwrap();
    let dir = folder.path();
    let settings = dir.join("circuitgame2.toml");
    fs::write(&settings, "sources = [\"main.ncg2m\", \"helper.ncg2m\"]\n").unwrap();
    fs::write(
        dir.join("main.ncg2m"),
        "(use ::math::step transform) (defn entry (as-type (Fn (Nat) Nat) (fn (x) (transform x))))",
    )
    .unwrap();
    let helper_path = dir.join("helper.ncg2m");
    let helper = "(namespace math (export step) (defn step (as-type (Fn (Nat) Nat) (fn (x) (mul (sub x 1) 2)))))";
    fs::write(&helper_path, helper).unwrap();
    let project = settings.to_str().unwrap();
    let evaluate = |value| {
        cli(&[
            "eval-meta",
            "--defn",
            "::entry",
            "--arg",
            value,
            "--fuel",
            "100",
            "--project",
            project,
        ])
    };
    let success = evaluate("Nat:9");
    assert!(success.status.success(), "{success:?}");
    assert_eq!(success.stdout, b"16\n");
    assert!(success.stderr.is_empty());

    let expression = "(sub x 1)";
    let start = helper.find(expression).unwrap();
    let failed = evaluate("Nat:0");
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    assert!(failed.stdout.is_empty());
    let message = String::from_utf8(failed.stderr).unwrap();
    assert!(message.contains(&format!(
        "helper.ncg2m [{start}..{}]",
        start + expression.len()
    )));
    assert!(message.contains("`sub` の計算結果は Nat の範囲外"));

    let incomplete = "(sub x)";
    fs::write(&helper_path, helper.replace(expression, incomplete)).unwrap();
    let failed = cli(&["check", "--format", "json", "--project", project]);
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    let report: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    let diagnostic = &report["diagnostics"][0];
    assert_eq!(diagnostic["code"], "argument_count");
    assert_eq!(
        diagnostic["location"],
        serde_json::json!({
            "path": "helper.ncg2m",
            "range": {"start": start, "end": start + incomplete.len()},
        })
    );
}

#[test]
fn eval_meta_steps_accumulates_across_slices_and_preserves_result_channels() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("steps.ncg2m");
    let mut source = String::from("(defn level0 (as-type (Fn () Nat) (fn () 1)))\n");
    for level in 1..=12 {
        let previous = level - 1;
        source.push_str(&format!(
            "(defn level{level} (as-type (Fn () Nat) (fn () (add (level{previous}) (level{previous})))))\n"
        ));
    }
    fs::write(&file, source).unwrap();
    let file = "steps.ncg2m";
    let cli = |args: &[&str]| cli_in(dir.path(), args);

    let limited = cli(&[
        "eval-meta",
        "--defn",
        "::level12",
        "--steps",
        "--fuel",
        "4097",
        file,
    ]);
    assert_eq!(limited.status.code(), Some(2), "{limited:?}");
    assert!(limited.stdout.is_empty());
    let diagnostic = String::from_utf8(limited.stderr).unwrap();
    assert!(diagnostic.contains("評価が完了しなかった"));
    assert_eq!(diagnostic.lines().last(), Some("steps=4097"));

    let complete = cli(&["eval-meta", "--defn", "::level12", "--steps", file]);
    assert!(complete.status.success(), "{complete:?}");
    assert_eq!(complete.stdout, b"4096\n");
    let stderr = String::from_utf8(complete.stderr).unwrap();
    let exact = stderr.trim_end().strip_prefix("steps=").unwrap();
    assert!(exact.parse::<u64>().unwrap() > 4097);
    let just_enough = cli(&[
        "eval-meta",
        "--defn",
        "::level12",
        "--steps",
        "--fuel",
        exact,
        file,
    ]);
    assert!(just_enough.status.success(), "{just_enough:?}");
    assert_eq!(just_enough.stdout, complete.stdout);
    assert_eq!(just_enough.stderr, stderr.as_bytes());
}

#[test]
fn eval_meta_steps_reports_evaluation_errors_and_skips_failed_validation() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("steps.ncg2m");
    fs::write(
        &file,
        "(defn fail (as-type (Fn (Nat) Nat) (fn (x) (sub x 1))))",
    )
    .unwrap();
    let path = "steps.ncg2m";
    let cli = |args: &[&str]| cli_in(dir.path(), args);
    let evaluate = |arg| {
        cli(&[
            "eval-meta",
            "--defn",
            "::fail",
            "--arg",
            arg,
            "--steps",
            path,
        ])
    };

    let failed = evaluate("Nat:0");
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    assert!(failed.stdout.is_empty());
    let diagnostic = String::from_utf8(failed.stderr).unwrap();
    let (steps, message) = diagnostic.split_once('\n').unwrap();
    let completed = steps
        .strip_prefix("steps=")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(completed > 1);
    assert!(message.contains("`sub` の計算結果は Nat の範囲外"));

    let prior = (completed - 1).to_string();
    let limited = cli(&[
        "eval-meta",
        "--defn",
        "::fail",
        "--arg",
        "Nat:0",
        "--steps",
        "--fuel",
        &prior,
        path,
    ]);
    assert_eq!(limited.status.code(), Some(2), "{limited:?}");
    assert!(limited.stdout.is_empty());
    let diagnostic = String::from_utf8(limited.stderr).unwrap();
    assert_eq!(
        diagnostic.lines().last(),
        Some(format!("steps={prior}").as_str())
    );

    let invalid = evaluate("Int:0");
    assert_eq!(invalid.status.code(), Some(1), "{invalid:?}");
    assert!(invalid.stdout.is_empty());
    let diagnostic = String::from_utf8(invalid.stderr).unwrap();
    assert!(diagnostic.contains("引数は Nat"));
    assert!(!diagnostic.contains("steps="));

    fs::write(&file, "(").unwrap();
    let invalid = evaluate("Nat:0");
    assert_eq!(invalid.status.code(), Some(1), "{invalid:?}");
    assert!(invalid.stdout.is_empty());
    assert!(
        !String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("steps=")
    );
}

#[test]
fn check_resolves_a_cross_language_use_before_reporting_the_unsupported_circuit_check() {
    let folder = tempfile::tempdir().unwrap();
    let dir = folder.path();
    let settings = dir.join("circuitgame2.toml");
    fs::write(&settings, "sources = [\"main.ncg2\", \"tools.ncg2m\"]\n").unwrap();
    let circuit =
        "; 🐈\n(using nor) (use ::tools::ready ready) (func gate (in a) (out y) (def y (nor a a)))";
    fs::write(dir.join("main.ncg2"), circuit).unwrap();
    let implementation = "; 🐈\n(namespace tools (defn ready (as-type (Fn () Bool) (fn () true)))";
    let meta_path = dir.join("tools.ncg2m");
    fs::write(&meta_path, format!("{implementation} (export ready))")).unwrap();
    let args = [
        "check",
        "--format",
        "json",
        "--project",
        settings.to_str().unwrap(),
    ];
    let result = cli(&args);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(json["files"], 2);
    assert_eq!(
        json["diagnostics"][0]["code"],
        "unsupported_mixed_circuit_check"
    );
    let gate = circuit.find("gate").unwrap();
    assert_eq!(
        json["diagnostics"][0]["location"],
        serde_json::json!({"path":"main.ncg2","range":{"start":gate,"end":gate + "gate".len()}})
    );

    fs::write(meta_path, format!("{implementation})")).unwrap();
    let result = cli(&args);
    assert_eq!(result.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let diagnostic = &json["diagnostics"][0];
    assert_eq!(diagnostic["code"], "private_use_target");
    let absolute = circuit.find("::tools::ready").unwrap();
    assert_eq!(
        diagnostic["location"],
        serde_json::json!({"path":"main.ncg2","range":{"start":absolute,"end":absolute + "::tools::ready".len()}})
    );
    let private = implementation.find("ready").unwrap();
    assert_eq!(
        diagnostic["related"][0],
        serde_json::json!({"path":"tools.ncg2m","range":{"start":private,"end":private + "ready".len()}})
    );
}

#[test]
fn mixed_check_reports_call_errors_with_original_cross_file_ranges() {
    let folder = tempfile::tempdir().unwrap();
    let dir = folder.path();
    let settings = dir.join("circuitgame2.toml");
    fs::write(
        &settings,
        "sources = [\"gates.ncg2\", \"helpers.ncg2m\", \"main.ncg2\"]\n",
    )
    .unwrap();
    let gates = "; 日本語\n(namespace gates (export invert) (func invert (in a) (out y) (def y (nor a a))))";
    let helpers =
        "; 🐈\n(namespace tools (export ready) (defn ready (as-type (Fn () Bool) (fn () true))))";
    fs::write(dir.join("gates.ncg2"), gates).unwrap();
    fs::write(dir.join("helpers.ncg2m"), helpers).unwrap();
    let check_error = |call: &str,
                       code,
                       primary: &str,
                       related_path,
                       related_source: &str,
                       related: &str| {
        let source = format!("; 🐈\n(using nor) (func gate (in a) (out y) (def y {call}))");
        fs::write(dir.join("main.ncg2"), &source).unwrap();
        let result = cli(&[
            "check",
            "--format",
            "json",
            "--project",
            settings.to_str().unwrap(),
        ]);
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        assert!(result.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        let diagnostic = &report["diagnostics"][0];
        let start = source.rfind(primary).unwrap();
        let other = related_source.rfind(related).unwrap();
        assert_eq!(diagnostic["code"], code);
        assert_eq!(
            diagnostic["location"],
            serde_json::json!({"path": "main.ncg2", "range": {"start": start, "end": start + primary.len()}})
        );
        assert_eq!(
            diagnostic["related"],
            serde_json::json!([{"path": related_path, "range": {"start": other, "end": other + related.len()}}])
        );
    };
    check_error(
        "(::tools::ready a)",
        "wrong_declaration_kind",
        "::tools::ready",
        "helpers.ncg2m",
        helpers,
        "ready",
    );
    check_error(
        "(::gates::invert a a)",
        "argument_count",
        "(::gates::invert a a)",
        "gates.ncg2",
        gates,
        "invert",
    );
}

#[test]
fn mixed_check_validates_expanded_func_and_reports_original_source_ranges() {
    let folder = tempfile::tempdir().unwrap();
    let dir = folder.path();
    let settings = dir.join("circuitgame2.toml");
    let circuit = dir.join("main.ncg2");
    let macros = dir.join("macros.ncg2m");
    fs::write(&settings, "sources = [\"main.ncg2\", \"macros.ncg2m\"]\n").unwrap();
    let template = "; 定義 🐈\n(defmacro invert (out source)
        (list (syntax (def tmp (unsyntax source)))
              (syntax (def (unsyntax out) (nor tmp 0)))))";
    fs::write(&macros, template).unwrap();
    let command = ["check", "--project", settings.to_str().unwrap()];
    let check_json = || {
        let result = cli(&[
            "check",
            "--format",
            "json",
            "--project",
            settings.to_str().unwrap(),
        ]);
        assert!(result.stderr.is_empty(), "{result:?}");
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        (result.status.code(), report)
    };

    let passing = "; 回路 🐈\n(using nor)
        (func twice (in a) (out y)
          (def input a)
          (expand invert once input)
          (expand invert y once))";
    fs::write(&circuit, format!("{passing} (func plain (in x) (out x))")).unwrap();
    let (status, report) = check_json();
    assert_eq!(status, Some(0), "{report}");
    assert_eq!(report["files"], 2);
    assert_eq!(report["diagnostics"], serde_json::json!([]));

    let call = "(expand invert y wide)";
    let failing = format!(
        "; 回路 🐈\n(using nor) (func fail (in a (wide (bits 2))) (out y)
        (def input a) (expand invert once input) {call})"
    );
    fs::write(&circuit, &failing).unwrap();
    let (status, report) = check_json();
    assert_eq!(status, Some(1), "{report}");
    let error = &report["diagnostics"][0];
    let called_at = failing.find(call).unwrap();
    let cause_at = template.find("nor tmp").unwrap() + "nor ".len();
    assert_eq!(error["code"], "call_argument");
    assert_eq!(
        error["location"],
        serde_json::json!({"path":"main.ncg2","range":{
            "start": called_at, "end": called_at + call.len(),
        }})
    );
    assert_eq!(
        error["related"],
        serde_json::json!([{"path": "macros.ncg2m","range":{
            "start": cause_at, "end": cause_at + "tmp".len(),
        }}])
    );
    let result = cli(&command);
    assert_eq!(result.status.code(), Some(1));
    let error = String::from_utf8(result.stderr).unwrap();
    assert!(error.contains("main.ncg2"), "{error}");
    assert!(error.contains("macros.ncg2m"), "{error}");
    assert!(error.contains("2 ビット"), "{error}");

    fs::write(&circuit, passing).unwrap();
    fs::write(
        &macros,
        format!("{template} (defmacro unchecked () (quote value))"),
    )
    .unwrap();
    let (status, report) = check_json();
    assert_eq!(status, Some(1), "{report}");
    assert_eq!(report["diagnostics"][0]["code"], "unsupported_macro_body");

    fs::write(&macros, template).unwrap();
    fs::write(
        &circuit,
        format!(
            "{passing} (func other (in x) (out y) (def y (plain x))) (func plain (in x) (out x))"
        ),
    )
    .unwrap();
    let (status, report) = check_json();
    assert_eq!(status, Some(1), "{report}");
    assert_eq!(
        report["diagnostics"][0]["code"],
        "unsupported_mixed_circuit_check"
    );
}

#[test]
fn mixed_eval_uses_the_checked_expansion_and_the_existing_port_order() {
    let folder = tempfile::tempdir().unwrap();
    let dir = folder.path();
    let settings = dir.join("circuitgame2.toml");
    let circuit = dir.join("main.ncg2");
    let macros = dir.join("macros.ncg2m");
    fs::write(&settings, "sources = [\"main.ncg2\", \"macros.ncg2m\"]\n").unwrap();
    let template = "; 定義 🐈\n(defmacro invert (out source)
        (list (syntax (def tmp (unsyntax source)))
              (syntax (def (unsyntax out) (nor tmp 0))))) (export invert)";
    fs::write(&macros, template).unwrap();
    let evaluate = |name, input| {
        cli(&[
            "eval",
            "--func",
            name,
            "--input",
            input,
            "--project",
            settings.to_str().unwrap(),
        ])
    };
    let source = "; 回路 🐈\n(using nor)
        (namespace logic
          (func direct (in x) (out x))
          (func twice (in (word (bits 3)) a) (out (relay (bits 3)) y inv)
            (def relay word)
            (def input a)
            (expand ::invert inv input)
            (expand ::invert y inv)))";
    fs::write(&circuit, source).unwrap();

    for (input, expected) in [("1011", "10110\n"), ("0110", "01101\n")] {
        let result = evaluate("::logic::twice", input);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(result.stdout, expected.as_bytes());
        assert!(result.stderr.is_empty(), "{result:?}");
    }
    let result = evaluate("::logic::direct", "0");
    assert!(result.status.success(), "{result:?}");
    assert_eq!(result.stdout, b"0\n");

    let result = evaluate("::logic::twice", "000");
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty(), "{result:?}");
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("4 ビット必要")
    );

    let invalid_call = "(expand ::invert y word)";
    let failing = source.replace("(expand ::invert y inv)", invalid_call);
    fs::write(&circuit, &failing).unwrap();
    let result = evaluate("::logic::twice", "1011");
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(result.stdout.is_empty(), "{result:?}");
    let message = String::from_utf8(result.stderr).unwrap();
    let called_at = failing.find(invalid_call).unwrap();
    let cause_at = template.find("nor tmp").unwrap() + "nor ".len();
    assert!(
        message.contains(&format!(
            "main.ncg2 [{called_at}..{}]",
            called_at + invalid_call.len()
        )),
        "{message}"
    );
    assert!(
        message.contains(&format!(
            "macros.ncg2m [{cause_at}..{}]",
            cause_at + "tmp".len()
        )),
        "{message}"
    );

    fs::write(&circuit, source).unwrap();
    fs::write(
        &macros,
        format!("{template} (defmacro unchecked () (quote value))"),
    )
    .unwrap();
    let result = evaluate("::logic::twice", "1011");
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(result.stdout.is_empty(), "{result:?}");
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("マクロ本体")
    );

    fs::write(&macros, template).unwrap();
    let result = evaluate("::invert", "0");
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(result.stdout.is_empty(), "{result:?}");
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("func を指す必要")
    );
}

#[test]
fn mixed_check_reports_wiring_and_output_count_with_related_ranges() {
    let folder = tempfile::tempdir().unwrap();
    let dir = folder.path();
    let settings = dir.join("circuitgame2.toml");
    let main = dir.join("main.ncg2");
    let library = "(namespace lib (export pair) (func pair (in a b) (out a b)))";
    fs::write(
        &settings,
        "sources = [\"main.ncg2\", \"library.ncg2\", \"helper.ncg2m\"]\n",
    )
    .unwrap();
    fs::write(dir.join("library.ncg2"), library).unwrap();
    fs::write(dir.join("helper.ncg2m"), "").unwrap();
    let check = |source: &str| {
        fs::write(&main, source).unwrap();
        let result = cli(&[
            "check",
            "--format",
            "json",
            "--project",
            settings.to_str().unwrap(),
        ]);
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        assert!(result.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        report["diagnostics"][0].clone()
    };
    let location = |path: &str, start, len| serde_json::json!({"path": path, "range": {"start": start, "end": start + len}});

    let forward = "(using nor) (func invalid (in a) (out y) (def y later) (def later a))";
    let error = check(forward);
    assert_eq!(error["code"], "forward_reference");
    assert_eq!(
        error["location"],
        location("main.ncg2", forward.find("later").unwrap(), 5)
    );
    assert_eq!(
        error["related"],
        serde_json::json!([location("main.ncg2", forward.rfind("later").unwrap(), 5)])
    );

    let expansion = "(expand make)";
    let waiting = format!("(using nor) (func waiting (in) (out z) {expansion})");
    let error = check(&waiting);
    assert_eq!(error["code"], "missing_reference");
    assert_eq!(
        error["location"],
        location(
            "main.ncg2",
            waiting.find(expansion).unwrap(),
            expansion.len()
        )
    );

    let too_many = format!("{waiting} (func invalid (in a b) (out y) (def y (::lib::pair a b)))");
    let call = "(::lib::pair a b)";
    let error = check(&too_many);
    assert_eq!(error["code"], "output_count");
    assert_eq!(
        error["location"],
        location("main.ncg2", too_many.find(call).unwrap(), call.len())
    );
    assert_eq!(
        error["related"],
        serde_json::json!([
            location("main.ncg2", too_many.rfind("y (").unwrap(), 1),
            location("library.ncg2", library.rfind("pair").unwrap(), 4)
        ])
    );
}

#[test]
fn mixed_check_reports_nested_input_and_output_types_with_original_ranges() {
    let folder = tempfile::tempdir().unwrap();
    let dir = folder.path();
    let settings = dir.join("circuitgame2.toml");
    let main = dir.join("main.ncg2");
    fs::write(
        &settings,
        "sources = [\"main.ncg2\", \"library.ncg2\", \"helper.ncg2m\"]\n",
    )
    .unwrap();
    let library = "; 型の宣言\n(namespace lib (export byte nibble)
        (func byte (in (x (bits 8))) (out (x (bits 8))))
        (func nibble (in (x (bits 4))) (out (x (bits 4)))))";
    fs::write(dir.join("library.ncg2"), library).unwrap();
    fs::write(dir.join("helper.ncg2m"), "; 補助関数\n").unwrap();
    let check = |source: &str| {
        fs::write(&main, source).unwrap();
        let result = cli(&[
            "check",
            "--format",
            "json",
            "--project",
            settings.to_str().unwrap(),
        ]);
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        assert!(result.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["files"], 3);
        assert_eq!(report["diagnostics"].as_array().unwrap().len(), 1);
        report["diagnostics"][0].clone()
    };
    let location = |path: &str, start, len| {
        serde_json::json!({
            "path": path, "range": {"start": start, "end": start + len}
        })
    };
    let eight = library.find("(bits 8)").unwrap() + "(bits ".len();
    let four = library.rfind("(bits 4)").unwrap() + "(bits ".len();
    let inner = "(::lib::nibble a)";

    let input = format!(
        "; 入力 🐈\n(using nor) (func gate (in (a (bits 4))) (out (y (bits 8))) (def y (::lib::byte {inner})))"
    );
    let error = check(&input);
    assert_eq!(error["code"], "call_argument");
    assert_eq!(
        error["location"],
        location("main.ncg2", input.find(inner).unwrap(), inner.len())
    );
    assert_eq!(
        error["related"],
        serde_json::json!([
            location("library.ncg2", eight, 1),
            location("library.ncg2", four, 1)
        ])
    );

    let output = format!(
        "; 出力 🐈\n(using nor) (func gate (in (a (bits 4))) (out (y (bits 8))) (def y {inner}))"
    );
    let error = check(&output);
    assert_eq!(error["code"], "output_type_mismatch");
    assert_eq!(
        error["location"],
        location("main.ncg2", output.rfind("y (").unwrap(), 1)
    );
    assert_eq!(
        error["related"],
        serde_json::json!([
            location(
                "main.ncg2",
                output.find("(bits 8)").unwrap() + "(bits ".len(),
                1
            ),
            location("library.ncg2", four, 1),
            location("main.ncg2", output.find(inner).unwrap(), inner.len()),
        ])
    );

    let expression = "(slice a 0 8)";
    let pending = format!(
        "(using nor) (func gate (in (a (bits 8))) (out (y (bits 8))) (def y (::lib::byte {expression})))"
    );
    let error = check(&pending);
    assert_eq!(error["code"], "unsupported_mixed_circuit_check");
    assert_eq!(
        error["location"],
        location(
            "main.ncg2",
            pending.find(expression).unwrap(),
            expression.len()
        )
    );

    let matching = "(using nor) (func gate (in (a (bits 8))) (out (y (bits 8))) (def y (::lib::byte (::lib::byte a))))";
    let error = check(matching);
    assert_eq!(error["code"], "unsupported_mixed_circuit_check");
    assert_eq!(
        error["location"],
        location("main.ncg2", matching.find("gate").unwrap(), "gate".len())
    );
}

#[test]
fn mixed_check_reports_selection_diagnostics_and_width_origins() {
    let folder = tempfile::tempdir().unwrap();
    let dir = folder.path();
    let settings = dir.join("circuitgame2.toml");
    let main = dir.join("main.ncg2");
    let library = "; ライブラリ\n(namespace lib (struct Word (width 4)) (export Word echo)
        (func echo (in (w Word)) (out (w Word))))";
    fs::write(
        &settings,
        "sources = [\"main.ncg2\", \"library.ncg2\", \"helper.ncg2m\"]\n",
    )
    .unwrap();
    fs::write(dir.join("library.ncg2"), library).unwrap();
    fs::write(dir.join("helper.ncg2m"), "").unwrap();
    let check = |source: &str| {
        fs::write(&main, source).unwrap();
        let result = cli(&[
            "check",
            "--format",
            "json",
            "--project",
            settings.to_str().unwrap(),
        ]);
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        assert!(result.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["diagnostics"].as_array().unwrap().len(), 1);
        report["diagnostics"][0].clone()
    };
    let location = |path: &str, start, len| {
        serde_json::json!({
            "path": path, "range": {"start": start, "end": start + len}
        })
    };

    let outside =
        "; 🐈\n(using nor) (func bad (in (a (bits 8))) (out y) (def y (bit (slice a 0 4 2) 4)))";
    let error = check(outside);
    let inner = "(slice a 0 4 2)";
    assert_eq!(error["code"], "selection_argument");
    assert_eq!(
        error["location"],
        location("main.ncg2", outside.rfind("4)").unwrap(), 1)
    );
    assert_eq!(
        error["related"],
        serde_json::json!([location(
            "main.ncg2",
            outside.find(inner).unwrap(),
            inner.len()
        )])
    );
    assert!(error["message"].as_str().unwrap().contains("末尾添字 4"));

    let struct_input = "; 🐈\n(using nor) (func bad (in (a ::lib::Word)) (out y)
        (def same (::lib::echo a)) (def y (bit same 0)))";
    let error = check(struct_input);
    assert_eq!(error["code"], "selection_input_type");
    assert_eq!(
        error["location"],
        location(
            "main.ncg2",
            struct_input.rfind("same").unwrap(),
            "same".len()
        )
    );
    assert_eq!(
        error["related"],
        serde_json::json!([location(
            "library.ncg2",
            library.rfind("Word").unwrap(),
            "Word".len()
        )])
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("ビット列が必要")
    );
}

#[test]
fn mixed_check_reports_basis_errors_before_resolving_names() {
    let folder = tempfile::tempdir().unwrap();
    let dir = folder.path();
    let settings = dir.join("circuitgame2.toml");
    fs::write(
        &settings,
        "sources = [\"tools.ncg2m\", \"main.ncg2\", \"other.ncg2\"]\n",
    )
    .unwrap();
    fs::write(dir.join("tools.ncg2m"), "").unwrap();
    let circuit = "(use ::missing::item absent) (func gate (in a) (out a))";
    let main = dir.join("main.ncg2");
    let other = dir.join("other.ncg2");
    fs::write(&main, circuit).unwrap();
    fs::write(&other, "").unwrap();

    let args = [
        "check",
        "--format",
        "json",
        "--project",
        settings.to_str().unwrap(),
    ];
    let check = || {
        let result = cli(&args);
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        assert!(result.stderr.is_empty());
        serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap()
    };
    let json = check();
    assert_eq!(json["version"], 1);
    assert_eq!(json["files"], 3);
    assert_eq!(json["diagnostics"][0]["code"], "missing_basis");
    assert!(json["diagnostics"][0]["location"].is_null());

    let first = format!("; 猫\n(using nor) {circuit}");
    let later = "; 基底\n(using nor)";
    fs::write(&main, &first).unwrap();
    fs::write(&other, later).unwrap();
    let json = check();
    let diagnostic = &json["diagnostics"][0];
    assert_eq!(diagnostic["code"], "duplicate_basis");
    let related_offset = first.find("nor").unwrap();
    let primary_offset = later.find("nor").unwrap();
    assert_eq!(
        diagnostic["location"],
        serde_json::json!({"path":"other.ncg2","range":{"start":primary_offset,"end":primary_offset+3}})
    );
    assert_eq!(
        diagnostic["related"],
        serde_json::json!([{"path":"main.ncg2","range":{"start":related_offset,"end":related_offset+3}}])
    );
}

#[test]
fn check_ncg2m_reports_original_ranges_for_unchecked_and_invalid_expressions() {
    let folder = tempfile::tempdir().unwrap();
    let dir = folder.path();
    let settings = dir.join("circuitgame2.toml");
    fs::write(&settings, "sources = [\"helpers.ncg2m\"]\n").unwrap();
    let source = dir.join("helpers.ncg2m");
    let args = [
        "check",
        "--format",
        "json",
        "--project",
        settings.to_str().unwrap(),
    ];
    let examples = [
        (
            "; 🐈\n(defmacro saved () (quote value))",
            ("(quote value)", "(quote value)"),
            "unsupported_macro_body",
            None,
        ),
        (
            "; 🐈\n(defn answer (as-type (Fn () Nat) (fn () -1)))",
            ("-1", "-1"),
            "ncg2m_expression",
            None,
        ),
        (
            "; 🐈\n(defn unused (as-type (Fn () Bool) (fn () (let ((key true) (key false)) key))))",
            ("key false", "key"),
            "ncg2m_expression",
            Some(("key true", "key")),
        ),
        (
            "; 🐈\n(defn before (as-type (Fn () Bool) (fn () (after)))) (defn after (as-type (Fn () Bool) (fn () (before))))",
            ("(after)", "(after)"),
            "defn_cycle",
            Some(("(before)", "(before)")),
        ),
    ];
    for (text, primary, code, related) in examples {
        fs::write(&source, text).unwrap();
        let result = cli(&args);
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        assert!(result.stderr.is_empty());
        let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(json["ok"], false);
        let diagnostic = &json["diagnostics"][0];
        assert_eq!(diagnostic["code"], code, "{json}");
        let locations = [
            (Some(primary), &diagnostic["location"]),
            (related, &diagnostic["related"][0]),
        ];
        for (value, location) in locations {
            if let Some((start_at, token)) = value {
                let start = text.find(start_at).unwrap();
                assert_eq!(
                    *location,
                    serde_json::json!({"path": "helpers.ncg2m", "range": {"start":start,"end":start + token.len()}}),
                    "{json}"
                );
            }
        }
    }
}

#[test]
fn nor_count_expands_named_routines_and_counts_only_nor_gates() {
    let source = "tests/fixtures/nor-count.ncg2";
    let result = cli(&["nor-count", source]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stderr.is_empty());
    assert_eq!(
        result.stdout,
        b"::demo::one\t1\n::demo::twice\t2\n::demo::direct\t0\n::demo::oscillator\t1\n"
    );

    let selected = cli(&["nor-count", "--entry", "::demo::twice", source]);
    assert!(selected.status.success());
    assert_eq!(selected.stdout, b"::demo::twice\t2\n");

    let storage = cli(&[
        "nor-count",
        "--project",
        "examples/vram/circuitgame2.toml",
        "--entry",
        "::main",
    ]);
    assert!(
        storage.status.success(),
        "{}",
        String::from_utf8_lossy(&storage.stderr)
    );
    assert_eq!(storage.stdout, b"::main\t0\n");
}

#[test]
fn nor_count_reports_expansion_errors_at_the_source_and_prints_no_count() {
    let result = cli(&[
        "nor-count",
        "--entry",
        "::tests::capacity",
        "tests/fixtures/clocked.ncg2",
    ]);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let error = String::from_utf8(result.stderr).unwrap();
    assert!(error.contains("tests/fixtures/clocked.ncg2"), "{error}");
}

#[test]
fn settling_uses_the_project_entry_and_distinguishes_unproved_conditions() {
    let project = "examples/project/circuitgame2.toml";
    let automatic = cli(&["settle", "--project", project]);
    assert_eq!(automatic.status.code(), Some(0));
    assert!(automatic.stderr.is_empty());
    assert!(
        String::from_utf8(automatic.stdout)
            .unwrap()
            .contains("推定上界 1 tick")
    );

    let verified = cli(&[
        "settle",
        "--project",
        project,
        "--within",
        "1",
        "--period",
        "18446744073709551615",
    ]);
    assert_eq!(verified.status.code(), Some(0));
    assert!(verified.stderr.is_empty());
    let result = String::from_utf8(verified.stdout).unwrap();
    assert!(
        result.contains("検証済み") && result.contains("18446744073709551615 tick"),
        "{result}"
    );

    let unproved = cli(&["settle", "--project", project, "--within", "0"]);
    assert_eq!(unproved.status.code(), Some(2));
    assert!(unproved.stderr.is_empty());
    assert!(
        String::from_utf8(unproved.stdout)
            .unwrap()
            .contains("未証明")
    );
}

#[test]
fn settling_reports_clocks_with_storage_or_feedback_as_unestimated() {
    let cases = [
        &["settle", "--project", "examples/vram/circuitgame2.toml"][..],
        &[
            "settle",
            "--entry",
            "::tests::oscillator",
            "tests/fixtures/clocked.ncg2",
        ][..],
    ];
    for args in cases {
        let result = cli(args);
        assert_eq!(
            result.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stderr.is_empty());
        assert!(String::from_utf8(result.stdout).unwrap().contains("未推定"));
    }
}

#[test]
fn settling_rejects_invalid_conditions_before_reading_sources() {
    let cases = [
        ("--period", "2", "--within も指定する"),
        ("--within", "18446744073709551616", "64ビット符号なし整数"),
    ];
    for (option, value, expected) in cases {
        let result = cli(&["settle", option, value, "missing.ncg2"]);
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8(result.stderr).unwrap().contains(expected));
    }
    let result = cli(&["settle", "--within", "1", "--period", "0", "missing.ncg2"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("1 以上の間隔")
    );
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

    let machine = cli(&["check", "--format", "json", "--project", path]);
    assert_eq!(machine.status.code(), Some(1));
    assert!(machine.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&machine.stdout).unwrap();
    assert_eq!(report["diagnostics"][0]["code"], "settings_utf8");
    assert_eq!(report["diagnostics"][0]["location"]["path"], path);
    assert_eq!(
        report["diagnostics"][0]["location"]["range"],
        serde_json::json!({"start": 15, "end": 16}),
    );

    for (source, expected) in [
        ("sources = [\"main.ncg2\"]\nunknown = true\n", "unknown"),
        (
            "sources = [\"main.ncg2\"]\ntick-ns = {\n numerator = 1, denominator = 1\n}\n",
            "設定を解釈できない",
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

    let config = "# 公開する信号\nsources = [\"local.ncg2\"]\n[[vram]]\nname = \"screen\"\nread-output = \"alias\"\n";
    fs::write(&settings, config).unwrap();
    fs::write(
        fixture.0.join("local.ncg2"),
        "(using nor) (memory ram (address 1) (data (bits 1))) (module main (in a d w) (out alias) (def read-data (ram a d w)) (def alias read-data))",
    ).unwrap();
    let result = cli(&["run", "--project", settings.to_str().unwrap()]);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let start = config.find("\"alias\"").unwrap();
    let end = start + "\"alias\"".len();
    let error = String::from_utf8(result.stderr).unwrap();
    assert!(
        error.contains(&format!("{}[{start}..{end}]", settings.display())),
        "{error}"
    );
    assert!(error.contains("回路側の定義 local.ncg2 ["), "{error}");
}

#[test]
fn project_vram_reports_committed_memory_after_each_tick() {
    let project = "examples/vram/circuitgame2.toml";
    let run = [
        "run",
        "--project",
        project,
        "--input",
        "1101",
        "--input",
        "1000",
    ];
    for selection in [&["--vram", "screen"][..], &["--vram=screen"][..]] {
        let mut with_vram = run.to_vec();
        with_vram.extend(selection);
        let trace = cli(&with_vram);
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
            ["0 00 0000", "1 00 0010", "2 10 0010"]
        );
    }
    let trace = cli(&run);
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
        ["0 00", "1 00", "2 10"]
    );
    let missing = cli(&[
        "run",
        "--project",
        project,
        "--vram",
        "screen",
        "--vram",
        "unknown",
    ]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("公開 VRAM \"unknown\"")
    );
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

    let machine = run(&["check", "--format", "json", "invalid.ncg2"]);
    assert_eq!(machine.status.code(), Some(1));
    assert!(machine.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&machine.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["diagnostics"][0]["code"], "syntax");
    assert_eq!(report["diagnostics"][0]["location"]["path"], "invalid.ncg2");

    fs::write(
        folder.0.join("invalid.ncg2"),
        "(using nor) (func broken (in x) (out y) (def y (unknown x)))",
    )
    .unwrap();
    let machine = run(&["check", "--format=json", "invalid.ncg2"]);
    assert_eq!(machine.status.code(), Some(1));
    assert!(machine.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&machine.stdout).unwrap();
    assert_eq!(report["files"], 1);
    assert_eq!(report["diagnostics"][0]["code"], "missing_reference");
    let dump = run(&["dump", "invalid.ncg2"]);
    assert!(dump.status.success());
    let dump: serde_json::Value = serde_json::from_slice(&dump.stdout).unwrap();
    assert_eq!(dump["version"], 1);
    let source = &dump["sources"][0];
    assert_eq!(source["symbols"][0]["name"], "broken");
    let reference = &source["references"][0];
    let form = reference["form"].as_u64().unwrap() as usize;
    assert_eq!(source["forms"][form]["text"], "unknown");
    assert_eq!(
        reference["range"],
        report["diagnostics"][0]["location"]["range"]
    );

    let missing = run(&["check", "invalid.ncg2"]);
    assert_eq!(missing.status.code(), Some(1));
    let stderr = String::from_utf8(missing.stderr).unwrap();
    assert!(stderr.contains("invalid.ncg2 ["));
    assert!(stderr.contains("unknown が見つからない"));

    let wrong_type = "; 日本語のコメント\n(using nor)\n(func bad (in (wide (bits 3))) (out (target (bits 2))) (def target wide))";
    fs::write(folder.0.join("wrong-type.ncg2"), wrong_type).unwrap();
    let plain = run(&["check", "wrong-type.ncg2"]);
    assert_eq!(plain.status.code(), Some(1));
    assert!(
        String::from_utf8(plain.stderr)
            .unwrap()
            .contains("駆動元の型")
    );
    let machine = run(&["check", "--format", "json", "wrong-type.ncg2"]);
    assert_eq!(machine.status.code(), Some(1));
    assert!(machine.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&machine.stdout).unwrap();
    let error = &report["diagnostics"][0];
    let start = wrong_type.rfind("wide").unwrap();
    assert_eq!(error["code"], "output_type_mismatch");
    assert_eq!(error["location"]["path"], "wrong-type.ncg2");
    assert_eq!(
        error["location"]["range"],
        serde_json::json!({"start": start, "end": start + 4})
    );
    assert_eq!(error["related"].as_array().unwrap().len(), 2);

    let expansion = "(using nor) (func pending (in) (out y) (expand make))";
    fs::write(folder.0.join("pending.ncg2"), expansion).unwrap();
    let machine = run(&["check", "--format", "json", "pending.ncg2"]);
    assert_eq!(machine.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&machine.stdout).unwrap();
    let error = &report["diagnostics"][0];
    let call = "(expand make)";
    let start = expansion.find(call).unwrap();
    assert_eq!(error["code"], "needs_expansion");
    assert_eq!(
        error["location"]["range"],
        serde_json::json!({"start": start, "end": start + call.len()})
    );

    let left = "(using nor) (func left (in x) (out y) (def y (right x)))";
    let right = "(func right (in x) (out y) (def y (left x)))";
    fs::write(folder.0.join("left.ncg2"), left).unwrap();
    fs::write(folder.0.join("right.ncg2"), right).unwrap();
    let machine = run(&["check", "--format", "json", "left.ncg2", "right.ncg2"]);
    assert_eq!(machine.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&machine.stdout).unwrap();
    let error = &report["diagnostics"][0];
    assert_eq!(error["code"], "function_cycle");
    let mut edges = [&error["location"], &error["related"][0]];
    edges.sort_by_key(|edge| edge["path"].as_str().unwrap());
    for (edge, path, source, target) in [
        (edges[0], "left.ncg2", left, "right"),
        (edges[1], "right.ncg2", right, "left"),
    ] {
        let start = source.find(target).unwrap();
        assert_eq!(edge["path"], path);
        assert_eq!(
            edge["range"],
            serde_json::json!({"start": start, "end": start + target.len()})
        );
    }

    fs::write(
        folder.0.join("wrong-type.ncg2"),
        wrong_type.replace("(using nor)", ""),
    )
    .unwrap();
    let machine = run(&[
        "check",
        "--format",
        "json",
        "left.ncg2",
        "right.ncg2",
        "wrong-type.ncg2",
    ]);
    assert_eq!(machine.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&machine.stdout).unwrap();
    assert_eq!(report["diagnostics"][0]["code"], "output_type_mismatch");
    assert_eq!(
        report["diagnostics"][0]["location"]["path"],
        "wrong-type.ncg2"
    );
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
fn run_advances_an_input_free_entry_for_the_requested_ticks() {
    for (options, expected) in [
        (&["--ticks", "3"][..], "0 0\n1 1\n2 0\n3 1\n"),
        (&["--ticks=2"][..], "0 0\n1 1\n2 0\n"),
        (&["--ticks", "0"][..], "0 0\n"),
    ] {
        let mut args = vec!["run", "--entry", "::tests::oscillator"];
        args.extend_from_slice(options);
        args.push("tests/fixtures/clocked.ncg2");
        let trace = cli(&args);
        assert!(
            trace.status.success(),
            "{}",
            String::from_utf8_lossy(&trace.stderr)
        );
        assert_eq!(String::from_utf8(trace.stdout).unwrap(), expected);
        assert!(trace.stderr.is_empty());
    }
}

#[test]
fn run_rejects_invalid_tick_options_before_reporting_a_trace() {
    for (options, reason) in [
        (&["--ticks"][..], "回数を指定"),
        (&["--ticks="][..], "64ビット符号なし整数"),
        (&["--ticks", "-1"][..], "64ビット符号なし整数"),
        (&["--ticks", "1.5"][..], "64ビット符号なし整数"),
        (
            &["--ticks", "18446744073709551616"][..],
            "64ビット符号なし整数",
        ),
        (&["--ticks", "1", "--ticks=2"][..], "一度だけ"),
        (&["--ticks", "1", "--input="][..], "併用できない"),
        (&["--input", "1", "--ticks", "1"][..], "併用できない"),
        (
            &["--ticks", "0", "examples/latch.ncg2"][..],
            "入力ポートがない回路",
        ),
    ] {
        let mut args = vec!["run"];
        args.extend_from_slice(options);
        let invalid = cli(&args);
        assert_eq!(invalid.status.code(), Some(1), "{options:?}");
        assert!(invalid.stdout.is_empty(), "{options:?}");
        let stderr = String::from_utf8(invalid.stderr).unwrap();
        assert!(stderr.contains(reason), "{options:?}: {stderr}");
    }
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

#[test]
fn check_validates_uart_bindings_at_their_settings_spans() {
    let folder = std::env::temp_dir().join(format!("ncg2-cli-uart-{}", std::process::id()));
    fs::create_dir(&folder).unwrap();
    fs::write(
        folder.join("main.ncg2"),
        "(using nor) (module main (in rx) (out tx) (def tx (nor rx rx)))",
    )
    .unwrap();
    let settings = folder.join("circuitgame2.toml");
    let path = settings.to_str().unwrap();
    let header = "sources = [\"main.ncg2\"]\ntick-ns = { numerator = 10, denominator = 1 }\n";
    let first = "[[device]]\nname = \"端末\"\nkind = \"uart-terminal\"\n";
    let valid = "bindings = { tx = \"tx\", rx = \"rx\" }\noptions = { baud = 115200 }\n";

    fs::write(&settings, format!("{header}{first}{valid}")).unwrap();
    let checked = cli(&["check", "--project", path]);
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(checked.stderr.is_empty());

    let wrong_direction = format!(
        "{header}{first}bindings = {{ tx = \"rx\", rx = \"rx\" }}\noptions = {{ baud = 115200 }}\n",
    );
    fs::write(&settings, &wrong_direction).unwrap();
    let offset = wrong_direction.find("tx = \"rx\"").unwrap() + "tx = ".len();
    let failed = cli(&["check", "--project", path]);
    assert!(!failed.status.success());
    let message = String::from_utf8(failed.stderr).unwrap();
    assert!(message.contains(&format!("[{offset}..{}]", offset + "\"rx\"".len())));
    assert!(message.contains("方向が合わない"));
    assert!(message.contains("関連箇所"));

    let second = first.replace("端末", "別端末");
    let duplicate = format!("{header}{first}{valid}{second}{valid}");
    fs::write(&settings, &duplicate).unwrap();
    let failed = cli(&["check", "--project", path]);
    assert!(!failed.status.success());
    let message = String::from_utf8(failed.stderr).unwrap();
    assert!(message.contains("回路入力を複数の接続が駆動"));
    for (offset, _) in duplicate.match_indices("rx = \"rx\"") {
        let value = offset + "rx = ".len();
        assert!(message.contains(&format!("[{value}..{}]", value + "\"rx\"".len())));
    }

    let malformed = format!("{header}{first}{}", valid.replace("115200", "0"));
    fs::write(&settings, malformed).unwrap();
    let failed = cli(&["check", "--project", path]);
    assert!(!failed.status.success());
    assert!(
        String::from_utf8(failed.stderr)
            .unwrap()
            .contains("baud には正の整数")
    );
    let coarse = format!(
        "{}{first}{valid}",
        header.replace("numerator = 10", "numerator = 100000")
    );
    fs::write(&settings, coarse).unwrap();
    let failed = cli(&["check", "--project", path]);
    assert!(!failed.status.success());
    assert!(
        String::from_utf8(failed.stderr)
            .unwrap()
            .contains("ビット境界を区別できない")
    );

    fs::remove_dir_all(&folder).unwrap();
}
