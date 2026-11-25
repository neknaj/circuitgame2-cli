#![cfg(not(target_os = "wasi"))]

use std::{
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use lsp_server::{Message, Notification, Request};
use serde_json::{Value, json};
use url::Url;

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
    let responses = exchange(messages);
    assert_eq!(responses.len(), 6, "{responses:?}");
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
    assert_eq!(responses[3]["params"]["version"], 3);
    let references = responses[3]["params"]["diagnostics"].as_array().unwrap();
    assert_eq!(references.len(), 1, "{references:?}");
    assert_eq!(references[0]["code"], "MissingReference");
    assert_eq!(
        references[0]["range"],
        json!({
            "start": {"line": 2, "character": 35}, "end": {"line": 2, "character": 42}
        })
    );
    assert_eq!(responses[4]["id"], 2);
    assert_eq!(
        responses[4]["result"],
        json!({"uri": uri, "range": {
            "start": {"line": 1, "character": 6}, "end": {"line": 1, "character": 12}
        }})
    );
    assert_eq!(responses[5]["id"], 3);
    assert!(responses[5].get("error").is_none(), "{:?}", responses[5]);
}

#[test]
fn name_diagnostics_follow_the_configured_dependency_snapshot() {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/project");
    let basis = Url::from_file_path(project.join("basis.ncg2")).unwrap();
    let main = Url::from_file_path(project.join("main.ncg2")).unwrap();
    let declaration = "(namespace lib (func hidden (in x) (out y) (def y (nor x x))))";
    let basis_source = format!("(using nor)\n; ✨\n{declaration}");
    let main_source = "(func entry (in x) (out y) (def y (::lib::hidden x)) (def z (missing x)))";
    let responses = exchange([
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": basis.as_str(), "languageId": "ncg2", "version": 1, "text": basis_source
            }}),
        ),
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": main.as_str(), "languageId": "ncg2", "version": 1, "text": main_source
            }}),
        ),
        notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": basis.as_str(), "version": 2},
                "contentChanges": [{"text": "(using nor"}]
            }),
        ),
        request(2, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]);
    let updates: Vec<_> = responses
        .iter()
        .map(|response| &response["params"])
        .filter(|params| params["uri"] == main.as_str())
        .collect();
    assert_eq!(updates.len(), 2, "{responses:?}");
    let diagnostics = updates[0]["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert_eq!(diagnostics[0]["code"], "PrivateReference");
    assert_eq!(diagnostics[1]["code"], "MissingReference");
    let declaration_column = declaration.find("hidden").unwrap();
    assert_eq!(
        diagnostics[0]["relatedInformation"][0]["location"],
        json!({
            "uri": basis.as_str(), "range": {
                "start": {"line": 2, "character": declaration_column},
                "end": {"line": 2, "character": declaration_column + "hidden".len()}
            }
        })
    );
    assert_eq!(updates[0]["version"], 1);
    assert_eq!(updates[1]["version"], 1);
    assert_eq!(updates[1]["diagnostics"], json!([]));
    let basis_syntax = responses
        .iter()
        .position(|response| {
            let params = &response["params"];
            params["uri"] == basis.as_str()
                && params["version"] == 2
                && params["diagnostics"][0]["code"] == "UnclosedList"
        })
        .unwrap();
    let cleared = responses
        .iter()
        .position(|response| {
            let params = &response["params"];
            params["uri"] == main.as_str() && params["diagnostics"] == json!([])
        })
        .unwrap();
    assert!(basis_syntax < cleared, "{responses:?}");
}

#[cfg(windows)]
#[test]
fn alternate_windows_project_paths_do_not_choose_a_conflicting_overlay() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let file = examples.join("project/main.ncg2");
    let alias = examples.join("PROJECT/main.ncg2");
    if !alias.is_file() {
        return;
    }
    let file = Url::from_file_path(file).unwrap();
    let alias = Url::from_file_path(alias).unwrap();
    let open = |uri: &Url, source: &str| {
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri.as_str(), "languageId": "ncg2", "version": 1, "text": source
            }}),
        )
    };
    let responses = exchange([
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        open(&file, "(func entry (in x) (out y) (def y (missing x)))"),
        open(&alias, "(func entry (in x) (out y) (def y (nor x x)))"),
        request(2, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]);
    let updates: Vec<_> = responses
        .iter()
        .map(|response| &response["params"])
        .filter(|params| params["uri"] == file.as_str())
        .collect();
    assert_eq!(updates.len(), 2, "{responses:?}");
    assert_eq!(updates[0]["diagnostics"][0]["code"], "MissingReference");
    assert_eq!(updates[1]["diagnostics"], json!([]));
}

fn exchange(messages: impl IntoIterator<Item = Message>) -> Vec<Value> {
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
    std::iter::from_fn(|| Message::read(&mut stdout).unwrap())
        .map(|message| serde_json::to_value(message).unwrap())
        .collect()
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
