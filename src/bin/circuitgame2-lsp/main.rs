//! エディタとの標準入出力を接続し、開いている回路ソースを診断する。

use std::process::ExitCode;

#[cfg(not(target_os = "wasi"))]
mod definition;
#[cfg(not(target_os = "wasi"))]
mod diagnostic;
#[cfg(not(target_os = "wasi"))]
mod server;
#[cfg(not(target_os = "wasi"))]
mod sources;

#[cfg(not(target_os = "wasi"))]
fn main() -> ExitCode {
    match server::run(&mut std::io::stdin().lock(), &mut std::io::stdout().lock()) {
        Ok(exit) => exit,
        Err(error) => {
            eprintln!("LSP の通信を続けられない。{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "wasi")]
fn main() -> ExitCode {
    eprintln!("circuitgame2-lsp はネイティブ版を使用する。");
    ExitCode::FAILURE
}
