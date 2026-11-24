//! UTF-8 の NCG2 ファイルを読み込み、回路を検査・評価・実行する。

mod clocked;
mod diagnostic;

use std::{
    collections::HashSet,
    env, fs,
    io::{self, Write},
    path::{Component, Path},
    process::ExitCode,
};

use circuitgame2_core::{
    project::{
        CircuitProject, ModuleBodyPosition, NameKey, SourceKind, SourceSnapshot, UseTargets,
        analyze_definitions, check_use_aliases, normalize_path, parse_circuit_project,
    },
    syntax::circuit::Head,
};

const HELP: &str = "NCG2 の回路ソースを検査し、関数を評価してモジュールを実行する。\n\
\n\
  circuitgame2 check <ファイル>...\n\
  circuitgame2 eval --func <関数名> --input <ビット列> <ファイル>...\n\
  circuitgame2 run [--entry <モジュール名>] [--input <ビット列>]... <ファイル>...\n\
  circuitgame2 --help\n\
  circuitgame2 --version\n\
\n\
ファイルはプロジェクトルートからの相対パスで指定する。\n\
宣言名には ::gate や ::lib::gate のような完全修飾名を使う。\n\
run の既定の開始点は ::main。入力ごとに 1 tick 進め、reset 直後から観測値を出力する。\n\
入出力はポートの宣言順に並べ、各ポート内は最下位ビットから記す。";

const WIDTH_LIMIT: u64 = 65_536;
const BIT_LIMIT: usize = 1_048_576;
const GATE_LIMIT: usize = 1_048_576;

enum Command<'a> {
    Help,
    Version,
    Check(&'a [String]),
    Eval {
        name: &'a str,
        input: &'a str,
        files: &'a [String],
    },
    Run {
        entry: &'a str,
        frames: Vec<&'a str>,
        files: &'a [String],
    },
}

fn run_command(mut args: &[String]) -> Result<Command<'_>, String> {
    let mut entry = None;
    let mut frames = Vec::new();
    while let Some((option, rest)) = args.split_first() {
        match option.as_str() {
            "--entry" if entry.is_none() => {
                let (name, rest) = rest.split_first().ok_or("--entry には宣言名を指定する。")?;
                entry = Some(name.as_str());
                args = rest;
            }
            "--input" => {
                let (frame, rest) = rest
                    .split_first()
                    .ok_or("--input にはビット列を指定する。")?;
                frames.push(frame.as_str());
                args = rest;
            }
            "--input=" => {
                frames.push("");
                args = rest;
            }
            "--" => {
                args = rest;
                break;
            }
            _ if option.starts_with('-') => {
                return Err(format!("run では {option} をここに指定できない。"));
            }
            _ => break,
        }
    }
    if args.is_empty() {
        return Err("run には 1 件以上のソースファイルを指定する。".into());
    }
    Ok(Command::Run {
        entry: entry.unwrap_or("::main"),
        frames,
        files: args,
    })
}

fn command(args: &[String]) -> Result<Command<'_>, String> {
    match args {
        [flag] if flag == "--help" || flag == "-h" => Ok(Command::Help),
        [flag] if flag == "--version" => Ok(Command::Version),
        [check, files @ ..] if check == "check" && !files.is_empty() => Ok(Command::Check(files)),
        [eval, func_flag, name, input_flag, input, files @ ..]
            if eval == "eval"
                && func_flag == "--func"
                && input_flag == "--input"
                && !files.is_empty() =>
        {
            Ok(Command::Eval { name, input, files })
        }
        [run, args @ ..] if run == "run" => run_command(args),
        _ => Err("使い方は circuitgame2 --help で確認できる。".into()),
    }
}

fn source_path(path: &str) -> Result<String, String> {
    if Path::new(path)
        .components()
        .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
    {
        return Err(format!(
            "{path} はプロジェクトルートからの相対パスで指定する。"
        ));
    }
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    let id = normalize_path(&path)
        .map_err(|error| format!("{path} のパスを正規化できない ({error:?})。"))?;
    if id.source_kind() != Some(SourceKind::Circuit) {
        return Err(format!("{path} の拡張子には .ncg2 を指定する。"));
    }

    // 診断のパスからも同じファイルを開けるよう、正規化したパスで読み込む。
    Ok(id.components().join("/"))
}

fn declaration_position(
    project: &CircuitProject<'_>,
    targets: &UseTargets<'_, '_>,
    name: &str,
    kind: Head,
) -> Result<(usize, usize), String> {
    let (label, head) = if kind == Head::Func {
        ("関数名", "func")
    } else {
        ("モジュール名", "module")
    };
    let qualified = name
        .strip_prefix("::")
        .filter(|path| {
            path.split("::")
                .all(|part| !part.is_empty() && !part.contains(':'))
        })
        .ok_or_else(|| format!("{label}は :: から始まる完全修飾名で指定する。"))?;
    let (parent, name_part) = qualified
        .rsplit_once("::")
        .map_or((None, qualified), |(parent, tail)| (Some(parent), tail));
    let key = NameKey {
        parent,
        name: name_part,
    };
    let (source, named_form) = targets
        .exports()
        .declarations()
        .declaration(key)
        .ok_or_else(|| format!("{name} の宣言が見つからない。"))?;
    if named_form.head != kind {
        return Err(format!("{name} は {head} を指す必要がある。"));
    }

    // Core の位置は名前を持たない項目も含む厳格索引から求める。
    let file = project
        .sources()
        .iter()
        .position(|candidate| candidate.id.same_file(&source.id))
        .ok_or_else(|| format!("{name} の宣言元ファイルが見つからない。"))?;
    let entry = project
        .circuits()
        .get(file)
        .and_then(|parsed| {
            parsed
                .syntax
                .entries
                .iter()
                .position(|entry| entry.form == named_form.form)
        })
        .ok_or_else(|| format!("{name} の宣言位置が見つからない。"))?;
    Ok((file, entry))
}

