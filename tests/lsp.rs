#![cfg(not(target_os = "wasi"))]

use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use lsp_server::{Message, Notification, Request};
use serde_json::{Value, json};

#[test]
fn stdio_uses_utf16_positions_from_the_latest_document() {
    let uri = "untitled:editing.ncg2";
    let source = "(using nor)\n(func invert (in x) (out y) (def y (nor x x)))\n\
                  (func other (in x) (out y) (def y (missing x)))\n\
                  (func caller (in x) (out y) (def y (invert x)))\n";
    let messages = [
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "languageId": "ncg2", "version": 1, "text": "\"漢😀\r\n"
            }}),
        ),
        notification(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri, "version": 3}, "contentChanges": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 4}},
                "text": source
            }]}),
        ),
        notification(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": ")"}]}),
        ),
        request(
            2,
            "textDocument/definition",
            json!({"textDocument": {"uri": uri}, "position": {"line": 3, "character": 36}}),
        ),
        request(3, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ];
    let mut child = Command::new(env!("CARGO_BIN_EXE_circuitgame2-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("LSP を起動できる");
    let mut input = child.stdin.take().unwrap();
    for message in messages {
        message.write(&mut input).unwrap();
    }
    // 標準入力を開いたまま待機し、`exit` 通知を受けたプロセスが終了することを確かめる。
    let deadline = Instant::now() + Duration::from_secs(5);
    let exited = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(exited && output.status.success(), "{:?}", output);
    let mut stdout = output.stdout.as_slice();
    let responses: Vec<Value> = std::iter::from_fn(|| Message::read(&mut stdout).unwrap())
        .map(|message| serde_json::to_value(message).unwrap())
        .collect();
    assert_eq!(responses.len(), 5, "{responses:?}");
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(
        responses[0]["result"]["capabilities"]["positionEncoding"],
        "utf-16"
    );
    assert_eq!(responses[1]["method"], "textDocument/publishDiagnostics");
    assert_eq!(responses[1]["params"]["version"], 1);
    let diagnostic = &responses[1]["params"]["diagnostics"][0];
    assert_eq!(diagnostic["code"], "NewlineInString");
    assert_eq!(
        diagnostic["range"],
        json!({
            "start": {"line": 0, "character": 4}, "end": {"line": 1, "character": 0}
        })
    );
    assert_eq!(responses[2]["params"]["version"], 3);
    assert_eq!(responses[2]["params"]["diagnostics"], json!([]));
    assert_eq!(responses[3]["id"], 2);
    assert_eq!(
        responses[3]["result"],
        json!({"uri": uri, "range": {
            "start": {"line": 1, "character": 6}, "end": {"line": 1, "character": 12}
        }})
    );
    assert_eq!(responses[4]["id"], 3);
    assert!(responses[4].get("error").is_none(), "{:?}", responses[4]);
}

fn request(id: i32, method: &str, params: Value) -> Message {
    Request {
        id: id.into(),
        method: method.into(),
        params,
    }
    .into()
}

fn notification(method: &str, params: Value) -> Message {
    Notification {
        method: method.into(),
        params,
    }
    .into()
}
