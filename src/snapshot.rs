//! 指定した tick の確定状態を、ネイティブ版でファイルに保存・復元する。

use std::path::Path;

#[cfg(any(unix, windows))]
use std::{
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    path::PathBuf,
};

#[cfg(any(unix, windows))]
use circuitgame2_core::{
    netlist::{
        CheckpointFileError, CheckpointHeader, ClockedCheckpointError, VmCheckpointFileError,
        checkpoint_file_len, decode_vm_checkpoint, encode_vm_checkpoint,
    },
    project::{CircuitProject, ClockedModuleVm, ClockedVramVm},
};

#[cfg(any(unix, windows))]
use super::{
    clocked::{TraceOptions, write_frame},
    config::TickPeriod,
    input_bits, validate_input,
};

#[derive(Default)]
pub(super) struct Options<'a> {
    pub(super) save: Option<Save<'a>>,
    pub(super) restore: Option<&'a Path>,
}

pub(super) struct Save<'a> {
    pub(super) tick: u64,
    pub(super) path: &'a Path,
}

impl Options<'_> {
    #[cfg(any(unix, windows))]
    pub(super) fn requested(&self) -> bool {
        self.save.is_some() || self.restore.is_some()
    }
}

/// スナップショットを扱える実行環境かどうかを確認する。
///
/// # Errors
///
/// WASI 版では保存・復元が利用できないことを示すエラーを返す。
pub(super) fn require_native() -> Result<(), String> {
    if cfg!(any(unix, windows)) {
        Ok(())
    } else {
        Err("この実行環境の run はスナップショットの保存・復元に対応していない。".into())
    }
}

#[cfg(any(unix, windows))]
fn source_directory(settings: Option<&Path>) -> &Path {
    settings
        .and_then(Path::parent)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(any(unix, windows))]
fn check_regular_destination(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.permissions().readonly() => Ok(()),
        Ok(_) => Err(format!(
            "{} は書き込み可能な通常ファイルではない。",
            path.display(),
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "{} の保存先を確認できない ({error})。",
            path.display()
        )),
    }
}

#[cfg(any(unix, windows))]
fn save_destination(
    project: &CircuitProject<'_>,
    settings: Option<&Path>,
    path: &Path,
) -> Result<PathBuf, String> {
    let ends_at_file = path
        .as_os_str()
        .as_encoded_bytes()
        .rsplit(|byte| *byte == b'/' || cfg!(windows) && *byte == b'\\')
        .next()
        .is_some_and(|part| !matches!(part, b"" | b"." | b".."));
    let file = path
        .file_name()
        .filter(|_| ends_at_file)
        .ok_or_else(|| format!("{} に保存ファイル名を指定する。", path.display()))?;
    let directory = source_directory(Some(path))
        .canonicalize()
        .map_err(|error| {
            format!(
                "{} の保存先ディレクトリを開けない ({error})。",
                path.display()
            )
        })?;
    let destination = directory.join(file);
    check_regular_destination(&destination)?;

    // 同じファイルを相対パスや別名で指定しても、実行に使うソースや設定の上書きを防ぐ。
    let existing = match destination.canonicalize() {
        Ok(path) => Some(path),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "{} の保存先を確認できない ({error})。",
                path.display()
            ));
        }
    };
    if let Some(existing) = existing {
        let settings_path = settings.and_then(|path| path.canonicalize().ok());
        let source_root = source_directory(settings);
        let is_source = project.sources().iter().any(|source| {
            let path = source
                .id
                .components()
                .iter()
                .fold(source_root.to_path_buf(), |path, part| path.join(part));
            path.canonicalize().is_ok_and(|path| path == existing)
        });
        if settings_path.as_ref() == Some(&existing) || is_source {
            return Err(format!(
                "{} は実行に使うソースまたは設定ファイル。別の保存先を指定する。",
                path.display()
            ));
        }
    }
    Ok(destination)
}

