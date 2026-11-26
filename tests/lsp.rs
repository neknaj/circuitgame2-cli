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
fn stdio_checks_ncg2m_after_sequential_edits_and_a_full_replacement() {
    let uri = "untitled:editing.ncg2m";
    let initial = concat!(
        "(defn quoted (as-type (Fn () Syntax) (fn () \"🦊\"))) ",
        "(defn choose (as-type (Fn (Bool Bool) Bool) (fn (left right) left)))",
    );
    let utf16_column = |text: &str, byte| text[..byte].encode_utf16().count();
    let replace = |text: &str, needle: &str, value: &str| {
        let start = text.find(needle).unwrap();
        json!({
            "range": {
                "start": {"line": 0, "character": utf16_column(text, start)},
                "end": {"line": 0, "character": utf16_column(text, start + needle.len())}
            },
            "text": value
        })
    };
    let change = |version, edits| {
        notification(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri, "version": version}, "contentChanges": edits}),
        )
    };
    let after_emoji = initial.replacen('🦊', "猫", 1);
    let after_both = after_emoji.replacen("right", "left", 1);
    let messages = [
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "languageId": "ncg2m", "version": 1, "text": initial
            }}),
        ),
        change(
            3,
            json!([
                replace(initial, "🦊", "猫"),
                replace(&after_emoji, "right", "left")
            ]),
        ),
        change(2, json!([{"text": ")"}])),
        change(
            4,
            json!([{
                "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 0}},
                "text": ")"
            }]),
        ),
        change(5, json!([replace(initial, "right", "value")])),
        change(6, json!([{"text": format!("bare {after_both}")}])),
        change(7, json!([{"text": ")"}])),
        change(8, json!([{"text": initial}])),
        request(
            2,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        ),
        notification(
            "textDocument/didClose",
            json!({"textDocument": {"uri": uri}}),
        ),
        request(3, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ];
    let responses = exchange(messages);
    let published: Vec<_> = responses
        .iter()
        .filter(|message| message["method"] == "textDocument/publishDiagnostics")
        .map(|message| &message["params"])
        .collect();
    let versions: Vec<_> = published
        .iter()
        .map(|publication| publication["version"].as_i64())
        .collect();
    assert_eq!(
        versions,
        [
            Some(1),
            Some(3),
            Some(4),
            Some(5),
            Some(6),
            Some(7),
            Some(8),
            None
        ]
    );
    assert!(
        published
            .iter()
            .all(|publication| publication["uri"] == uri)
    );
    let reports = |index: usize| published[index]["diagnostics"].as_array().unwrap();
    for index in [0, 2, 3, 6, 7] {
        assert!(reports(index).is_empty(), "{published:?}");
    }
    assert_eq!(reports(1).len(), 1);
    let diagnostic = &reports(1)[0];
    assert_eq!(diagnostic["source"], "NCG2M");
    assert_eq!(diagnostic["code"], "DuplicateDefnParameter");
    let first = after_both.find("left left").unwrap();
    let duplicate = first + "left ".len();
    let expected_range = |byte| {
        json!({
            "start": {"line": 0, "character": utf16_column(&after_both, byte)},
            "end": {"line": 0, "character": utf16_column(&after_both, byte + "left".len())},
        })
    };
    assert_eq!(diagnostic["range"], expected_range(duplicate));
    assert_eq!(diagnostic["data"]["byteStart"], duplicate);
    assert_eq!(diagnostic["data"]["byteEnd"], duplicate + "left".len());
    let related = &diagnostic["relatedInformation"][0]["location"];
    assert_eq!(related["uri"], uri);
    assert_eq!(related["range"], expected_range(first));
    assert_eq!(reports(4).len(), 2);
    assert_eq!(reports(4)[0]["code"], "ExpectedMetaItem");
    assert_eq!(reports(4)[0]["data"]["byteStart"], 0);
    assert_eq!(reports(4)[0]["data"]["byteEnd"], "bare".len());
    assert_eq!(reports(4)[1]["code"], "DuplicateDefnParameter");
    assert_eq!(
        reports(4)[1]["data"]["byteStart"],
        "bare ".len() + duplicate
    );
    assert_eq!(reports(5).len(), 1);
    assert_eq!(reports(5)[0]["code"], "UnexpectedClose");
    let symbols = responses.iter().find(|message| message["id"] == 2).unwrap();
    assert_eq!(symbols["result"].as_array().unwrap().len(), 2);
    assert_eq!(symbols["result"][0]["name"], "quoted");
    assert_eq!(symbols["result"][1]["name"], "choose");
}

