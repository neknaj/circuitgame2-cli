//! UTF-8 の NCG2 ファイルを読み込み、検査や組み合わせ評価を行う。

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
        CircuitProject, NameKey, SourceKind, SourceSnapshot, UseTargets, analyze_definitions,
        check_use_aliases, normalize_path, parse_circuit_project,
    },
    syntax::circuit::Head,
};

const HELP: &str = "NCG2 の回路ソースを検査し、組み合わせ関数を評価する。\n\
\n\
  circuitgame2 check <ファイル>...\n\
  circuitgame2 eval --func <関数名> --input <ビット列> <ファイル>...\n\
  circuitgame2 --help\n\
  circuitgame2 --version\n\
\n\
ファイルはプロジェクトルートからの相対パスで指定する。\n\
関数名には ::gate や ::lib::gate のような完全修飾名を使う。\n\
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

fn function_position(
    project: &CircuitProject<'_>,
    targets: &UseTargets<'_, '_>,
    name: &str,
) -> Result<(usize, usize), String> {
    let qualified = name
        .strip_prefix("::")
        .filter(|path| {
            path.split("::")
                .all(|part| !part.is_empty() && !part.contains(':'))
        })
        .ok_or_else(|| "関数名は :: から始まる完全修飾名で指定する。".to_owned())?;
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
    if named_form.head != Head::Func {
        return Err(format!("{name} は func を指す必要がある。"));
    }

    // 展開 API は、名前を持たない項目も含む厳格索引の添字を受ける。
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

fn run(args: &[String]) -> Result<String, String> {
    let command = command(args)?;
    let files = match &command {
        Command::Help => return Ok(HELP.into()),
        Command::Version => return Ok(format!("circuitgame2 {}", env!("CARGO_PKG_VERSION"))),
        Command::Check(files) | Command::Eval { files, .. } => *files,
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

    let Command::Eval { name, input, .. } = command else {
        return Ok(format!("検査成功（{} ファイル）", project.sources().len()));
    };
    let position = function_position(&project, &analysis.aliases.targets, name)?;
    let aliases = check_use_aliases(&analysis.aliases.targets).map_err(diagnostic::alias_error)?;
    let functions = aliases
        .expand_functions(WIDTH_LIMIT, BIT_LIMIT, GATE_LIMIT)
        .map_err(|error| diagnostic::expansion_error(&project, error))?;
    let function = functions
        .get(position.0, position.1)
        .ok_or_else(|| format!("{name} の展開結果が見つからない。"))?;

    if input.len() != function.inputs().total_bits() {
        return Err(format!(
            "{name} の入力には {} ビット必要だが、{} バイト指定された。",
            function.inputs().total_bits(),
            input.len(),
        ));
    }
    let input: Vec<bool> = input
        .bytes()
        .map(|digit| match digit {
            b'0' => Ok(false),
            b'1' => Ok(true),
            _ => Err("--input には 0 と 1 だけを指定する。".to_owned()),
        })
        .collect::<Result<_, _>>()?;
    let output = function
        .netlist()
        .evaluate(&input)
        .map_err(|error| format!("{name} を評価できない ({error:?})。"))?;
    Ok(output
        .into_iter()
        .map(|bit| if bit { '1' } else { '0' })
        .collect())
}

fn main() -> ExitCode {
    let args = env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| "引数は UTF-8 で指定する。".to_owned())
        })
        .collect::<Result<Vec<_>, _>>();
    match args.and_then(|args| run(&args)) {
        Ok(output) => match writeln!(io::stdout().lock(), "{output}") {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(io::stderr().lock(), "結果を出力できない ({error})。");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");
            ExitCode::FAILURE
        }
    }
}