// メタデータの長さが上限以内でも、読み込み中にファイルが大きくなった場合は拒否する。
#[cfg(any(unix, windows))]
fn read_bytes(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let read_error = |error| {
        format!(
            "{} のスナップショットを読み込めない ({error})。",
            path.display()
        )
    };
    if !fs::metadata(path).map_err(read_error)?.is_file() {
        return Err(format!("{} は通常ファイルではない。", path.display()));
    }
    let mut file = File::open(path).map_err(read_error)?;
    let metadata = file.metadata().map_err(read_error)?;
    if !metadata.is_file() {
        return Err(format!("{} は通常ファイルではない。", path.display()));
    }
    let length = usize::try_from(metadata.len())
        .ok()
        .filter(|length| *length <= limit)
        .ok_or_else(|| {
            format!(
                "{} のスナップショットが上限 {limit} バイトを超えている。",
                path.display()
            )
        })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| format!("{} を読み込む領域を確保できない。", path.display()))?;
    bytes.resize(length, 0);
    file.read_exact(&mut bytes).map_err(read_error)?;
    if file.read(&mut [0]).map_err(read_error)? != 0 {
        return Err(format!(
            "{} の大きさが読み込み中に変わった。",
            path.display()
        ));
    }
    Ok(bytes)
}

#[cfg(any(unix, windows))]
fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    check_regular_destination(path)?;
    let write_error = |error| {
        format!(
            "{} にスナップショットを保存できない ({error})。",
            path.display()
        )
    };
    let mut temporary =
        tempfile::NamedTempFile::new_in(source_directory(Some(path))).map_err(write_error)?;
    temporary.write_all(bytes).map_err(write_error)?;
    temporary.as_file().sync_all().map_err(write_error)?;
    temporary
        .persist(path)
        .map_err(|error| write_error(error.error))?;
    Ok(())
}

#[cfg(any(unix, windows))]
fn validate_tick_range(start: u64, frames: u64, save: Option<&Save<'_>>) -> Result<u64, String> {
    let end = start
        .checked_add(frames)
        .ok_or("入力列を実行すると tick 番号が64ビットの範囲を超える。")?;
    if let Some(save) = save {
        if save.tick < start {
            return Err(format!(
                "保存を指定した tick {} は実行開始の tick {start} より前。",
                save.tick
            ));
        }
        if save.tick > end {
            return Err(format!(
                "保存を指定した tick {} に達する前に tick {end} で実行が終わる。",
                save.tick
            ));
        }
    }
    Ok(end)
}

#[cfg(any(unix, windows))]
fn period_values(period: Option<&TickPeriod>) -> (u64, u64) {
    period.map_or((0, 0), |period| {
        (period.numerator.get(), period.denominator.get())
    })
}

#[cfg(any(unix, windows))]
fn checkpoint_error(path: &Path, error: VmCheckpointFileError) -> String {
    let reason = match error {
        VmCheckpointFileError::File(CheckpointFileError::ShortHeader) => {
            "ヘッダーの途中でデータが終わっている。"
        }
        VmCheckpointFileError::File(CheckpointFileError::Format) => {
            "形式の識別子またはバージョンが合わない。"
        }
        VmCheckpointFileError::File(CheckpointFileError::Period) => {
            "保存された tick 周期の分子または分母が規定外。"
        }
        VmCheckpointFileError::File(CheckpointFileError::Length) => {
            "ファイルの長さが復元先の回路に必要な長さと異なる。"
        }
        VmCheckpointFileError::File(CheckpointFileError::Layout) => {
            "端子または配線、状態素子の配置が復元先の回路と異なる。"
        }
        VmCheckpointFileError::PeriodMismatch => "保存された tick 周期が今回の実行設定と異なる。",
        VmCheckpointFileError::PortLayout => "端子配置または外部入力の幅が回路と異なる。",
        VmCheckpointFileError::MalformedState => "状態のビット列が保存形式に適合しない。",
        VmCheckpointFileError::File(CheckpointFileError::Allocation)
        | VmCheckpointFileError::Allocation => "必要な保存データの領域を確保できない。",
    };
    format!("{} のスナップショット。{reason}", path.display())
}

#[cfg(any(unix, windows))]
struct Boundary {
    tick: u64,
    generation: u64,
    input: Vec<bool>,
    output: Vec<bool>,
}

#[cfg(any(unix, windows))]
fn restore(
    module: &mut ClockedModuleVm,
    path: &Path,
    period: Option<&TickPeriod>,
) -> Result<Boundary, String> {
    let inputs = module.inputs.ranges();
    let outputs = module.outputs.ranges();
    let length = checkpoint_file_len(&module.vm, inputs, outputs)
        .map_err(|error| checkpoint_error(path, error))?;
    let file = read_bytes(path, length)?;
    let saved = decode_vm_checkpoint(&file, &module.vm, inputs, outputs, period_values(period))
        .map_err(|error| checkpoint_error(path, error))?;
    module.vm.restore_checkpoint(&saved.vm).map_err(|error| {
        let reason = match error {
            ClockedCheckpointError::Layout => "配線または状態素子の並びが復元先の回路と異なる。",
            ClockedCheckpointError::StateShape => {
                "ゲート、状態素子、RAM のビット数が復元先の回路と異なる。"
            }
        };
        format!(
            "{} のスナップショットを復元できない。{reason}",
            path.display()
        )
    })?;
    let output = module.vm.observe_current(&saved.input).map_err(|error| {
        format!(
            "{} の状態から出力を求められない ({error:?})。",
            path.display()
        )
    })?;
    Ok(Boundary {
        tick: saved.header.tick,
        generation: saved.header.generation,
        input: saved.input,
        output,
    })
}

