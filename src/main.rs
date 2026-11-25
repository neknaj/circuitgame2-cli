//! UTF-8 のソースを読み込んで静的に検査し、回路を評価・実行する。

mod check_body;
mod check_ncg2m;
mod check_report;
mod clocked;
mod device_config;
mod diagnostic;
mod dump;
mod formatting;
mod nor_count;
mod rp_plan;
mod settling;

use std::{
    collections::HashSet,
    env, fs,
    io::{self, Write},
    num::NonZeroU64,
    path::{Component, Path},
    process::ExitCode,
};

use circuitgame2_cli::config;
use circuitgame2_core::{
    ncg2m::call_evaluation::scalar::ScalarValue,
    project::{
        CircuitProject, DefinitionAnalysisError, ModuleBodyPosition, NameKey,
        PreparedDefinitionError, SourceKind, SourceSnapshot, UseAliases, UseTargets,
        normalize_path, parse_circuit_project, prepare_definition_analysis,
    },
    syntax::circuit::Head,
};

const HELP: &str = "NCG2 の回路ソースを検査し、関数を評価してモジュールを実行する。\n\
\n\
  ncg2 check [--format json] <ファイル>...\n\
  ncg2 check [--format json] --project <設定ファイル>\n\
  ncg2 dump <ファイル>...\n\
  ncg2 dump --project <設定ファイル>\n\
  ncg2 nor-count [--entry <宣言名>] <ファイル>...\n\
  ncg2 nor-count [--entry <宣言名>] --project <設定ファイル>\n\
  ncg2 fmt [--check | --write] <ファイル>\n\
  ncg2 eval --func <関数名> --input <ビット列> <ファイル>...\n\
  ncg2 eval --func <関数名> --input <ビット列> --project <設定ファイル>\n\
  ncg2 eval-meta --defn <名前> [--arg <値>]... [--fuel <回数>] [--steps] <ファイル>...\n\
  ncg2 eval-meta --defn <名前> [--arg <値>]... [--fuel <回数>] [--steps] --project <設定ファイル>\n\
  ncg2 run [--entry <モジュール名>] [--input <ビット列> ... | --ticks <回数>] <ファイル>...\n\
  ncg2 run --project <設定ファイル> [--input <ビット列> ... | --ticks <回数>] [--vram <公開名>]...\n\
  ncg2 settle [--within <tick 数> [--period <間隔>]] [--entry <モジュール名>] <ファイル>...\n\
  ncg2 settle [--within <tick 数> [--period <間隔>]] --project <設定ファイル>\n\
  ncg2 rp-plan --project <設定ファイル> --board <ボード設定>\n\
  ncg2 --help\n\
  ncg2 --version\n\
\n\
ファイルはコマンドを実行するディレクトリからの相対パスで指定する。\n\
--project を使う場合、ソースは設定ファイルのディレクトリを基準に読む。\n\
run と settle の --project はソースファイルの列挙や --entry と併用できない。\n\
宣言名には ::gate や ::lib::gate のような完全修飾名を使う。\n\
eval-meta の --arg は true、false、Nat:<十進整数>、Int:<十進整数>。\n\
eval-meta の --steps は実行した評価ステップ数を標準エラー出力に表示する。\n\
run の既定の開始点は ::main。--input 一つにつき 1 tick 進める。\n\
入力ポートがない回路は --ticks <回数> で指定回数だけ進める。--input= でも 1 tick 進められる。\n\
--ticks と --input は併用できない。--ticks 0 は reset 直後の出力だけを表示する。\n\
run は reset 直後を tick 0 とし、各行に tick 番号と出力ビット列を表示する。\n\
--vram を指定すると、指定順に各 VRAM の記憶部を追加する。\n\
settle は固定入力での定常化を解析する。ネイティブ版では未推定・未証明の終了コードは 2。\n\
check は .ncg2 の回路または .ncg2m の補助関数を静的に検査する。\n\
nor-count は展開後の NOR ゲート数を完全修飾名ごとに表示する。\n\
fmt は整形結果を標準出力に表示する。--check は整形が必要な場合に終了コード 1 を返す。\n\
ネイティブ版の fmt --write は構文検査と整形が成功した場合にファイルを置き換える。\n\
入出力はポートの宣言順に並べ、各ポート内は最下位ビットから記す。";

