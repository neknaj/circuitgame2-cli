//! UTF-8 の NCG2 ファイルを読み込み、回路を検査・評価・実行する。

mod clocked;
mod config;
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
  circuitgame2 check --project <設定ファイル>\n\
  circuitgame2 eval --func <関数名> --input <ビット列> <ファイル>...\n\
  circuitgame2 eval --func <関数名> --input <ビット列> --project <設定ファイル>\n\
  circuitgame2 run [--entry <モジュール名>] [--input <ビット列>]... <ファイル>...\n\
  circuitgame2 run --project <設定ファイル> [--input <ビット列>]... [--vram <公開名>]...\n\
  circuitgame2 --help\n\
  circuitgame2 --version\n\
\n\
ファイルはコマンドを実行するディレクトリからの相対パスで指定する。\n\
--project を使う場合、ソースは設定ファイルのディレクトリを基準に読む。\n\
run の --project はソースファイルの列挙や --entry と併用できない。\n\
宣言名には ::gate や ::lib::gate のような完全修飾名を使う。\n\
run の既定の開始点は ::main。--input 一つにつき 1 tick 進める。\n\
入力ポートがない回路は --input= で 1 tick 進める。\n\
run は reset 直後を tick 0 とし、各行に tick 番号と出力ビット列を表示する。\n\
--vram を指定すると、指定順に各 VRAM の記憶部を追加する。\n\
入出力はポートの宣言順に並べ、各ポート内は最下位ビットから記す。";

const WIDTH_LIMIT: u64 = 65_536;
const BIT_LIMIT: usize = 1_048_576;
const GATE_LIMIT: usize = 1_048_576;

enum Command<'a> {
    Help,
    Version,
    Check(SourceInput<'a>),
    Eval {
        name: &'a str,
        input: &'a str,
        sources: SourceInput<'a>,
    },
    Run {
        entry: &'a str,
        frames: Vec<&'a str>,
        vram: Vec<&'a str>,
        sources: SourceInput<'a>,
    },
}

#[derive(Clone, Copy)]
enum SourceInput<'a> {
    Files(&'a [String]),
    Project(&'a str),
}

fn option_value<'a>(option: &str, args: &'a [String]) -> Result<(&'a str, &'a [String]), String> {
    match args.split_first() {
        Some((value, rest)) if !value.starts_with('-') => Ok((value, rest)),
        _ => Err(format!("{option} の値を指定する。")),
    }
}

fn source_input(args: &[String]) -> Result<SourceInput<'_>, String> {
    if let Some((first, rest)) = args.split_first() {
        match first.as_str() {
            "--project" => {
                let (path, remaining) = option_value("--project", rest)?;
                if remaining.is_empty() {
                    return Ok(SourceInput::Project(path));
                }
            }
            "--" if !rest.is_empty() => return Ok(SourceInput::Files(rest)),
            _ if !args.iter().any(|arg| arg == "--project") => return Ok(SourceInput::Files(args)),
            _ => {}
        }
        return Err("--project とソースファイルの列挙は併用できない。".into());
    }
    Err("ソースファイルまたは --project を指定する。".into())
}

fn run_command(mut args: &[String]) -> Result<Command<'_>, String> {
    let mut entry = None;
    let mut project = None;
    let mut frames = Vec::new();
    let mut vram = Vec::new();
    let mut end_options = false;
    while let Some((option, rest)) = args.split_first() {
        match option.as_str() {
            "--entry" if entry.is_none() => {
                let (name, rest) = rest
                    .split_first()
                    .filter(|(value, _)| {
                        *value != "--project" && *value != "--vram" && !value.starts_with("--vram=")
                    })
                    .ok_or("--entry には宣言名を指定する。")?;
                entry = Some(name.as_str());
                args = rest;
            }
            "--project" if project.is_none() => {
                let (path, rest) = option_value("--project", rest)?;
                project = Some(path);
                args = rest;
            }
            "--input" => {
                let (frame, rest) = rest
                    .split_first()
                    .filter(|(value, _)| {
                        *value != "--project" && *value != "--vram" && !value.starts_with("--vram=")
                    })
                    .ok_or("--input にはビット列を指定する。")?;
                frames.push(frame.as_str());
                args = rest;
            }
            "--input=" => {
                frames.push("");
                args = rest;
            }
            "--vram" => {
                let (name, rest) = option_value("--vram", rest)?;
                vram.push(name);
                args = rest;
            }
            option if option.starts_with("--vram=") => {
                vram.push(&option["--vram=".len()..]);
                args = rest;
            }
            "--" => {
                args = rest;
                end_options = true;
                break;
            }
            _ if option.starts_with('-') => {
                return Err(format!("run では {option} をここに指定できない。"));
            }
            _ => break,
        }
    }
    let sources = match project {
        Some(path) if args.is_empty() && entry.is_none() => SourceInput::Project(path),
        Some(_) => {
            return Err(
                "run の --project はソースファイルの列挙や --entry と併用できない。".into(),
            );
        }
        None if !vram.is_empty() => return Err("--vram を使う場合は --project を指定する。".into()),
        None if end_options && !args.is_empty() => SourceInput::Files(args),
        None => source_input(args)?,
    };
    Ok(Command::Run {
        entry: entry.unwrap_or("::main"),
        frames,
        vram,
        sources,
    })
}

fn command(args: &[String]) -> Result<Command<'_>, String> {
    match args {
        [flag] if flag == "--help" || flag == "-h" => Ok(Command::Help),
        [flag] if flag == "--version" => Ok(Command::Version),
        [check, files @ ..] if check == "check" => Ok(Command::Check(source_input(files)?)),
        [eval, func_flag, name, input_flag, input, files @ ..]
            if eval == "eval"
                && func_flag == "--func"
                && input_flag == "--input"
                && input != "--project"
                && !files.is_empty() =>
        {
            Ok(Command::Eval {
                name,
                input,
                sources: source_input(files)?,
            })
        }
        [run, args @ ..] if run == "run" => run_command(args),
        _ => Err("使い方は circuitgame2 --help で確認できる。".into()),
    }
}