fn run(args: &[String], stdout: &mut impl Write) -> Result<(), String> {
    let command = command(args)?;
    let files = match &command {
        Command::Help => return writeln!(stdout, "{HELP}").map_err(output_error),
        Command::Version => {
            return writeln!(stdout, "circuitgame2 {}", env!("CARGO_PKG_VERSION"))
                .map_err(output_error);
        }
        Command::Check(files) | Command::Eval { files, .. } | Command::Run { files, .. } => *files,
    };
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    for path in files {
        let path = source_path(path)?;
        if seen.insert(path.clone()) {
            let text = fs::read_to_string(&path)
                .map_err(|error| format!("{path} を UTF-8 で読み込めない ({error})。"))?;
            sources.push((path, text));
        }
    }
    let snapshots: Vec<_> = sources
        .iter()
        .map(|(path, text)| SourceSnapshot {
            path,
            revision: 1,
            text,
        })
        .collect();
    let project = parse_circuit_project(&snapshots).map_err(diagnostic::project_error)?;
    let analysis = analyze_definitions(&project).map_err(diagnostic::analysis_error)?;
    diagnostic::alias_warnings(&analysis.aliases);

    let (name, kind) = match &command {
        Command::Eval { name, .. } => (*name, Head::Func),
        Command::Run { entry, .. } => (*entry, Head::Module),
        _ => {
            return writeln!(stdout, "検査成功（{} ファイル）", project.sources().len())
                .map_err(output_error);
        }
    };
    let (file, entry) = declaration_position(&project, &analysis.aliases.targets, name, kind)?;
    let aliases = check_use_aliases(&analysis.aliases.targets).map_err(diagnostic::alias_error)?;
    if let Command::Run { frames, .. } = command {
        return clocked::trace(
            &project,
            &aliases,
            ModuleBodyPosition { file, entry },
            name,
            &frames,
            stdout,
        );
    }
    let Command::Eval { input, .. } = command else {
        return Err("評価する関数を指定する。".into());
    };
    let functions = aliases
        .expand_functions(WIDTH_LIMIT, BIT_LIMIT, GATE_LIMIT)
        .map_err(|error| diagnostic::expansion_error(&project, error))?;
    let function = functions
        .get(file, entry)
        .ok_or_else(|| format!("{name} の展開結果が見つからない。"))?;

    let width = function.inputs().total_bits();
    if input.len() != width {
        return Err(format!(
            "{name} の入力には {width} ビット必要だが、{} バイト指定された。",
            input.len(),
        ));
    }
    if !input_is_binary(input) {
        return Err("--input には 0 と 1 だけを指定する。".into());
    }
    let output = function
        .netlist()
        .evaluate(&input_bits(input))
        .map_err(|error| format!("{name} を評価できない ({error:?})。"))?;
    writeln!(stdout, "{}", format_bits(output)).map_err(output_error)
}

/// 入力が 0 と 1 だけで構成され、ポートの合計幅に一致することを確認する。
fn validate_input(input: &str, width: usize) -> Result<(), String> {
    if !input_is_binary(input) {
        return Err("--input には 0 と 1 だけを指定する。".into());
    }
    if input.len() != width {
        return Err(format!(
            "入力は {width} ビット必要だが、{} ビット指定された。",
            input.len()
        ));
    }
    Ok(())
}

fn input_is_binary(input: &str) -> bool {
    input.bytes().all(|byte| matches!(byte, b'0' | b'1'))
}

/// 検査済みの ASCII ビット列を、各ポートのビット順を保ったまま変換する。
fn input_bits(input: &str) -> Vec<bool> {
    input.bytes().map(|bit| bit == b'1').collect()
}

fn format_bits(bits: Vec<bool>) -> String {
    bits.into_iter()
        .map(|bit| if bit { '1' } else { '0' })
        .collect()
}

fn output_error(error: io::Error) -> String {
    format!("結果を出力できない ({error})。")
}

fn main() -> ExitCode {
    let args = env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| "引数は UTF-8 で指定する。".to_owned())
        })
        .collect::<Result<Vec<_>, _>>();
    match args.and_then(|args| {
        let mut stdout = io::stdout().lock();
        run(&args, &mut stdout)?;
        stdout.flush().map_err(output_error)
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");
            ExitCode::FAILURE
        }
    }
}