const WIDTH_LIMIT: u64 = circuitgame2_cli::CLOCKED_LIMITS.width;
const BIT_LIMIT: usize = circuitgame2_cli::CLOCKED_LIMITS.wiring_bits;
const GATE_LIMIT: usize = circuitgame2_cli::CLOCKED_LIMITS.gates;

enum Command<'a> {
    Help,
    Version,
    Check {
        sources: SourceInput<'a>,
        json: bool,
    },
    Dump(SourceInput<'a>),
    NorCount {
        entry: Option<&'a str>,
        sources: SourceInput<'a>,
    },
    RpPlan {
        project: &'a str,
        board: &'a str,
    },
    Format(formatting::FormatCommand<'a>),
    Eval {
        name: &'a str,
        input: &'a str,
        sources: SourceInput<'a>,
    },
    EvalMeta {
        name: &'a str,
        arguments: Vec<ScalarValue>,
        fuel: Option<NonZeroU64>,
        steps: bool,
        sources: SourceInput<'a>,
    },
    Run {
        entry: &'a str,
        frames: clocked::InputFrames<'a>,
        vram: Vec<&'a str>,
        sources: SourceInput<'a>,
    },
    Settle {
        entry: &'a str,
        condition: Option<settling::Condition>,
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

fn module_sources<'a>(
    command: &str,
    project: Option<&'a str>,
    entry: Option<&str>,
    files: &'a [String],
    end_options: bool,
) -> Result<SourceInput<'a>, String> {
    match project {
        Some(path) if files.is_empty() && entry.is_none() => Ok(SourceInput::Project(path)),
        Some(_) => Err(format!(
            "{command} の --project はソースファイルの列挙や --entry と併用できない。"
        )),
        None if end_options && !files.is_empty() => Ok(SourceInput::Files(files)),
        None => source_input(files),
    }
}

fn run_command(mut args: &[String]) -> Result<Command<'_>, String> {
    let mut entry = None;
    let mut project = None;
    let mut frames = Vec::new();
    let mut ticks = None;
    let mut vram = Vec::new();
    let mut end_options = false;
    while let Some((option, rest)) = args.split_first() {
        match option.as_str() {
            "--entry" if entry.is_none() => {
                let (name, rest) = rest
                    .split_first()
                    .filter(|(value, _)| {
                        *value != "--project"
                            && *value != "--vram"
                            && !value.starts_with("--vram=")
                            && *value != "--ticks"
                            && !value.starts_with("--ticks=")
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
                        *value != "--project"
                            && *value != "--vram"
                            && !value.starts_with("--vram=")
                            && *value != "--ticks"
                            && !value.starts_with("--ticks=")
                    })
                    .ok_or("--input にはビット列を指定する。")?;
                frames.push(frame.as_str());
                args = rest;
            }
            "--input=" => {
                frames.push("");
                args = rest;
            }
            option if option == "--ticks" || option.starts_with("--ticks=") => {
                if ticks.is_some() {
                    return Err("--ticks は一度だけ指定できる。".into());
                }
                let (value, rest) = match option.strip_prefix("--ticks=") {
                    Some(value) => (value, rest),
                    None => {
                        let (value, rest) =
                            rest.split_first().ok_or("--ticks には回数を指定する。")?;
                        (value.as_str(), rest)
                    }
                };
                ticks = Some(tick_count("--ticks", value)?);
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
    if project.is_none() && !vram.is_empty() {
        return Err("--vram を使う場合は --project を指定する。".into());
    }
    let frames = match ticks {
        Some(_) if !frames.is_empty() => return Err("--ticks と --input は併用できない。".into()),
        Some(ticks) => clocked::InputFrames::EmptyTicks(ticks),
        None => clocked::InputFrames::Explicit(frames),
    };
    let sources = module_sources("run", project, entry, args, end_options)?;
    Ok(Command::Run {
        entry: entry.unwrap_or("::main"),
        frames,
        vram,
        sources,
    })
}

fn tick_count(option: &str, value: &str) -> Result<u64, String> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        if let Ok(value) = value.parse() {
            return Ok(value);
        }
    }
    Err(format!(
        "{option} には 0 以上の64ビット符号なし整数を指定する。"
    ))
}

fn settle_command(mut args: &[String]) -> Result<Command<'_>, String> {
    let mut entry = None;
    let mut project = None;
    let mut within = None;
    let mut period = None;
    let mut end_options = false;
    while let Some((option, rest)) = args.split_first() {
        match option.as_str() {
            "--entry" if entry.is_none() => {
                let (value, rest) = option_value("--entry", rest)?;
                entry = Some(value);
                args = rest;
            }
            "--project" if project.is_none() => {
                let (value, rest) = option_value("--project", rest)?;
                project = Some(value);
                args = rest;
            }
            "--within" if within.is_none() => {
                let (value, rest) = option_value("--within", rest)?;
                within = Some(tick_count("--within", value)?);
                args = rest;
            }
            "--period" if period.is_none() => {
                let (value, rest) = option_value("--period", rest)?;
                period = Some(
                    NonZeroU64::new(tick_count("--period", value)?)
                        .ok_or("--period には 1 以上の間隔を指定する。")?,
                );
                args = rest;
            }
            "--" => {
                args = rest;
                end_options = true;
                break;
            }
            _ if option.starts_with('-') => {
                return Err(format!("settle では {option} をここに指定できない。"));
            }
            _ => break,
        }
    }
    if period.is_some() && within.is_none() {
        return Err("--period を指定する場合は --within も指定する。".into());
    }
    let sources = module_sources("settle", project, entry, args, end_options)?;
    Ok(Command::Settle {
        entry: entry.unwrap_or("::main"),
        condition: within.map(|within| settling::Condition {
            within,
            period: period.unwrap_or(NonZeroU64::MIN),
        }),
        sources,
    })
}

