use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ncg2"))
        .current_dir(directory)
        .arg("fmt")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn prints_checks_and_writes_one_circuit_without_changing_comments() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.ncg2");
    let source = " (using  nor)\r\n(func gate (in x) (out y)\r(def y (nor x x))) ;末尾\t";
    let formatted = "(using nor)\n(func gate (in x) (out y)\n  (def y (nor x x))) ;末尾\t\n";
    fs::write(&file, source).unwrap();

    let print = run(dir.path(), &["main.ncg2"]);
    assert!(print.status.success(), "{:?}", print.stderr);
    assert_eq!(print.stdout, formatted.as_bytes());
    assert_eq!(fs::read(&file).unwrap(), source.as_bytes());
    let check = run(dir.path(), &["--check", "main.ncg2"]);
    assert_eq!(check.status.code(), Some(1));
    assert!(check.stdout.is_empty());
    assert_eq!(fs::read(&file).unwrap(), source.as_bytes());

    let write = run(dir.path(), &["--write", "main.ncg2"]);
    assert!(write.status.success(), "{:?}", write.stderr);
    assert!(write.stdout.is_empty());
    assert_eq!(fs::read(&file).unwrap(), formatted.as_bytes());
    assert!(run(dir.path(), &["--check", "main.ncg2"]).status.success());
}

#[test]
fn accepts_meta_files_and_preserves_files_with_invalid_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.ncg2m");
    fs::write(
        &file,
        "(defn identity (as-type (Fn (Bool) Bool)\r(fn (value) value)))",
    )
    .unwrap();
    let result = run(dir.path(), &["main.ncg2m"]);
    assert!(result.status.success(), "{:?}", result.stderr);
    assert_eq!(
        result.stdout,
        b"(defn identity (as-type (Fn (Bool) Bool)\n    (fn (value) value)))\n"
    );

    for (name, source, diagnostic) in [
        ("broken.ncg2", "))", "UnexpectedClose"),
        ("broken.ncg2", "(using \"漢\\q\")", "InvalidEscape"),
        ("broken.ncg2", "(unknown x)", "回路フォーム"),
        ("broken.ncg2m", "(unknown x)", "NCG2M フォーム"),
    ] {
        let path = dir.path().join(name);
        fs::write(&path, source).unwrap();
        let failed = run(dir.path(), &["--write", name]);
        assert_eq!(failed.status.code(), Some(1));
        assert!(failed.stdout.is_empty());
        assert!(
            String::from_utf8(failed.stderr)
                .unwrap()
                .contains(diagnostic)
        );
        assert_eq!(fs::read(path).unwrap(), source.as_bytes());
    }

    let duplicate = "(defn dup (as-type (Fn (Bool Bool) Bool) (fn (x x) x)))";
    fs::write(&file, duplicate).unwrap();
    let result = run(dir.path(), &["--write", "main.ncg2m"]);
    assert_eq!(result.status.code(), Some(1));
    let first = duplicate.find("x x").unwrap();
    let second = first + 2;
    let stderr = String::from_utf8(result.stderr).unwrap();
    assert!(stderr.contains(&format!("main.ncg2m [{second}..{}]", second + 1)));
    assert!(stderr.contains(&format!("最初の仮引数 main.ncg2m [{first}..{}]", first + 1)));
    assert_eq!(fs::read(file).unwrap(), duplicate.as_bytes());
}
