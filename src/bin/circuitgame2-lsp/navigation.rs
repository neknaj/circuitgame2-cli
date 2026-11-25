//! 文書の同じ版で解決した名前から、ホバーとセマンティックトークンを作る。

use std::{collections::BTreeSet, ops::Range};

use circuitgame2_cli::CLOCKED_LIMITS;

use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding, TextPosition},
    project::{
        CircuitProject, DefinitionBinding, ModuleBodyPosition, ProjectSource, ResolvedPortType,
        SignalOccurrence, SignalOccurrenceKind, SignalRole, UseAliases, UseTargets,
        check_use_aliases, index_use_targets, normalize_path, validate_circuit_basis,
    },
    syntax::{
        Form,
        circuit::{Head, NamedForm, routine_ports},
    },
};
use lsp_types::{
    Hover, HoverContents, MarkupContent, MarkupKind, SemanticToken, SemanticTokenModifier,
    SemanticTokenType, SemanticTokens, SemanticTokensClientCapabilities, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, TokenFormat,
};

use super::{
    diagnostic::wire_range,
    sources::{OpenDocument, SourceSet},
};

#[derive(Clone, Copy)]
enum TokenKind {
    Namespace,
    Func,
    Module,
    Signal,
    Port,
    Struct,
}

impl TokenKind {
    fn for_head(head: Head) -> Option<Self> {
        Some(match head {
            Head::Namespace => Self::Namespace,
            Head::Func => Self::Func,
            Head::Module | Head::ModulePart | Head::Latch | Head::Memory => Self::Module,
            Head::Struct => Self::Struct,
            _ => return None,
        })
    }
}

struct TokenSite {
    span: Range<usize>,
    kind: TokenKind,
    declaration: bool,
}

fn head_word(head: Head) -> &'static str {
    match head {
        Head::Using => "using",
        Head::Namespace => "namespace",
        Head::Use => "use",
        Head::Export => "export",
        Head::Func => "func",
        Head::Module => "module",
        Head::ModulePart => "module-part",
        Head::Struct => "struct",
        Head::Latch => "latch",
        Head::Memory => "memory",
        Head::Test => "test",
    }
}

fn with_project<'a, R>(
    current: OpenDocument<'a>,
    open: impl Iterator<Item = OpenDocument<'a>>,
    query: impl FnOnce(&CircuitProject<'_>, usize) -> Option<R>,
) -> Option<R> {
    let (id, sources) = SourceSet::load(current, open).ok()?;
    let path = normalize_path(&id).ok()?;
    let project = sources.parse_strict().ok()?;
    let file = project
        .sources()
        .iter()
        .position(|source| source.id.same_file(&path))?;
    let result = query(&project, file)?;
    sources
        .still_current([(current.uri, id.as_str())])
        .then_some(result)
}

fn form_text<'a>(form: &Form, source: &'a str) -> Option<&'a str> {
    let (Form::List { span, .. } | Form::Atom { span, .. }) = form;
    source.get(span.clone())
}

fn qualified_name(form: &NamedForm<'_>) -> String {
    match &form.namespace {
        Some(namespace) => format!("::{}::{}", namespace.word, form.name.word),
        None => form.name.word.to_owned(),
    }
}

fn declaration_text(
    project: &CircuitProject<'_>,
    source: &ProjectSource<'_>,
    form: &NamedForm<'_>,
) -> String {
    let name = qualified_name(form);
    let mut text = format!("{} {}", head_word(form.head), name);
    let Some(file) = project
        .sources()
        .iter()
        .position(|other| other.id.same_file(&source.id))
    else {
        return text;
    };
    let parsed = &project.circuits()[file];
    let Some(entry) = parsed
        .syntax
        .entries
        .iter()
        .position(|entry| entry.form == form.form)
    else {
        return text;
    };
    if let Some(ports) = routine_ports(source.text(), &parsed.syntax, entry) {
        for (label, ports) in [("in", &ports.inputs), ("out", &ports.outputs)] {
            text.push_str("\n(");
            text.push_str(label);
            for port in ports {
                if let Some(value) = parsed
                    .syntax
                    .forms
                    .get(port.form)
                    .and_then(|form| form_text(form, source.text()))
                {
                    text.push(' ');
                    text.push_str(value);
                }
            }
            text.push(')');
        }
    }
    text
}