#[test]
fn stdio_checks_the_unsaved_ncg2m_text_of_a_mixed_project_file() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path();
    let circuit = directory.join("main.ncg2");
    let functional = directory.join("補助 関数.ncg2m");
    let circuit_text = "(using nor)";
    std::fs::write(
        directory.join("circuitgame2.toml"),
        "sources = [\"main.ncg2\", \"補助 関数.ncg2m\"]\n",
    )
    .unwrap();
    std::fs::write(&circuit, circuit_text).unwrap();
    std::fs::write(&functional, "(defn ok (as-type (Fn () Bool) (fn () true)))").unwrap();
    let circuit_uri = Url::from_file_path(circuit).unwrap();
    let functional_uri = Url::from_file_path(functional).unwrap();
    let opened = |uri: &Url, language_id, text| {
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri.as_str(), "languageId": language_id, "version": 1, "text": text,
            }}),
        )
    };
    let messages = [
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        opened(&circuit_uri, "ncg2", circuit_text),
        opened(&functional_uri, "ncg2m", ")"),
        request(2, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ];
    let responses = exchange(messages);
    let published: Vec<_> = responses
        .iter()
        .filter(|message| message["method"] == "textDocument/publishDiagnostics")
        .map(|message| &message["params"])
        .collect();
    assert_eq!(published.len(), 2, "{published:?}");
    assert_eq!(published[0]["uri"], circuit_uri.as_str());
    assert_eq!(published[0]["diagnostics"], json!([]));
    assert_eq!(published[1]["uri"], functional_uri.as_str());
    assert_eq!(published[1]["version"], 1);
    assert_eq!(published[1]["diagnostics"][0]["source"], "NCG2M");
    assert_eq!(published[1]["diagnostics"][0]["code"], "UnexpectedClose");
    assert_eq!(published[1]["diagnostics"][0]["data"]["byteStart"], 0);
    assert_eq!(published[1]["diagnostics"][0]["data"]["byteEnd"], 1);
}

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
fn ncg2m_outline_uses_current_document_and_real_namespace_blocks() {
    let uri = "untitled:outline.ncg2m";
    let source = "(namespace tools (defmacro make () (quote \"🐈\")) (defn ready (as-type (Fn () Bool) (fn () true))) (export ready))\n\
                  (namespace tools (defmacro other () (quote value)))";
    let ready_column = source[..source.find(" ready").unwrap() + 1]
        .encode_utf16()
        .count();
    let ready_span = json!({"start": {"line": 0, "character": ready_column},
                             "end": {"line": 0, "character": ready_column + 5}});
    let damaged = format!(
        "(use ::tools::demo prefix) (defn bad (as-type (Fn (Bool Bool) Bool) (fn (key key) \"🦊\")))\n\
         )\n{source}\n(namespace uncertain (defmacro hidden () (quote value))"
    );
    let document = json!({"textDocument": {"uri": uri}});
    for hierarchical in [false, true] {
        let responses = exchange([
            request(
                1,
                "initialize",
                json!({"capabilities": {"textDocument": {
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": hierarchical}
                }}}),
            ),
            notification("initialized", json!({})),
            notification(
                "textDocument/didOpen",
                json!({"textDocument": {
                    "uri": uri, "languageId": "ncg2m", "version": 1, "text": source
                }}),
            ),
            request(2, "textDocument/documentSymbol", document.clone()),
            notification(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": uri, "version": 2},
                    "contentChanges": [{"text": ")"}]
                }),
            ),
            request(3, "textDocument/documentSymbol", document.clone()),
            notification(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": uri, "version": 3},
                    "contentChanges": [{"text": damaged}]
                }),
            ),
            request(4, "textDocument/documentSymbol", document.clone()),
            notification(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": uri, "version": 4},
                    "contentChanges": [{"text": format!("{damaged})")}]
                }),
            ),
            request(5, "textDocument/documentSymbol", document.clone()),
            notification(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": uri, "version": 5},
                    "contentChanges": [{"text": "(defmacro fresh () (quote value))"}]
                }),
            ),
            request(6, "textDocument/documentSymbol", document.clone()),
            request(7, "shutdown", Value::Null),
            notification("exit", Value::Null),
        ]);
        let result = |id| {
            &responses
                .iter()
                .find(|response| response["id"] == id)
                .unwrap()["result"]
        };
        let symbols = result(2).as_array().unwrap();
        if hierarchical {
            assert_eq!(symbols.len(), 2);
            assert_eq!(symbols[0]["name"], "tools");
            assert_eq!(symbols[0]["children"].as_array().unwrap().len(), 2);
            assert_eq!(symbols[0]["children"][0]["name"], "make");
            assert_eq!(symbols[0]["children"][1]["name"], "ready");
            assert_eq!(symbols[0]["children"][1]["selectionRange"], ready_span);
            assert_eq!(symbols[1]["name"], "tools");
            assert_eq!(symbols[1]["children"].as_array().unwrap().len(), 1);
            assert_eq!(symbols[1]["children"][0]["name"], "other");
        } else {
            let names: Vec<_> = symbols
                .iter()
                .map(|symbol| symbol["name"].as_str().unwrap())
                .collect();
            assert_eq!(names, ["tools", "make", "ready", "tools", "other"]);
            assert_eq!(symbols[2]["location"]["range"], ready_span);
            assert_eq!(symbols[2]["containerName"], "tools");
        }
        assert!(result(3).is_null());
        let diagnostics = &responses
            .iter()
            .find(|message| {
                message["method"] == "textDocument/publishDiagnostics"
                    && message["params"]["version"] == 3
            })
            .unwrap()["params"]["diagnostics"];
        let names: Vec<_> = diagnostics
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["code"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["DuplicateDefnParameter", "UnexpectedClose", "UnclosedList"]
        );
        let first = damaged.find("key key").unwrap();
        let duplicate = first + "key ".len();
        assert_eq!(diagnostics[0]["data"]["byteStart"], duplicate);
        assert_eq!(
            diagnostics[0]["data"]["related"],
            json!([first, first + "key".len()])
        );
        let eof = damaged.len();
        let opening = damaged.rfind("(namespace uncertain").unwrap();
        assert_eq!(diagnostics[2]["data"]["byteStart"], eof);
        assert_eq!(diagnostics[2]["data"]["byteEnd"], eof);
        assert_eq!(
            diagnostics[2]["data"]["related"],
            json!([opening, opening + 1])
        );
        let recovered = result(4).as_array().unwrap();
        assert_eq!(recovered.len(), symbols.len() + usize::from(hierarchical));
        let mut moved = ready_span.clone();
        moved["start"]["line"] = 2.into();
        moved["end"]["line"] = 2.into();
        if hierarchical {
            assert_eq!(recovered[0]["children"][1]["name"], "ready");
            assert_eq!(recovered[0]["children"][1]["selectionRange"], moved);
            assert_eq!(recovered[1]["children"][0]["name"], "other");
            assert_eq!(recovered[2]["name"], "hidden");
            assert_eq!(recovered[2]["detail"], "所属未確定");
            assert_eq!(
                recovered[2]["selectionRange"]["start"],
                json!({"line": 4, "character": 31})
            );
            assert_eq!(result(5)[2]["name"], "uncertain");
            assert_eq!(result(5)[2]["children"][0]["name"], "hidden");
            assert_eq!(result(5)[2]["children"][0]["detail"], "defmacro");
        } else {
            assert_eq!(recovered[2]["name"], "ready");
            assert_eq!(recovered[2]["location"]["range"], moved);
            assert_eq!(recovered[4]["name"], "other");
            assert_eq!(result(5)[6]["name"], "hidden");
            assert_eq!(result(5)[6]["containerName"], "uncertain");
        }
        assert_eq!(result(6).as_array().unwrap().len(), 1);
        assert_eq!(result(6)[0]["name"], "fresh");
    }
}

