//! ファイルの構文を検査し、共通の整形結果を出力する。

use std::{fs, io::Write, path::Path, process::ExitCode};

use circuitgame2_core::{
    ncg2m::file::index_meta_file,
    syntax::{
        circuit::{CircuitFormError, index_named_build_forms},
        formatting::format_sexpr,
    },
};

enum Mode {
    Print,
    Check,
    Write,
}

pub(super) struct FormatCommand<'a> {
    path: &'a str,
    mode: Mode,
}

pub(super) fn command(args: &[String]) -> Result<FormatCommand<'_>, String> {
    let (mode, paths) = match args.first().map(String::as_str) {
        Some("--check") => (Mode::Check, &args[1..]),
        Some("--write") if cfg!(target_os = "wasi") => {
            return Err("WASI 版の fmt は標準出力と --check に対応する。".into());
        }
        Some("--write") => (Mode::Write, &args[1..]),
        _ => (Mode::Print, args),
    };
    match paths {
        [delimiter, path] if delimiter == "--" => Ok(FormatCommand { path, mode }),
        [path] if !path.starts_with('-') => Ok(FormatCommand { path, mode }),
        _ => Err("ncg2 fmt [--check | --write] <ファイル> の形式で指定する。".into()),
    }
}

#[cfg(not(target_os = "wasi"))]
fn write_formatted(path: &Path, original: &str, output: &str) -> Result<(), String> {
    let message = |error| format!("{} を置き換えられない ({error})。", path.display());
    let metadata = fs::symlink_metadata(path).map_err(message)?;
    if !metadata.is_file() || metadata.permissions().readonly() {
        return Err(format!(
            "{} は書き換え可能な通常ファイルではない。",
            path.display()
        ));
    }
    let directory = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(message)?;
    temporary.write_all(output.as_bytes()).map_err(message)?;
    temporary
        .as_file()
        .set_permissions(metadata.permissions())
        .map_err(message)?;
    temporary.as_file().sync_all().map_err(message)?;
    if fs::read_to_string(path).map_err(message)? != original {
        return Err(format!("{} は整形中に更新された。", path.display()));
    }
    temporary
        .persist(path)
        .map_err(|error| message(error.error))?;
    Ok(())
}

pub(super) fn run(
    command: &FormatCommand<'_>,
    stdout: &mut impl Write,
) -> Result<ExitCode, String> {
    let path = Path::new(command.path);
    let meta = match path.extension().and_then(|extension| extension.to_str()) {
        Some("ncg2") => false,
        Some("ncg2m") => true,
        _ => {
            return Err(format!(
                "{} は .ncg2 または .ncg2m を指定する。",
                command.path
            ));
        }
    };
    let source = fs::read_to_string(path)
        .map_err(|error| format!("{} を UTF-8 で読み込めない ({error})。", command.path))?;
    if meta {
        index_meta_file(&source)
            .map_err(|error| super::diagnostic::meta_file_error(command.path, error))?;
    } else {
        index_named_build_forms(&source)
            .map_err(|error| super::diagnostic::circuit_form_error(command.path, error))?;
    }
    let output = format_sexpr(&source).map_err(|error| {
        super::diagnostic::circuit_form_error(command.path, CircuitFormError::Syntax(error))
    })?;
    match command.mode {
        Mode::Print => stdout
            .write_all(output.as_bytes())
            .map_err(super::output_error)?,
        Mode::Check if source != output => {
            return Err(format!("{} は整形が必要。", command.path));
        }
        #[cfg(not(target_os = "wasi"))]
        Mode::Write if source != output => write_formatted(path, &source, &output)?,
        Mode::Check | Mode::Write => {}
    }
    Ok(ExitCode::SUCCESS)
}