fn signal_sites<'p, 'a>(
    project: &'p CircuitProject<'a>,
    targets: &UseTargets<'p, 'a>,
    aliases: &UseAliases<'_, 'p, 'a>,
    file: usize,
    offset: Option<usize>,
) -> Vec<SignalOccurrence<'p, 'a>> {
    let mut signals = Vec::new();
    let mut owners = BTreeSet::new();
    let parsed = &project.circuits()[file];
    for (entry, item) in parsed.syntax.entries.iter().enumerate() {
        if let Some(offset) = offset {
            let Some(Form::List { span, .. }) = parsed.syntax.forms.get(item.form) else {
                continue;
            };
            if !span.contains(&offset) {
                continue;
            }
        }
        match item.head {
            Head::Func => {
                if let Ok(Some(sites)) =
                    aliases.observe_func_signals(file, entry, CLOCKED_LIMITS.width)
                {
                    signals.extend(sites);
                }
            }
            Head::Module => {
                owners.insert((file, entry));
            }
            Head::ModulePart => {
                let name = parsed.names.iter().position(|name| name.form == item.form);
                if let Some(part) =
                    name.and_then(|name| targets.resolve_module_part(file, name).ok())
                {
                    owners.insert((part.module_position.file, part.module_position.entry));
                }
            }
            _ => {}
        }
    }
    for (owner_file, owner_entry) in owners {
        let owner = ModuleBodyPosition {
            file: owner_file,
            entry: owner_entry,
        };
        if let Ok(Some(sites)) = aliases.observe_module_signals(owner, file, CLOCKED_LIMITS.width) {
            signals.extend(sites);
        }
    }
    signals
}

fn signal_text(site: &SignalOccurrence<'_, '_>) -> Option<String> {
    let role = match site.role {
        SignalRole::Input => "入力ポート",
        SignalRole::Output => "出力ポート",
        SignalRole::Local => "内部信号",
    };
    let word = site.site.source.text().get(site.site.span.clone())?;
    let mut value = format!("{role} {word}");
    if let Some(ty) = &site.ty {
        match ty.ty {
            ResolvedPortType::Bits(width) => value.push_str(&format!("\n(bits {width})")),
            ResolvedPortType::Struct { form, .. } => {
                value.push('\n');
                value.push_str(&qualified_name(form));
            }
        }
    }
    Some(value)
}