#[test]
fn document_symbols_hover_and_semantic_tokens_cover_the_same_circuit() {
    let uri = "untitled:navigation.ncg2";
    let source = "(using nor)\n\
        (namespace logic\n\
          (func invert (in input) (out result) (def result (nor input input)))\n\
          (module gate (in x) (out y) (def wire (invert x)) (def y wire)))\n";
    let lines: Vec<_> = source.lines().collect();
    let document = json!({"textDocument": {"uri": uri}});
    let hover_at = |id, line: usize, word| {
        let character = lines[line].find(word).unwrap();
        request(
            id,
            "textDocument/hover",
            json!({
                "textDocument": {"uri": uri}, "position": {"line": line, "character": character}
            }),
        )
    };
    let responses = exchange([
        request(
            1,
            "initialize",
            json!({
                "initializationOptions": {"ncg2SemanticTokens": true},
            "capabilities": {"textDocument": {
                "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                "semanticTokens": {"requests": {"full": true}, "tokenTypes": ["struct"], "tokenModifiers": ["declaration"], "formats": ["relative"]}
            }}
            }),
        ),
        notification("initialized", json!({})),
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "languageId": "ncg2", "version": 1, "text": source
            }}),
        ),
        request(2, "textDocument/documentSymbol", document.clone()),
        hover_at(3, 3, "invert"),
        hover_at(4, 3, "wire"),
        request(5, "textDocument/semanticTokens/full", document.clone()),
        notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": format!(")\n{source}")}]
            }),
        ),
        request(6, "textDocument/documentSymbol", document),
        request(7, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]);
    let result = |id| {
        &responses
            .iter()
            .find(|response| response["id"] == id)
            .unwrap()["result"]
    };
    let capabilities = &result(1)["capabilities"];
    assert_eq!(capabilities["hoverProvider"], true);
    assert_eq!(capabilities["documentSymbolProvider"], true);
    let legend = capabilities["semanticTokensProvider"]["legend"]["tokenTypes"]
        .as_array()
        .unwrap();
    assert_eq!(result(2)[0]["name"], "logic");
    assert_eq!(result(2)[0]["children"][0]["name"], "invert");
    assert_eq!(result(2)[0]["children"][1]["name"], "gate");
    assert_eq!(
        result(3)["contents"]["value"],
        "func ::logic::invert\n(in input)\n(out result)"
    );
    assert_eq!(result(4)["contents"]["value"], "内部信号 wire\n(bits 1)");

    let tokens = result(5)["data"].as_array().unwrap();
    let mut line = 0;
    let mut column = 0;
    let mut names = Vec::new();
    for token in tokens.chunks_exact(5) {
        let number = |position: usize| token[position].as_u64().unwrap() as usize;
        if number(0) != 0 {
            line += number(0);
            column = 0;
        }
        column += number(1);
        names.push((
            &lines[line][column..column + number(2)],
            legend[number(3)].as_str().unwrap(),
            number(4),
        ));
    }
    for expected in [
        ("logic", "ncg2Namespace", 1),
        ("invert", "ncg2Function", 1),
        ("invert", "ncg2Function", 0),
        ("gate", "ncg2Module", 1),
        ("wire", "ncg2Signal", 1),
        ("wire", "ncg2Signal", 0),
        ("x", "ncg2Port", 1),
        ("x", "ncg2Port", 0),
    ] {
        assert!(
            names.contains(&expected),
            "{expected:?} is missing from {names:?}"
        );
    }
    assert!(
        result(6)
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["name"] == "gate" && item["containerName"] == "logic" })
    );
}