fn nor_count_command(mut args: &[String]) -> Result<Command<'_>, String> {
    let mut entry = None;
    let mut project = None;
    let mut end_options = false;
    while let Some((option, rest)) = args.split_first() {
        match option.as_str() {
            "--entry" if entry.is_none() => {
                let (value, rest) = option_value("--entry", rest)?;
                entry = Some(value);
                args = rest;
            }
            "--project" if project.is_none() => {
                let (value, rest) = option_value("--project", rest)?;
                project = Some(value);
                args = rest;
            }
            "--" => {
                args = rest;
                end_options = true;
                break;
            }
            _ if option.starts_with('-') => {
                return Err(format!("nor-count では {option} をここに指定できない。"));
            }
            _ => break,
        }
    }
    let sources = match project {
        Some(path) if args.is_empty() => SourceInput::Project(path),
        Some(_) => return Err("--project とソースファイルの列挙は併用できない。".into()),
        None if end_options && !args.is_empty() => SourceInput::Files(args),
        None => source_input(args)?,
    };
    Ok(Command::NorCount { entry, sources })
}

fn eval_meta_command(args: &[String]) -> Result<Command<'_>, String> {
    let [flag, name, rest @ ..] = args else {
        return Err("eval-meta には --defn と補助関数名を指定する。".into());
    };
    if flag != "--defn" || name.starts_with('-') {
        return Err("eval-meta には --defn と補助関数名を指定する。".into());
    }
    let mut remaining = rest;
    let mut arguments = Vec::new();
    let mut fuel = None;
    let mut steps = false;
    while let Some((option, rest)) = remaining.split_first() {
        match option.as_str() {
            "--arg" => {
                let (value, next) = option_value(option, rest)?;
                arguments.push(check_ncg2m::parse_argument(value, arguments.len() + 1)?);
                remaining = next;
            }
            "--fuel" if fuel.is_none() => {
                let (value, next) = option_value(option, rest)?;
                fuel = Some(value.parse().map_err(|_| "--fuel に正の回数を指定する。")?);
                remaining = next;
            }
            "--steps" if !steps => {
                steps = true;
                remaining = rest;
            }
            "--project" | "--" => break,
            option if option.starts_with('-') => {
                return Err(format!("eval-meta のオプション {option} を確認する。"));
            }
            _ => break,
        }
    }
    Ok(Command::EvalMeta {
        name,
        arguments,
        fuel,
        steps,
        sources: source_input(remaining)?,
    })
}