/// カーソル位置の回路名や信号名が指す宣言、署名、型を表示する。
pub(super) fn hover<'a>(
    current: OpenDocument<'a>,
    position: TextPosition,
    encoding: PositionEncoding,
    open: impl Iterator<Item = OpenDocument<'a>>,
) -> Option<Hover> {
    let text = current.text?;
    let index = LineIndex::new(text);
    let offset = index.position_to_byte(position, encoding)?;
    let (span, value) = with_project(current, open, |project, file| {
        let local = &project.circuits()[file];
        let source = &project.sources()[file];
        let selected = local
            .names
            .iter()
            .find(|name| name.name.span.contains(&offset));
        if let Some(name) = selected.filter(|name| name.head != Head::ModulePart) {
            return Some((
                name.name.span.clone(),
                declaration_text(project, source, name),
            ));
        }
        validate_circuit_basis(project).ok()?;
        let targets = index_use_targets(project).ok()?;
        if let Some(binding) = targets.resolve_module_part_at_offset(file, offset) {
            let binding = binding.ok()?;
            return Some((
                binding.part.name.span.clone(),
                declaration_text(project, binding.module_source, binding.module),
            ));
        }
        let aliases = check_use_aliases(&targets).ok()?;
        if let Some(target) = aliases.resolve_reference_at_offset(file, offset) {
            let target = target.ok()?;
            let value = match target.binding {
                DefinitionBinding::Declaration { source, form } => {
                    declaration_text(project, source, form)
                }
                DefinitionBinding::Nor => "基底ゲート nor\n2入力の論理和を反転する。".into(),
            };
            return Some((target.site.name.span, value));
        }
        let site = signal_sites(project, &targets, &aliases, file, Some(offset))
            .into_iter()
            .find(|site| site.site.span.contains(&offset))?;
        let value = signal_text(&site)?;
        Some((site.site.span, value))
    })
    .or_else(|| {
        let name = super::symbols::recovered_sites(current)
            .into_iter()
            .find(|item| item.head != Head::ModulePart && item.name.span.contains(&offset))?;
        Some((
            name.name.span,
            format!("{} {}", head_word(name.head), name.name.word),
        ))
    })?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::PlainText,
            value,
        }),
        range: wire_range(&index, text.as_bytes(), &span, encoding),
    })
}

fn push_name(
    tokens: &mut Vec<TokenSite>,
    word: &str,
    span: Range<usize>,
    kind: TokenKind,
    declaration: bool,
) {
    let mut start = span.start;
    let mut parts = word.split("::").peekable();
    while let Some(part) = parts.next() {
        let end = start + part.len();
        if !part.is_empty() {
            tokens.push(TokenSite {
                span: start..end,
                kind: if parts.peek().is_some() {
                    TokenKind::Namespace
                } else {
                    kind
                },
                declaration,
            });
        }
        start = end + 2;
    }
}

fn declarations(tokens: &mut Vec<TokenSite>, head: Head, word: &str, span: Range<usize>) {
    if let Some(kind) = TokenKind::for_head(head) {
        push_name(tokens, word, span, kind, head != Head::ModulePart);
    }
}

fn token_sites(project: &CircuitProject<'_>, file: usize) -> Vec<TokenSite> {
    let mut tokens = Vec::new();
    let parsed = &project.circuits()[file];
    for item in &parsed.names {
        declarations(
            &mut tokens,
            item.head,
            item.name.word,
            item.name.span.clone(),
        );
    }
    if validate_circuit_basis(project).is_err() {
        return tokens;
    }
    let Ok(targets) = index_use_targets(project) else {
        return tokens;
    };
    let Ok(aliases) = check_use_aliases(&targets) else {
        return tokens;
    };
    for (entry, row) in parsed.entry_ranges.iter().enumerate() {
        for (definition, index) in row.definitions.clone().enumerate() {
            for reference in 0..parsed.definitions[index].references.len() {
                let Ok(resolved) =
                    aliases.resolve_definition_reference(file, entry, definition, reference)
                else {
                    continue;
                };
                let kind = match resolved.binding {
                    DefinitionBinding::Declaration { form, .. } => TokenKind::for_head(form.head),
                    DefinitionBinding::Nor => Some(TokenKind::Func),
                };
                if let Some(kind) = kind {
                    let name = resolved.site.name;
                    push_name(&mut tokens, name.word, name.span, kind, false);
                }
            }
        }
    }
    for site in signal_sites(project, &targets, &aliases, file, None) {
        let kind = match site.role {
            SignalRole::Input | SignalRole::Output => TokenKind::Port,
            SignalRole::Local => TokenKind::Signal,
        };
        tokens.push(TokenSite {
            span: site.site.span,
            kind,
            declaration: matches!(site.occurrence, SignalOccurrenceKind::PortDeclaration)
                || matches!(
                    (site.occurrence, site.role),
                    (SignalOccurrenceKind::Definition, SignalRole::Local)
                ),
        });
    }
    tokens
}