#[test]
fn semantic_legend_respects_standard_clients() {
    let responses = exchange([
        request(
            1,
            "initialize",
            json!({"capabilities": {"textDocument": {
                "semanticTokens": {
                    "requests": {"full": true},
                    "tokenTypes": ["namespace", "function", "class", "variable", "parameter"],
                    "tokenModifiers": [],
                    "formats": ["relative"]
                }
            }}}),
        ),
        notification("initialized", json!({})),
        request(2, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]);
    assert_eq!(
        responses[0]["result"]["capabilities"]["semanticTokensProvider"]["legend"],
        json!({
            "tokenTypes": ["namespace", "function", "class", "variable", "parameter"],
            "tokenModifiers": [],
        }),
    );
}
#[test]
fn recovered_outline_and_tokens_use_the_negotiated_columns() {
    let uri = "untitled:recovered-symbols.ncg2";
    let source = "\"漢😀\" (func flip (in x) (out y) (def y (nor x x)))";
    let prefix = &source[..source.find("flip").unwrap()];
    for (encoding, column) in [
        ("utf-8", prefix.len()),
        ("utf-16", prefix.encode_utf16().count()),
        ("utf-32", prefix.chars().count()),
    ] {
        let document = json!({"textDocument": {"uri": uri}});
        let responses = exchange([
            request(
                1,
                "initialize",
                json!({"capabilities": {
                    "general": {"positionEncodings": [encoding]},
                    "textDocument": {
                        "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                        "semanticTokens": {
                            "requests": {"full": true}, "tokenTypes": ["function"],
                            "tokenModifiers": [], "formats": ["relative"]
                        }
                    }
                }}),
            ),
            notification("initialized", json!({})),
            notification(
                "textDocument/didOpen",
                json!({"textDocument": {
                    "uri": uri, "languageId": "ncg2", "version": 1, "text": source
                }}),
            ),
            request(2, "textDocument/documentSymbol", document.clone()),
            request(3, "textDocument/semanticTokens/full", document),
            request(4, "shutdown", Value::Null),
            notification("exit", Value::Null),
        ]);
        let result = |id| &responses.iter().find(|item| item["id"] == id).unwrap()["result"];
        assert_eq!(result(2)[0]["name"], "flip");
        assert_eq!(
            result(2)[0]["location"]["range"]["start"]["character"],
            column
        );
        assert_eq!(result(3)["data"], json!([0, column, 4, 0, 0]));
    }
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

#[test]
fn configured_reference_diagnostics_follow_edits_and_closed_dependencies() {
    let target = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let basis = target.path().join("basis.ncg2");
    let main = target.path().join("main.ncg2");
    let unrelated = other.path().join("main.ncg2");
    let using = "(using nor)";
    let hidden = "(namespace lib (func hidden (in x) (out y) (def y (nor x x))))";
    let basis_text = format!("{using}\n{hidden}");
    let exported = basis_text.replace("(namespace lib ", "(namespace lib (export hidden) ");
    let main_text =
        "(func entry (in x) (out y) (def y (::lib::hidden x)) (def z (missing x)))";
    std::fs::write(target.path().join("circuitgame2.toml"), "sources = [\"basis.ncg2\", \"main.ncg2\"]").unwrap();
    std::fs::write(other.path().join("circuitgame2.toml"), "sources = [\"main.ncg2\"]").unwrap();
    std::fs::write(&basis, &basis_text).unwrap();
    std::fs::write(&main, main_text).unwrap();
    std::fs::write(&unrelated, using).unwrap();
    let basis = Url::from_file_path(basis).unwrap();
    let main = Url::from_file_path(main).unwrap();
    let unrelated = Url::from_file_path(unrelated).unwrap();
    let open = |uri: &Url, text: &str| {
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri.as_str(), "languageId": "ncg2", "version": 1, "text": text}}),
        )
    };
    let change = |uri: &Url, version, text: &str| {
        notification(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri.as_str(), "version": version}, "contentChanges": [{"text": text}]}),
        )
    };
    let responses = exchange([
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        open(&main, main_text),
        open(&unrelated, using),
        change(&unrelated, 2, "(using nor) ; 編集"),
        open(&basis, &exported),
        change(&main, 2, &format!("; 🦊\n{main_text}")),
        change(&main, 1, "(using nor"),
        notification("textDocument/didClose", json!({"textDocument": {"uri": basis.as_str()}})),
        request(2, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]);
    let updates: Vec<_> = responses.iter().filter_map(|message| {
        let params = &message["params"];
        (params["uri"] == main.as_str()).then_some(params)
    }).collect();
    assert!(updates.windows(2).all(|window| window[0]["version"].as_i64() <= window[1]["version"].as_i64()));
    let semantic: Vec<_> = updates.into_iter().filter(|params| {
        params["diagnostics"].as_array().is_some_and(|items| !items.is_empty())
    }).collect();
    assert_eq!(semantic.len(), 4, "{responses:?}");
    assert_eq!(semantic[0]["version"], 1);
    assert_eq!(semantic[0]["diagnostics"][0]["code"], "PrivateReference");
    assert_eq!(semantic[1]["version"], 1);
    assert_eq!(semantic[1]["diagnostics"][0]["code"], "MissingReference");
    assert_eq!(semantic[2]["version"], 2);
    assert_eq!(semantic[2]["diagnostics"][0]["range"]["start"]["line"], 1);
    assert_eq!(semantic[3]["version"], 2);
    let private = &semantic[3]["diagnostics"][0];
    assert_eq!(private["code"], "PrivateReference");
    assert_eq!(private["range"]["start"]["line"], 1);
    assert_eq!(private["relatedInformation"][0]["location"]["uri"], basis.as_str());
    assert_eq!(private["relatedInformation"][0]["location"]["range"]["start"]["line"], 1);
}