fn command(args: &[String]) -> Result<Command<'_>, String> {
    match args {
        [flag] if flag == "--help" || flag == "-h" => Ok(Command::Help),
        [flag] if flag == "--version" => Ok(Command::Version),
        [check, files @ ..] if check == "check" => {
            let (json, files) = match files {
                [option, value, remaining @ ..] if option == "--format" && value == "json" => {
                    (true, remaining)
                }
                [option, remaining @ ..] if option == "--format=json" => (true, remaining),
                [option, ..] if option == "--format" || option.starts_with("--format=") => {
                    return Err("--format では json を指定する。".into());
                }
                _ => (false, files),
            };
            Ok(Command::Check {
                sources: source_input(files)?,
                json,
            })
        }
        [dump, files @ ..] if dump == "dump" => Ok(Command::Dump(source_input(files)?)),
        [count, args @ ..] if count == "nor-count" => nor_count_command(args),
        [format, args @ ..] if format == "fmt" => formatting::command(args).map(Command::Format),
        [eval, args @ ..] if eval == "eval-meta" => eval_meta_command(args),
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
        [rp, first, left, second, right]
            if rp == "rp-plan" && !left.starts_with('-') && !right.starts_with('-') =>
        {
            match (first.as_str(), second.as_str()) {
                ("--project", "--board") => Ok(Command::RpPlan {
                    project: left,
                    board: right,
                }),
                ("--board", "--project") => Ok(Command::RpPlan {
                    project: right,
                    board: left,
                }),
                _ => Err("rp-plan には --project と --board を指定する。".into()),
            }
        }
        [run, args @ ..] if run == "run" => run_command(args),
        [settle, args @ ..] if settle == "settle" => settle_command(args),
        _ => Err("使い方は ncg2 --help で確認できる。".into()),
    }
}

fn source_path(
    path: &str,
    from_config: bool,
    check_ncg2m: bool,
) -> Result<(String, SourceKind), String> {
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
    let kind = match id.source_kind() {
        Some(kind @ SourceKind::Circuit) => kind,
        Some(kind @ SourceKind::Functional) if check_ncg2m => kind,
        Some(SourceKind::Functional) if from_config => {
            return Err(format!(
                "{path} は NCG2M のソース。この CLI は NCG2M の実行に未対応。"
            ));
        }
        _ if from_config || check_ncg2m => {
            return Err(format!(
                "{path} の拡張子には .ncg2 または .ncg2m を指定する。"
            ));
        }
        _ => return Err(format!("{path} の拡張子には .ncg2 を指定する。")),
    };

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
    Ok((normalized, kind))
}

