//! I²C のボード設定を回路ポートと照合し、ファームウェア用の配線表に変換する。

use super::{
    PeripheralLocations, Source, Wiring, config, data_error, image_error, write_peripheral_layout,
};
use circuitgame2_core::backend::rp::{
    I2cCircuitImage, I2cImageError, I2cPortNames, I2cSetup, Signal,
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

    fn locations<'a>(
        &'a self,
        pins: &'a [(Signal, &'static str, Range<usize>)],
    ) -> PeripheralLocations<'a> {
        let mut to = vec![&self.request_ready, &self.response_valid];
        to.extend(&self.response_status);
        to.extend(self.read_data.get_ref());
        let mut from = vec![&self.request_valid, &self.response_ready];
        from.extend(&self.address);
        from.extend(&self.write_length);
        from.extend(&self.read_length);
        from.extend(self.write_data.get_ref());
        PeripheralLocations {
            name: "I²C",
            instance: self.instance.span(),
            pins,
            to,
            from,
        }
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
    let pins = [
        (Signal::I2cData, "SDA", i2c.sda_pin.span()),
        (Signal::I2cClock, "SCL", i2c.scl_pin.span()),
    ];
    let locations = i2c.locations(&pins);
    let (to, from) = locations.names();
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
            I2cImageError::PortCounts => data_error(board, &i2c.read_data, &i2c.write_data),
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
    write_peripheral_layout(
        out,
        "i2c",
        inputs,
        gpio_outputs,
        i2c_outputs,
        image.layout(),
    )
}