/// クライアントが解釈できる種別と修飾の凡例。独自の配色に対応する拡張は初期化時に指定する。
pub(super) struct SemanticPalette {
    options: SemanticTokensOptions,
    indices: [Option<u32>; 6],
    declarations: bool,
}

impl SemanticPalette {
    pub(super) fn new(
        capabilities: &SemanticTokensClientCapabilities,
        custom: bool,
    ) -> Option<Self> {
        if !capabilities.formats.contains(&TokenFormat::RELATIVE) {
            return None;
        }
        let kinds = [
            ("ncg2Namespace", SemanticTokenType::NAMESPACE),
            ("ncg2Function", SemanticTokenType::FUNCTION),
            ("ncg2Module", SemanticTokenType::CLASS),
            ("ncg2Signal", SemanticTokenType::VARIABLE),
            ("ncg2Port", SemanticTokenType::PARAMETER),
            ("struct", SemanticTokenType::STRUCT),
        ];
        let declarations = capabilities
            .token_modifiers
            .contains(&SemanticTokenModifier::DECLARATION);
        let mut token_types = Vec::new();
        let indices = kinds.map(|(name, fallback)| {
            let kind = SemanticTokenType::new(name);
            let selected = if (custom && name.starts_with("ncg2"))
                || capabilities.token_types.contains(&kind)
            {
                kind
            } else if capabilities.token_types.contains(&fallback) {
                fallback
            } else {
                return None;
            };
            let index = token_types.len() as u32;
            token_types.push(selected);
            Some(index)
        });
        if token_types.is_empty() {
            return None;
        }
        Some(Self {
            options: SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types,
                    token_modifiers: if declarations {
                        vec![SemanticTokenModifier::DECLARATION]
                    } else {
                        Vec::new()
                    },
                },
                full: Some(SemanticTokensFullOptions::Bool(true)),
                ..SemanticTokensOptions::default()
            },
            indices,
            declarations,
        })
    }

    pub(super) fn capability(&self) -> lsp_types::SemanticTokensServerCapabilities {
        self.options.clone().into()
    }
}

/// 現在の文書版の元位置を、クライアントの位置単位で差分符号化する。
pub(super) fn semantic<'a>(
    palette: &SemanticPalette,
    current: OpenDocument<'a>,
    encoding: PositionEncoding,
    open: impl Iterator<Item = OpenDocument<'a>>,
) -> Option<SemanticTokens> {
    let text = current.text?;
    let lines = LineIndex::new(text);
    let mut sites = with_project(current, open, |project, file| {
        Some(token_sites(project, file))
    })
    .unwrap_or_else(|| {
        let mut sites = Vec::new();
        for name in super::symbols::recovered_sites(current) {
            declarations(&mut sites, name.head, name.name.word, name.name.span);
        }
        sites
    });
    sites.sort_by_key(|site| (site.span.start, site.span.end));
    let mut data = Vec::new();
    let mut previous_end = 0;
    let mut previous_line = 0;
    let mut previous_start = 0;
    for site in sites {
        let Some(token_type) = palette.indices[site.kind as usize] else {
            continue;
        };
        if site.span.start < previous_end {
            continue;
        }
        let Some(range) = wire_range(&lines, text.as_bytes(), &site.span, encoding) else {
            continue;
        };
        if range.start.line != range.end.line || range.end.character <= range.start.character {
            continue;
        }
        let delta_line = range.start.line - previous_line;
        let delta_start = if delta_line == 0 {
            range.start.character - previous_start
        } else {
            range.start.character
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length: range.end.character - range.start.character,
            token_type,
            token_modifiers_bitset: u32::from(site.declaration && palette.declarations),
        });
        previous_end = site.span.end;
        previous_line = range.start.line;
        previous_start = range.start.character;
    }
    Some(SemanticTokens {
        result_id: None,
        data,
    })
}