fn source_path(path: &str, from_config: bool) -> Result<String, String> {
    if !from_config
        && Path::new(path)
            .components()
            .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
    {
        return Err(format!(
            "{path} はプロジェクトルートからの相対パスで指定する。"
        ));
    }
    let path = if cfg!(windows) && !from_config {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    let id = normalize_path(&path)
        .map_err(|error| format!("{path} のパスを正規化できない ({error:?})。"))?;
    match id.source_kind() {
        Some(SourceKind::Circuit) => {}
        Some(SourceKind::Functional) if from_config => {
            return Err(format!(
                "{path} は NCG2M のソース。この CLI は NCG2M の実行に未対応。"
            ));
        }
        _ if from_config => {
            return Err(format!(
                "{path} の拡張子には .ncg2 または .ncg2m を指定する。"
            ));
        }
        _ => return Err(format!("{path} の拡張子には .ncg2 を指定する。")),
    }

    // 正規化で Windows のドライブ指定が先頭に現れる場合も、相対パスとして扱わない。
    let normalized = id.components().join("/");
    if Path::new(&normalized)
        .components()
        .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
    {
        return Err(format!(
            "{path} はプロジェクトルートからの相対パスで指定する。"
        ));
    }
    Ok(normalized)
}

/// 正規化後のパスが同じソースは、最初の指定だけを読み込む。
fn read_source(
    sources: &mut Vec<(String, String)>,
    seen: &mut HashSet<String>,
    base: &Path,
    path: String,
) -> Result<(), String> {
    if !seen.contains(&path) {
        let host_path = base.join(&path);
        let text = fs::read_to_string(&host_path).map_err(|error| {
            format!(
                "{} を UTF-8 で読み込めない ({error})。",
                host_path.display()
            )
        })?;
        seen.insert(path.clone());
        sources.push((path, text));
    }
    Ok(())
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
    let input = match &command {
        Command::Help => return writeln!(stdout, "{HELP}").map_err(output_error),
        Command::Version => {
            return writeln!(stdout, "circuitgame2 {}", env!("CARGO_PKG_VERSION"))
                .map_err(output_error);
        }
        Command::Check(sources) | Command::Eval { sources, .. } | Command::Run { sources, .. } => {
            *sources
        }
    };
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    let settings = match input {
        SourceInput::Files(files) => {
            for path in files {
                read_source(
                    &mut sources,
                    &mut seen,
                    Path::new(""),
                    source_path(path, false)?,
                )?;
            }
            None
        }
        SourceInput::Project(path) => {
            let path = Path::new(path);
            let settings = config::read(path)?;
            let base = path.parent().unwrap_or(Path::new(""));
            for value in settings.sources.get_ref() {
                let result = source_path(value.get_ref(), true)
                    .and_then(|source| read_source(&mut sources, &mut seen, base, source));
                result.map_err(|error| config::at(path, Some(value.span()), &error))?;
            }
            Some((path, settings))
        }
    };
    let snapshots: Vec<_> = sources
        .iter()
        .map(|(path, text)| SourceSnapshot {
            path,
            revision: 1,
            text,
        })
        .collect();
    let source_diagnostic = |message: String| match &settings {
        Some((path, _)) => config::source_diagnostic(path, &message),
        None => message,
    };
    let project = parse_circuit_project(&snapshots)
        .map_err(|error| source_diagnostic(diagnostic::project_error(error)))?;
    let analysis = analyze_definitions(&project).map_err(|error| {
        source_diagnostic(diagnostic::analysis_error(error, &source_diagnostic))
    })?;
    diagnostic::alias_warnings(&analysis.aliases, &source_diagnostic);

    let (name, kind) = match &command {
        Command::Eval { name, .. } => (*name, Head::Func),
        Command::Run { entry, .. } => {
            let configured = settings
                .as_ref()
                .and_then(|(_, settings)| settings.entry.as_ref());
            (
                configured.map_or(*entry, |entry| entry.get_ref()),
                Head::Module,
            )
        }
        _ => {
            return writeln!(stdout, "検査成功（{} ファイル）", project.sources().len())
                .map_err(output_error);
        }
    };
    let (file, entry) = declaration_position(&project, &analysis.aliases.targets, name, kind)
        .map_err(|error| match (&command, &settings) {
            (Command::Run { .. }, Some((path, settings))) => config::at(
                path,
                settings.entry.as_ref().map(toml::Spanned::span),
                &error,
            ),
            _ => error,
        })?;
    let aliases = check_use_aliases(&analysis.aliases.targets)
        .map_err(|error| source_diagnostic(diagnostic::alias_error(error)))?;
    if let Command::Run { frames, vram, .. } = command {
        return clocked::trace(
            &project,
            &aliases,
            ModuleBodyPosition { file, entry },
            clocked::TraceOptions {
                name,
                frames: &frames,
                vram: &vram,
                settings: settings.as_ref().map(|(path, settings)| (*path, settings)),
            },
            stdout,
        );
    }
    let Command::Eval { input, .. } = command else {
        return Err("評価する関数を指定する。".into());
    };
    let functions = aliases
        .expand_functions(WIDTH_LIMIT, BIT_LIMIT, GATE_LIMIT)
        .map_err(|error| source_diagnostic(diagnostic::expansion_error(&project, error)))?;
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

fn format_bits(bits: impl IntoIterator<Item = bool>) -> String {
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