#[test]
fn cached_absolute_diagnostics_match_fresh_positions_after_equal_byte_edits() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.ncg2");
    let library = directory.path().join("lib/defs.ncg2");
    std::fs::create_dir(library.parent().unwrap()).unwrap();
    std::fs::write(
        directory.path().join("circuitgame2.toml"),
        "sources = [\"lib/defs.ncg2\", \"main.ncg2\"]",
    )
    .unwrap();
    let library_text = "(using nor) (namespace lib (export Pair) (func hidden (in x) (out x)) (struct Pair (width 1)))";
    let main_text = "(func run (in x) (out q) (def q (::lib::hidden x)) (def r (::lib::Pair x)) (def s (missing x)))";
    let library_edit = library_text.replacen(") (namespace", ")\n(namespace", 1);
    let main_edit = main_text.replacen(") (def q", ")\n(def q", 1);
    std::fs::write(&main, main_text).unwrap();
    std::fs::write(&library, library_text).unwrap();
    let main = Url::from_file_path(main).unwrap();
    let library = Url::from_file_path(library).unwrap();

    let open = |uri: &Url, version, text: &str| {
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri.as_str(), "languageId": "ncg2", "version": version, "text": text}}),
        )
    };
    let change = |uri: &Url, version, text: &str| {
        notification(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri.as_str(), "version": version}, "contentChanges": [{"text": text}]}),
        )
    };
    let diagnostics = |messages: Vec<Value>| -> Vec<Value> {
        messages
            .into_iter()
            .filter(|message| message["method"] == "textDocument/publishDiagnostics")
            .map(|message| message["params"].clone())
            .filter(|params| {
                params["uri"] == main.as_str()
                    && params["diagnostics"]
                        .as_array()
                        .is_some_and(|items| !items.is_empty())
            })
            .collect()
    };

    let edited = diagnostics(exchange([
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        open(&library, 1, library_text),
        open(&main, 1, main_text),
        change(&library, 2, &library_edit),
        change(&main, 2, &main_edit),
        request(2, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]));
    std::fs::write(library.to_file_path().unwrap(), &library_edit).unwrap();
    let fresh = diagnostics(exchange([
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        open(&main, 2, &main_edit),
        request(2, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]));
    assert_eq!(edited.len(), 3, "{edited:?}");
    assert_eq!(&edited[2..], fresh.as_slice());
    assert_eq!(edited[0]["version"], 1);
    assert_eq!(edited[1]["version"], 1);
    assert_eq!(edited[2]["version"], 2);
    let last = edited[2]["diagnostics"].as_array().unwrap();
    assert_eq!(
        last.iter()
            .map(|item| item["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "PrivateReference",
            "WrongDeclarationKind",
            "MissingReference"
        ]
    );
    assert_eq!(last[0]["range"]["start"]["line"], 1);
    assert_eq!(
        last[0]["relatedInformation"][0]["location"]["uri"],
        library.as_str()
    );
    assert_eq!(
        last[0]["relatedInformation"][0]["location"]["range"]["start"]["line"],
        1
    );
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
    assert_eq!(first["label"], "NOR 1 · 定常化 ≤ 1 tick");
    assert!(first.get("kind").is_none());
    assert!(first.get("textEdits").is_none());
    assert_eq!(response(3), &json!([]));
    assert_eq!(response(4)[0]["label"], "NOR 2 · 定常化 ≤ 2 tick");
    assert_eq!(response(5), &json!([]));
}

#[test]
fn inlay_hints_count_functions_and_modules_without_a_settling_bound() {
    let uri = "untitled:counts.ncg2";
    let source = "(using nor)\n(func direct (in x) (out x))\n(module oscillator (in) (out q) (def q (nor q q)))\n";
    let responses = exchange([
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "languageId": "ncg2", "version": 1, "text": source
            }}),
        ),
        request(
            2,
            "textDocument/inlayHint",
            json!({"textDocument": {"uri": uri}, "range": {
                "start": {"line": 1, "character": 0},
                "end": {"line": 3, "character": 0}
            }}),
        ),
        request(3, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]);
    let hints = responses
        .iter()
        .find(|response| response["id"] == 2)
        .unwrap()["result"]
        .as_array()
        .unwrap();
    assert_eq!(hints.len(), 2);
    assert_eq!(hints[0]["position"], json!({"line": 1, "character": 12}));
    assert_eq!(hints[0]["label"], "NOR 0");
    assert_eq!(hints[1]["position"], json!({"line": 2, "character": 18}));
    assert_eq!(hints[1]["label"], "NOR 1");
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
    let main_source = "(module top (in x) (out y) (def y (::lib::forward x)))";
    let definition = |id| {
        request(
            id,
            "textDocument/definition",
            json!({"textDocument": {"uri": main.as_str()}, "position": {
                "line": 0, "character": main_source.find("::lib::forward").unwrap()
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
        open(&main, main_source),
        hint(2),
        definition(7),
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
        definition(8),
        open(
            &alias,
            "(using nor) (namespace lib (export forward) (func forward (in x) (out y) (def y (nor x x))))",
        ),
        hint(5),
        definition(9),
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
    assert_eq!(
        responses[at(2)]["result"][0]["label"],
        "NOR 1 · 定常化 ≤ 1 tick"
    );
    assert_eq!(
        responses[at(3)]["result"][0]["label"],
        "NOR 2 · 定常化 ≤ 2 tick"
    );
    assert_eq!(responses[at(4)]["result"], json!([]));
    assert!(
        responses[at(4) + 1..at(5)]
            .iter()
            .any(|response| response["method"] == "workspace/inlayHint/refresh"),
        "{responses:?}"
    );
    assert_eq!(
        responses[at(5)]["result"][0]["label"],
        "NOR 1 · 定常化 ≤ 1 tick"
    );
    let target = &responses[at(7)]["result"];
    assert_eq!(target["uri"], basis.as_str());
    assert!(responses[at(8)]["result"].is_null());
    let reopened = &responses[at(9)]["result"];
    assert_eq!(reopened["uri"], alias.as_str());
    assert_eq!(reopened["range"], target["range"]);
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

#[test]
fn completion_follows_open_dependency_edits_and_close() {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/project");
    let basis = Url::from_file_path(project.join("basis.ncg2")).unwrap();
    let main = Url::from_file_path(project.join("main.ncg2")).unwrap();
    let open = |uri: &Url, text| {
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri.as_str(), "languageId": "ncg2", "version": 1, "text": text}}),
        )
    };
    let completion = |id| {
        request(
            id,
            "textDocument/completion",
            json!({"textDocument": {"uri": main.as_str()}, "position": {"line": 0, "character": 15}}),
        )
    };
    let responses = exchange([
        request(1, "initialize", json!({"capabilities": {}})),
        notification("initialized", json!({})),
        open(&basis, "(using nor) (module gate (in x) (out y)) (bad)"),
        open(&main, "(module-part ga (def y x))"),
        completion(2),
        notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": basis.as_str(), "version": 2},
                "contentChanges": [{"text": ") (using nor) (module gait (in x) (out y))"}]
            }),
        ),
        completion(3),
        notification(
            "textDocument/didClose",
            json!({"textDocument": {"uri": basis.as_str()}}),
        ),
        completion(4),
        request(5, "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]);
    let result = |id| &responses.iter().find(|value| value["id"] == id).unwrap()["result"];
    assert_eq!(result(2)["items"][0]["label"], "gate");
    assert_eq!(result(3)["items"][0]["label"], "gait");
    assert_eq!(result(4)["items"], json!([]));
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
