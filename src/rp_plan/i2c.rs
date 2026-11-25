//! I²C のボード設定を回路ポートと照合し、ファームウェア用の配線表に変換する。

use super::{Source, Wiring, config, image_error, write_layout};
use circuitgame2_core::backend::rp::{
    AssignmentError, I2cCircuitImage, I2cImageError, I2cInputSource, I2cPortNames, I2cSetup, Signal,
};
use circuitgame2_core::project::UseAliases;
use serde::Deserialize;
use std::{io::Write, ops::Range, path::Path};
use toml::Spanned;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct I2c {
    instance: Spanned<u8>,
    sda_pin: Spanned<u8>,
    scl_pin: Spanned<u8>,
    frequency_hz: Spanned<u32>,
    timeout_micros: Spanned<u32>,
    request_ready: Spanned<String>,
    response_valid: Spanned<String>,
    response_status: [Spanned<String>; 3],
    read_data: Spanned<Vec<Spanned<String>>>,
    request_valid: Spanned<String>,
    response_ready: Spanned<String>,
    address: [Spanned<String>; 7],
    write_length: [Spanned<String>; 8],
    read_length: [Spanned<String>; 8],
    write_data: Spanned<Vec<Spanned<String>>>,
}

impl I2c {
    fn setup(&self) -> I2cSetup {
        I2cSetup {
            instance: *self.instance.get_ref(),
            sda: *self.sda_pin.get_ref(),
            scl: *self.scl_pin.get_ref(),
            frequency_hz: *self.frequency_hz.get_ref(),
            timeout_micros: *self.timeout_micros.get_ref(),
        }
    }

    /// Core が要求する信号順と、設定の各ポート名の位置を同じ列に保持する。
    fn locations(&self) -> Locations<'_> {
        let mut to = vec![&self.request_ready, &self.response_valid];
        to.extend(&self.response_status);
        to.extend(self.read_data.get_ref());
        let mut from = vec![&self.request_valid, &self.response_ready];
        from.extend(&self.address);
        from.extend(&self.write_length);
        from.extend(&self.read_length);
        from.extend(self.write_data.get_ref());
        Locations {
            config: self,
            to,
            from,
        }
    }

    fn data_error(&self, board: &Path) -> String {
        let read = self.read_data.get_ref().len();
        let write = self.write_data.get_ref().len();
        let valid = |bits| (8..=255 * 8).contains(&bits) && bits % 8 == 0;
        if !valid(read) || !valid(write) {
            let (field, span, bits) = if !valid(read) {
                ("read-data", self.read_data.span(), read)
            } else {
                ("write-data", self.write_data.span(), write)
            };
            return config::at(
                board,
                Some(span),
                &format!("`{field}` のポート名は {bits} 個。1〜255 バイト分を8個ずつ指定する。"),
            );
        }
        format!(
            "{}\n{}",
            config::at(
                board,
                Some(self.write_data.span()),
                &format!(
                    "`write-data` の容量は {} バイト。`read-data` と同じ容量を指定する。",
                    write / 8
                )
            ),
            config::at(
                board,
                Some(self.read_data.span()),
                &format!("`read-data` の容量は {} バイト。", read / 8)
            ),
        )
    }
}

pub(super) struct Locations<'a> {
    config: &'a I2c,
    to: Vec<&'a Spanned<String>>,
    from: Vec<&'a Spanned<String>>,
}

impl Locations<'_> {
    pub(super) fn pin(&self, signal: Signal) -> Option<Range<usize>> {
        match signal {
            Signal::I2cData => Some(self.config.sda_pin.span()),
            Signal::I2cClock => Some(self.config.scl_pin.span()),
            _ => None,
        }
    }

    pub(super) fn request(&self, index: usize) -> Option<Range<usize>> {
        let row = if index < self.to.len() {
            self.to.get(index)
        } else {
            self.from.get(index - self.to.len())
        };
        row.map(|name| name.span())
    }
}