#[cfg(any(unix, windows))]
fn capture(
    module: &ClockedModuleVm,
    path: &Path,
    period: Option<&TickPeriod>,
    tick: u64,
    generation: u64,
    input: &[bool],
) -> Result<(), String> {
    let (period_numerator, period_denominator) = period_values(period);
    let header = CheckpointHeader {
        tick,
        generation,
        period_numerator,
        period_denominator,
    };
    let bytes = encode_vm_checkpoint(
        &module.vm,
        module.inputs.ranges(),
        module.outputs.ranges(),
        header,
        input,
    )
    .map_err(|error| checkpoint_error(path, error))?;
    write_bytes(path, &bytes)
}

/// スナップショットの復元と指定した tick での保存を含めて実行する。
///
/// 復元直後の出力には、保存した回路の状態と最後に適用した入力を使う。
#[cfg(any(unix, windows))]
pub(super) fn trace(
    project: &CircuitProject<'_>,
    compiled: ClockedVramVm<'_, '_, '_>,
    options: TraceOptions<'_>,
    stdout: &mut impl Write,
) -> Result<(), String> {
    let TraceOptions {
        name,
        frames,
        vram,
        snapshot,
        settings,
        ..
    } = options;
    if let Some((path, settings)) = settings
        && let Some(device) = settings.device.first()
    {
        return Err(super::config::at(
            path,
            Some(device.span()),
            "外部デバイスを使う実行ではスナップショットの保存・復元に対応していない。",
        ));
    }
    let indices = vram
        .iter()
        .map(|name| {
            compiled
                .vram_storage_index(name)
                .ok_or_else(|| format!("公開 VRAM {name:?} が設定に見つからない。"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut module = compiled.into_module();
    let (explicit, empty_ticks) = frames.for_inputs(&module.inputs, name)?;
    let width = module.inputs.total_bits();
    let period =
        settings.and_then(|(_, settings)| settings.tick_ns.as_ref().map(|period| period.get_ref()));
    let save_path = snapshot
        .save
        .as_ref()
        .map(|save| save_destination(project, settings.map(|(path, _)| path), save.path))
        .transpose()?;

    let Boundary {
        tick: start,
        generation,
        input: initial_input,
        output: initial_output,
    } = match snapshot.restore {
        Some(path) => restore(&mut module, path, period)?,
        None => Boundary {
            tick: 0,
            generation: 0,
            input: vec![false; width],
            output: module.vm.reset(),
        },
    };
    let remaining = u64::try_from(explicit.len())
        .ok()
        .and_then(|count| count.checked_add(empty_ticks))
        .ok_or("入力列の長さが64ビットの範囲を超えている。")?;
    let end = validate_tick_range(start, remaining, snapshot.save.as_ref())?;
    for (tick, input) in (start..end).zip(explicit) {
        validate_input(input, width)
            .map_err(|reason| format!("tick {} の入力。{reason}", tick + 1))?;
    }

    let save = snapshot.save.as_ref().zip(save_path.as_deref());
    if let Some((request, path)) = save.filter(|(request, _)| request.tick == start) {
        capture(
            &module,
            path,
            period,
            request.tick,
            generation,
            &initial_input,
        )?;
    }
    write_frame(stdout, vram, start, initial_output, |index, _| {
        module.vm.ram_memory(indices[index])
    })?;
    let frames = explicit.iter().copied().chain((0..empty_ticks).map(|_| ""));
    for (previous, frame) in (start..end).zip(frames) {
        let tick = previous + 1;
        let input = input_bits(frame);
        let output = module
            .vm
            .step(&input)
            .map_err(|error| format!("{name} の tick {tick} を実行できない ({error:?})。"))?;
        if let Some((request, path)) = save.filter(|(request, _)| request.tick == tick) {
            capture(&module, path, period, request.tick, generation, &input)?;
        }
        write_frame(stdout, vram, tick, output, |index, _| {
            module.vm.ram_memory(indices[index])
        })?;
    }
    Ok(())
}