/// 正規化後のパスが同じソースは、最初の指定だけを読み込む。
fn read_source(
    sources: &mut Vec<(String, String, SourceKind)>,
    seen: &mut HashSet<String>,
    base: &Path,
    (path, kind): (String, SourceKind),
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
        sources.push((path, text, kind));
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

fn run(args: &[String], stdout: &mut impl Write) -> Result<ExitCode, String> {
    let command = command(args)?;
    if let Command::Check {
        sources,
        json: true,
    } = command
    {
        let project = match sources {
            SourceInput::Files(_) => None,
            SourceInput::Project(path) => Some(path),
        };
        let mut report = check_report::Report::new(project);
        let result = execute(command, &mut io::sink(), Some(&mut report));
        return report.finish(result, stdout);
    }
    execute(command, stdout, None)
}

fn execute(
    command: Command<'_>,
    stdout: &mut impl Write,
    mut report: Option<&mut check_report::Report<'_>>,
) -> Result<ExitCode, String> {
    let input = match &command {
        Command::Help => {
            return writeln!(stdout, "{HELP}")
                .map_err(output_error)
                .map(|()| ExitCode::SUCCESS);
        }
        Command::Version => {
            return writeln!(stdout, "ncg2 {}", env!("CARGO_PKG_VERSION"))
                .map_err(output_error)
                .map(|()| ExitCode::SUCCESS);
        }
        Command::Format(format) => return formatting::run(format, stdout),
        Command::RpPlan { project, .. } => SourceInput::Project(project),
        Command::Check { sources, .. }
        | Command::Dump(sources)
        | Command::NorCount { sources, .. }
        | Command::Eval { sources, .. }
        | Command::EvalMeta { sources, .. }
        | Command::Run { sources, .. }
        | Command::Settle { sources, .. } => *sources,
    };
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    let check_ncg2m = matches!(command, Command::Check { .. });
    let allow_meta = check_ncg2m || matches!(command, Command::EvalMeta { .. });
    let settings = match input {
        SourceInput::Files(files) => {
            for path in files {
                read_source(
                    &mut sources,
                    &mut seen,
                    Path::new(""),
                    source_path(path, false, allow_meta)?,
                )?;
            }
            None
        }
        SourceInput::Project(path) => {
            let path = Path::new(path);
            let settings = if matches!(
                command,
                Command::Check { .. }
                    | Command::EvalMeta { .. }
                    | Command::Dump(_)
                    | Command::NorCount { .. }
                    | Command::RpPlan { .. }
            ) {
                config::read_settings_located(path)
                    .map_err(|error| {
                        if let Some(report) = &mut report {
                            report.settings_error(error.code, path, error.span.clone());
                        }
                        config::at(path, error.span, &error.reason)
                    })?
                    .0
            } else {
                config::read(path)?
            };
            let base = path.parent().unwrap_or(Path::new(""));
            for value in settings.sources.get_ref() {
                let result = source_path(value.get_ref(), true, allow_meta)
                    .and_then(|source| read_source(&mut sources, &mut seen, base, source));
                result.map_err(|error| {
                    if let Some(report) = &mut report {
                        report.settings_error("settings_source", path, Some(value.span()));
                    }
                    config::at(path, Some(value.span()), &error)
                })?;
            }
            Some((path, settings))
        }
    };
    let snapshots: Vec<_> = sources
        .iter()
        .map(|(path, text, _)| SourceSnapshot {
            path,
            revision: 1,
            text,
        })
        .collect();
    let source_diagnostic = |message: String| match &settings {
        Some((path, _)) => config::source_diagnostic(path, &message),
        None => message,
    };
    if let Command::EvalMeta {
        name,
        arguments,
        fuel,
        steps,
        ..
    } = command
    {
        return check_ncg2m::evaluate(
            &snapshots,
            name,
            arguments,
            fuel,
            steps,
            stdout,
            source_diagnostic,
        );
    }
    if check_ncg2m
        && sources
            .iter()
            .any(|(_, _, kind)| *kind == SourceKind::Functional)
    {
        let files = check_ncg2m::check(&snapshots, &mut report, &source_diagnostic)?;
        if let Some((path, settings)) = &settings
            && let Some(device) = settings.device.first()
        {
            let reason = "外部デバイスを検査するには、回路で開始モジュールを定義する必要がある。";
            if let Some(report) = &mut report {
                report.settings_error("device_requires_circuit", path, Some(device.span()));
            }
            return Err(config::at(path, Some(device.span()), reason));
        }
        return writeln!(stdout, "NCG2M の静的検査成功（{files} ファイル）")
            .map_err(output_error)
            .map(|()| ExitCode::SUCCESS);
    }
    let project = parse_circuit_project(&snapshots).map_err(|error| {
        if let Some(report) = &mut report {
            report.project_error(&error);
        }
        source_diagnostic(diagnostic::project_error(error))
    })?;
    if let Some(report) = &mut report {
        report.files = Some(project.sources().len());
    }
    if matches!(command, Command::Dump(_)) {
        return dump::write(&project, stdout).map(|()| ExitCode::SUCCESS);
    }
    let prepared = prepare_definition_analysis(&project).map_err(|error| {
        if let Some(report) = &mut report {
            report.analysis_error(&error);
        }
        source_diagnostic(diagnostic::analysis_error(error))
    })?;
    let checked = prepared.resolve().map_err(|error| match error {
        PreparedDefinitionError::Alias(conflict) => {
            let error = DefinitionAnalysisError::Alias(conflict);
            if let Some(report) = &mut report {
                report.analysis_error(&error);
            }
            source_diagnostic(diagnostic::analysis_error(error))
        }
        PreparedDefinitionError::Definition { error, aliases } => {
            alias_diagnostics(&aliases, &mut report, &source_diagnostic);
            if let Some(report) = &mut report {
                report.definition_error(&error);
            }
            source_diagnostic(diagnostic::reference_error(error))
        }
    })?;
    let aliases = checked.aliases();
    alias_diagnostics(aliases, &mut report, &source_diagnostic);

    if matches!(command, Command::Check { .. }) {
        check_body::check(&project, &checked, &mut report).map_err(&source_diagnostic)?;
        if let Some((path, settings)) = &settings
            && !settings.device.is_empty()
        {
            let name = settings
                .entry
                .as_ref()
                .map_or("::main", |name| name.get_ref());
            let (file, entry) =
                declaration_position(&project, prepared.targets(), name, Head::Module).map_err(
                    |error| {
                        config::at(
                            path,
                            settings.entry.as_ref().map(|entry| entry.span()),
                            &error,
                        )
                    },
                )?;
            device_config::check(path, settings, aliases, ModuleBodyPosition { file, entry })?;
        }
    }

    if let (Command::RpPlan { board, .. }, Some((path, settings))) = (&command, &settings) {
        let name = settings
            .entry
            .as_ref()
            .map_or("::main", |entry| entry.get_ref());
        let (file, entry) = declaration_position(&project, prepared.targets(), name, Head::Module)
            .map_err(|error| {
                config::at(
                    path,
                    settings.entry.as_ref().map(|entry| entry.span()),
                    &error,
                )
            })?;
        return rp_plan::export(
            rp_plan::Source {
                settings_path: path,
                settings,
                project: &project,
                owner: ModuleBodyPosition { file, entry },
                name,
            },
            Path::new(board),
            aliases,
            stdout,
        )
        .map(|()| ExitCode::SUCCESS);
    }

    if let Command::NorCount { entry, .. } = command {
        return nor_count::write(&project, aliases, entry, &source_diagnostic, stdout)
            .map(|()| ExitCode::SUCCESS);
    }

    let (name, kind) = match &command {
        Command::Eval { name, .. } => (*name, Head::Func),
        Command::Run { entry, .. } | Command::Settle { entry, .. } => {
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
                .map_err(output_error)
                .map(|()| ExitCode::SUCCESS);
        }
    };
    let (file, entry) =
        declaration_position(&project, prepared.targets(), name, kind).map_err(|error| {
            match (&command, &settings) {
                (Command::Run { .. } | Command::Settle { .. }, Some((path, settings))) => {
                    config::at(
                        path,
                        settings.entry.as_ref().map(|entry| entry.span()),
                        &error,
                    )
                }
                _ => error,
            }
        })?;
    if let Command::Run { frames, vram, .. } = command {
        return clocked::trace(
            &project,
            aliases,
            ModuleBodyPosition { file, entry },
            clocked::TraceOptions {
                name,
                frames,
                vram: &vram,
                settings: settings.as_ref().map(|(path, settings)| (*path, settings)),
            },
            stdout,
        )
        .map(|()| ExitCode::SUCCESS);
    }
    if let Command::Settle { condition, .. } = command {
        return settling::analyze(
            &project,
            aliases,
            ModuleBodyPosition { file, entry },
            name,
            condition,
            &source_diagnostic,
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
    writeln!(stdout, "{}", format_bits(output))
        .map_err(output_error)
        .map(|()| ExitCode::SUCCESS)
}

fn alias_diagnostics(
    aliases: &UseAliases<'_, '_, '_>,
    report: &mut Option<&mut check_report::Report<'_>>,
    contextualize: impl Fn(String) -> String,
) {
    if let Some(report) = report {
        report.warnings(aliases.directives(), aliases.dispositions());
    } else {
        diagnostic::alias_warnings(aliases.directives(), aliases.dispositions(), contextualize);
    }
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
        let status = run(&args, &mut stdout)?;
        stdout.flush().map_err(output_error)?;
        Ok(status)
    }) {
        Ok(status) => status,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");
            ExitCode::FAILURE
        }
    }
}