pub(super) fn assignment_error(
    error: AssignmentError,
    board: &Path,
    locations: &Locations<'_>,
) -> String {
    match error {
        AssignmentError::UnknownInstance { instance, .. } => config::at(
            board,
            Some(locations.config.instance.span()),
            &format!("I²C コントローラー番号は 0 または 1。指定値は {instance}。"),
        ),
        AssignmentError::UnsupportedSignal {
            site,
            pin,
            instance,
        } => {
            let signal = if matches!(site.signal, Signal::I2cData) {
                "SDA"
            } else {
                "SCL"
            };
            config::at(
                board,
                locations.pin(site.signal),
                &format!("GP{pin} は I²C{instance} の {signal} に割り当てられない。"),
            )
        }
        // GPIO とコントローラー一つを検査する入口。通常のピン違反は共通の表示で処理する。
        _ => config::at(
            board,
            None,
            "I²C のピンまたはコントローラーの割り当てに違反がある。",
        ),
    }
}

pub(super) fn export(
    out: &mut impl Write,
    source: &Source<'_, '_>,
    board: &Path,
    wiring: &Wiring,
    settings: &Spanned<I2c>,
    aliases: &UseAliases<'_, '_, '_>,
    gpio: &[super::GpioBinding<'_>],
    period: u64,
) -> Result<(), String> {
    let i2c = settings.get_ref();
    let locations = i2c.locations();
    let to: Vec<_> = locations
        .to
        .iter()
        .map(|name| name.get_ref().as_str())
        .collect();
    let from: Vec<_> = locations
        .from
        .iter()
        .map(|name| name.get_ref().as_str())
        .collect();
    let image = aliases
        .build_rp_i2c_image(
            source.owner,
            wiring.board.native(),
            gpio,
            i2c.setup(),
            I2cPortNames {
                to_circuit: &to,
                from_circuit: &from,
            },
            period,
            crate::WIDTH_LIMIT,
            crate::BIT_LIMIT,
            crate::GATE_LIMIT,
        )
        .map_err(|error| match error {
            I2cImageError::Frequency => config::at(
                board,
                Some(i2c.frequency_hz.span()),
                &format!(
                    "I²C の周波数は 100000 Hz または 400000 Hz。指定値は {} Hz。",
                    i2c.frequency_hz.get_ref()
                ),
            ),
            I2cImageError::ZeroTimeout => config::at(
                board,
                Some(i2c.timeout_micros.span()),
                "I²C の待ち時間には正のマイクロ秒数を指定する。",
            ),
            I2cImageError::PortCounts => i2c.data_error(board),
            I2cImageError::BindingCount => config::at(
                board,
                Some(settings.span()),
                "I²C と GPIO の接続数をホストで扱える上限を超えている。",
            ),
            I2cImageError::Circuit(error) => {
                image_error(error, board, wiring, source, Some(&locations))
            }
        })?;
    write_plan(out, wiring.board.wire_name(), &image).map_err(crate::output_error)
}

fn write_plan(out: &mut impl Write, board: &str, image: &I2cCircuitImage) -> std::io::Result<()> {
    let (_, setup, period, capacity) = image.setup();
    writeln!(out, "ncg2-rp-i2c-v1")?;
    writeln!(out, "board {board}")?;
    writeln!(out, "period-us {period}")?;
    writeln!(
        out,
        "i2c {} {} {} {} {} {capacity}",
        setup.instance, setup.sda, setup.scl, setup.frequency_hz, setup.timeout_micros
    )?;
    let (inputs, gpio_outputs, i2c_outputs) = image.ports();
    write!(out, "i2c-outputs")?;
    for port in i2c_outputs {
        write!(out, " {port}")?;
    }
    writeln!(out)?;
    for source in inputs {
        match source {
            I2cInputSource::Gpio(pin) => writeln!(out, "input {pin}")?,
            I2cInputSource::Controller(index) => writeln!(out, "input-i2c {index}")?,
        }
    }
    write_layout(out, image.layout(), gpio_outputs)
}
