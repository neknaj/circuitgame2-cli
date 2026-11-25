//! 修復のある文書で、同じ前後の構文と回路診断を再利用する。

use std::ops::Range;

use circuitgame2_core::{
    editor::PositionEncoding,
    syntax::{
        OwnedRecoveryCache, SnapshotRecovery,
        circuit::{CachedCircuitRecovery, CircuitRecovery, relocate_circuit_span},
    },
};
use lsp_types::{Diagnostic, Uri};

use super::{Report, original_reports, render, repair};

pub(crate) struct LocalRecovery {
    cache: OwnedRecoveryCache,
    old_join: usize,
    head: Vec<Report>,
    tail: Vec<Report>,
}

impl LocalRecovery {
    pub(crate) fn remember(
        old: &CachedCircuitRecovery,
        revision: u64,
        edit: Range<usize>,
    ) -> Option<Self> {
        // 回路診断の採用条件が同じ場合に前後の診断を再利用する。
        let CircuitRecovery::WithSyntaxRepairs { original, .. } = old.recovery() else {
            return None;
        };
        let window = old.recovery_reuse_window(edit)?;
        let cache = OwnedRecoveryCache::remember(
            old.text().to_owned(), revision, window.start, window.join,
        ).ok()?;
        let head = original[..window.head_count]
            .iter()
            .flatten()
            .flat_map(original_reports)
            .collect();
        let tail = original[window.first_tail..]
            .iter()
            .flatten()
            .flat_map(original_reports)
            .collect();
        Some(Self {
            cache,
            old_join: window.join,
            head,
            tail,
        })
    }

    pub(crate) fn collect(
        &self,
        uri: &Uri,
        source: &str,
        revision: u64,
        encoding: PositionEncoding,
    ) -> Option<Vec<Diagnostic>> {
        let snapshot = self.cache.try_snapshot(source, revision)?;
        let SnapshotRecovery::Local(local) = snapshot.recovery() else {
            return None;
        };
        if !local.has_repairs() {
            return None;
        }
        let sections = local.sections();
        let mut reports = Vec::new();
        for index in 0..local.repair_count() {
            reports.push(repair(&local.repair_at(index)?));
        }
        reports.extend(self.head.iter().cloned());
        for index in 0..sections[1].recovered.original_lists.len() {
            if let Some(list) = local.inspect_original_list(1, index) {
                reports.extend(original_reports(&list));
            }
        }
        for report in &self.tail {
            reports.push(
                report
                    .clone()
                    .relocated(self.old_join, sections[2].byte_base)?,
            );
        }
        Some(render(uri, snapshot.source(), reports, encoding))
    }
}

impl Report {
    fn relocated(mut self, old_base: usize, new_base: usize) -> Option<Self> {
        self.span = relocate_circuit_span(&self.span, old_base, new_base)?;
        if let Some((span, _)) = &mut self.related {
            *span = relocate_circuit_span(span, old_base, new_base)?;
        }
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reused_diagnostics_match_full_recovery_for_each_utf16_revision() {
        let uri: Uri = "untitled:diagnostic-recovery.ncg2".parse().unwrap();
        let prefix = "(using nor) (using) ";
        let middle = "(using nor)";
        let suffix = " ) (test demo (reset) (case))\r\n(using)\r\n(unfinished";
        let old = CachedCircuitRecovery::analyze_with_build_index(format!("{prefix}{middle}{suffix}"));
        let local = LocalRecovery::remember(&old, 1, prefix.len()..prefix.len() + middle.len())
            .expect("変更箇所の前後に再利用できるリストがある");
        let revised = [
            "(using \"漢😀\")",
            "  ; 前置き😀\r\n(using nor)",
            "(using) (using nor)",
        ];

        for (index, middle) in revised.into_iter().enumerate() {
            let source = format!("{prefix}{middle}{suffix}");
            let from_local = local
                .collect(&uri, &source, index as u64 + 2, PositionEncoding::Utf16)
                .expect("局所再解析を適用できる");
            let full = CachedCircuitRecovery::analyze_with_build_index(source);
            let from_full = super::super::collect(&uri, &full, PositionEncoding::Utf16);
            assert_eq!(from_local, from_full, "文書版 {}", index + 2);
            assert!(
                from_local
                    .iter()
                    .any(|diagnostic| diagnostic.related_information.is_some())
            );
        }
    }
}
