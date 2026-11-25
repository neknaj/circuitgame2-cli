//! SPI のボード設定を回路ポートと照合し、ファームウェア用の配線表に変換する。

use super::{
    PeripheralLocations, Source, Wiring, config, data_error, image_error, write_peripheral_layout,
};
use circuitgame2_core::backend::rp::{
    GpioBinding, Signal, SpiBitOrder, SpiCircuitImage, SpiImageError, SpiPortNames,
    SpiSettingError, SpiSetup, spi_frequency_range,
};
use circuitgame2_core::project::UseAliases;
use serde::Deserialize;
use std::{io::Write, path::Path};
use toml::Spanned;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum BitOrder {
    MsbFirst,
    LsbFirst,
}

impl BitOrder {
    fn native(self) -> SpiBitOrder {
        match self {
            Self::MsbFirst => SpiBitOrder::MostSignificantFirst,
            Self::LsbFirst => SpiBitOrder::LeastSignificantFirst,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct Spi {
    instance: Spanned<u8>,
    sck_pin: Spanned<u8>,
    mosi_pin: Spanned<u8>,
    miso_pin: Spanned<u8>,
    cs_pin: Spanned<u8>,
    frequency_hz: Spanned<u32>,
    mode: Spanned<u8>,
    bit_order: BitOrder,
    timeout_micros: Spanned<u32>,
    request_ready: Spanned<String>,
    response_valid: Spanned<String>,
    response_status: [Spanned<String>; 3],
    read_data: Spanned<Vec<Spanned<String>>>,
    request_valid: Spanned<String>,
    response_ready: Spanned<String>,
    length: [Spanned<String>; 8],
    write_data: Spanned<Vec<Spanned<String>>>,
}

impl Spi {
    fn setup(&self) -> SpiSetup {
        SpiSetup {
            instance: *self.instance.get_ref(),
            sck: *self.sck_pin.get_ref(),
            mosi: *self.mosi_pin.get_ref(),
            miso: *self.miso_pin.get_ref(),
            cs: *self.cs_pin.get_ref(),
            frequency_hz: *self.frequency_hz.get_ref(),
            mode: *self.mode.get_ref(),
            bit_order: self.bit_order.native(),
            timeout_micros: *self.timeout_micros.get_ref(),
        }
    }
}

pub(super) fn export(
    out: &mut impl Write,
    source: &Source<'_, '_>,
    board: &Path,
    wiring: &Wiring,
    settings: &Spanned<Spi>,
    aliases: &UseAliases<'_, '_, '_>,
    gpio: &[GpioBinding<'_>],
    period: u64,
) -> Result<(), String> {
    let spi = settings.get_ref();
    let pins = [
        (Signal::SpiClock, "SCK", spi.sck_pin.span()),
        (Signal::SpiMosi, "MOSI", spi.mosi_pin.span()),
        (Signal::SpiMiso, "MISO", spi.miso_pin.span()),
        (Signal::SpiSelect, "CS", spi.cs_pin.span()),
    ];
    let mut to = vec![&spi.request_ready, &spi.response_valid];
    to.extend(&spi.response_status);
    to.extend(spi.read_data.get_ref());
    let mut from = vec![&spi.request_valid, &spi.response_ready];
    from.extend(&spi.length);
    from.extend(spi.write_data.get_ref());
    let locations = PeripheralLocations {
        name: "SPI",
        instance: spi.instance.span(),
        pins: &pins,
        to,
        from,
    };
    let (to, from) = locations.names();
    let image = aliases
        .build_rp_spi_image(
            source.owner,
            wiring.board.native(),
            gpio,
            spi.setup(),
            SpiPortNames {
                to_circuit: &to,
                from_circuit: &from,
            },
            period,
            crate::WIDTH_LIMIT,
            crate::BIT_LIMIT,
            crate::GATE_LIMIT,
        )
        .map_err(|error| match error {
            SpiImageError::Setting(SpiSettingError::Mode) => config::at(
                board,
                Some(spi.mode.span()),
                &format!("SPI のモードは 0 から 3。指定値は {}。", spi.mode.get_ref()),
            ),
            SpiImageError::Setting(SpiSettingError::Frequency) => {
                let (min, max) = spi_frequency_range(wiring.board.native());
                config::at(
                    board,
                    Some(spi.frequency_hz.span()),
                    &format!(
                        "{} では SPI の周波数に {min}〜{max} Hz を指定する。指定値は {} Hz。",
                        wiring.board.wire_name(),
                        spi.frequency_hz.get_ref(),
                    ),
                )
            }
            SpiImageError::Setting(SpiSettingError::ZeroTimeout) => config::at(
                board,
                Some(spi.timeout_micros.span()),
                "SPI の待ち時間には正のマイクロ秒数を指定する。",
            ),
            SpiImageError::Setting(SpiSettingError::PortCounts) => {
                data_error(board, &spi.read_data, &spi.write_data)
            }
            SpiImageError::Setting(SpiSettingError::BindingCount) => config::at(
                board,
                Some(settings.span()),
                "SPI と GPIO の接続数がホストで扱える上限を超えている。",
            ),
            SpiImageError::Circuit(error) => {
                image_error(error, board, wiring, source, Some(&locations))
            }
        })?;
    write_plan(out, wiring.board.wire_name(), &image).map_err(crate::output_error)
}

fn write_plan(out: &mut impl Write, board: &str, image: &SpiCircuitImage) -> std::io::Result<()> {
    let (_, setup, period, capacity) = image.setup();
    let bit_order = match setup.bit_order {
        SpiBitOrder::MostSignificantFirst => "msb-first",
        SpiBitOrder::LeastSignificantFirst => "lsb-first",
    };
    writeln!(out, "ncg2-rp-spi-v1")?;
    writeln!(out, "board {board}")?;
    writeln!(out, "period-us {period}")?;
    writeln!(
        out,
        "spi {} {} {} {} {} {} {} {bit_order} {} {capacity}",
        setup.instance,
        setup.sck,
        setup.mosi,
        setup.miso,
        setup.cs,
        setup.frequency_hz,
        setup.mode,
        setup.timeout_micros,
    )?;
    let (inputs, gpio_outputs, spi_outputs) = image.ports();
    write_peripheral_layout(
        out,
        "spi",
        inputs,
        gpio_outputs,
        spi_outputs,
        image.layout(),
    )
}
