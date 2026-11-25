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

#[test]
fn inlay_hints_follow_the_requested_range_and_latest_edit() {
    let uri = "untitled:clocked.ncg2";
    let initial = "(using nor)\n(module entry (in x) (out y) (def y (nor x x)))\n";
    let edited = "(using nor)\n(module entry (in x) (out y) (def z (nor x x)) (def y (nor z z)))\n";
    let hint = |id, end| {
        request(
            id,
            "textDocument/inlayHint",
            json!({"textDocument": {"uri": uri}, "range": {
                "start": {"line": 1, "character": 0},
                "end": {"line": 1, "character": end}
            }}),
        )
    };
    let change = |version, text| {
        notification(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]}),
        )
    };
    let responses = exchange([
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "languageId": "ncg2", "version": 1, "text": initial
            }}),
        ),
        hint(2, 20),
        hint(3, 12),
        change(2, edited),
        hint(4, 20),
        change(3, "(using nor)\n(module entry (in x) ("),
        hint(5, 20),
        request(6, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]);
    let response = |id| {
        &responses
            .iter()
            .find(|response| response["id"] == id)
            .unwrap()["result"]
    };
    assert_eq!(response(1)["capabilities"]["inlayHintProvider"], true);
    let first = &response(2)[0];
    assert_eq!(first["position"], json!({"line": 1, "character": 13}));
    assert_eq!(first["label"], "定常化 ≤ 1 tick");
    assert!(first.get("kind").is_none());
    assert!(first.get("textEdits").is_none());
    assert_eq!(response(3), &json!([]));
    assert_eq!(response(4)[0]["label"], "定常化 ≤ 2 tick");
    assert_eq!(response(5), &json!([]));
}

#[test]
fn inlay_hint_refreshes_when_an_open_dependency_changes() {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/project");
    let basis = Url::from_file_path(project.join("basis.ncg2")).unwrap();
    let main = Url::from_file_path(project.join("main.ncg2")).unwrap();
    let spelling = if cfg!(windows) {
        "/BASIS.ncg2"
    } else {
        "/%62asis.ncg2"
    };
    let alias = Url::parse(&basis.as_str().replace("/basis.ncg2", spelling)).unwrap();
    let open = |uri: &Url, text| {
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri.as_str(), "languageId": "ncg2", "version": 1, "text": text}}),
        )
    };
    let hint = |id| {
        request(
            id,
            "textDocument/inlayHint",
            json!({"textDocument": {"uri": main.as_str()}, "range": {
                "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 20}
            }}),
        )
    };
    let responses = exchange([
        request(
            1,
            "initialize",
            json!({"capabilities": {"workspace": {"inlayHint": {"refreshSupport": true}}}}),
        ),
        notification("initialized", json!({})),
        open(
            &basis,
            "(using nor) (namespace lib (export forward) (func forward (in x) (out y) (def y (nor x x))))",
        ),
        open(
            &main,
            "(module top (in x) (out y) (def y (::lib::forward x)))",
        ),
        hint(2),
        notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": basis.as_str(), "version": 2},
                "contentChanges": [{"text": "(using nor) (namespace lib (export forward) (func forward (in x) (out y) (def z (nor x x)) (def y (nor z z))))"}]
            }),
        ),
        hint(3),
        notification(
            "textDocument/didClose",
            json!({"textDocument": {"uri": basis.as_str()}}),
        ),
        hint(4),
        open(
            &alias,
            "(using nor) (namespace lib (export forward) (func forward (in x) (out y) (def y (nor x x))))",
        ),
        hint(5),
        request(6, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]);
    let at = |id| {
        responses
            .iter()
            .position(|response| response["id"] == id)
            .unwrap()
    };
    let refreshed = responses
        .iter()
        .position(|response| response["method"] == "workspace/inlayHint/refresh")
        .unwrap();
    assert!(at(2) < refreshed && refreshed < at(3), "{responses:?}");
    assert_eq!(responses[at(2)]["result"][0]["label"], "定常化 ≤ 1 tick");
    assert_eq!(responses[at(3)]["result"][0]["label"], "定常化 ≤ 2 tick");
    assert_eq!(responses[at(4)]["result"], json!([]));
    assert!(
        responses[at(4) + 1..at(5)]
            .iter()
            .any(|response| response["method"] == "workspace/inlayHint/refresh"),
        "{responses:?}"
    );
    assert_eq!(responses[at(5)]["result"][0]["label"], "定常化 ≤ 1 tick");
}

#[test]
fn completion_replaces_the_original_module_part_name_after_a_syntax_error() {
    let uri = "untitled:parts.ncg2";
    let before_name = "\x22漢😀\x22 (module-part ";
    let start = before_name.encode_utf16().count();
    let end = start + 2;
    let source = format!(
        "(using nor)\n) (module gate (in x) (out y))\n(func garden (in x) (out y) (def y x))\n{before_name}ga (def y x))"
    );
    let query = |id, method, line, offset| {
        request(
            id,
            method,
            json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": offset}}),
        )
    };
    let responses = exchange([
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "languageId": "ncg2", "version": 1, "text": source
            }}),
        ),
        query(2, "textDocument/completion", 3, end),
        query(3, "textDocument/definition", 3, start),
        query(4, "textDocument/completion", 3, end + 2),
        notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": "\x22(module-part ga"}]
            }),
        ),
        query(5, "textDocument/completion", 0, 16),
        request(6, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]);
    let response = |id| {
        &responses
            .iter()
            .find(|message| message["id"] == id)
            .unwrap()["result"]
    };
    assert!(response(1)["capabilities"]["completionProvider"].is_object());
    let list = response(2);
    assert_eq!(list["isIncomplete"], true);
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["label"], "gate");
    assert!(items[0]["detail"].as_str().unwrap().contains("暫定"));
    assert_eq!(
        items[0]["textEdit"],
        json!({
            "newText": "gate", "range": {
                "start": {"line": 3, "character": start}, "end": {"line": 3, "character": end}
            }
        })
    );
    assert!(response(3).is_null());
    assert!(response(4).is_null());
    assert!(response(5).is_null());
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
