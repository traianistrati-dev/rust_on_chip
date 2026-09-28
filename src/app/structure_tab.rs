//! "Structure" tab driver — caches the parsed module graph + layout, drives
//! the Phase-3 call-graph pass, and maps node/row clicks into the editor.
//!
//! The graph is rebuilt only when the project content changes (hash over every
//! file's path + text), so keeping the tab open costs one hash pass per frame
//! and nothing else. The graph itself is parse-based (no LSP); only the
//! OPTIONAL call-edge pass talks to rust-analyzer, under strict discipline —
//! see `structure_map::calls` for the rules (serialized, no did_change, sync-
//! gated, dedicated reply channel).

use super::{AppIde, ProjectFileId};
use crate::panels::structure_map::{calls, gui, layout, parse};
use eframe::egui;

impl AppIde {
    /// Render the Structure tab (called from the MCU-panel tab dispatch).
    pub(super) fn show_structure_tab(&mut self, ui: &mut egui::Ui) {
        // ── Rebuild the graph when the project content changed ────────────
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.generated_code.hash(&mut h);
            // The manifest decides which local crates the firmware reaches
            // (`CrateLinks`) and which are detached - editing a dependency
            // line changes the picture without touching a `.rs` file.
            self.cargo_toml.hash(&mut h);
            // And what the RUNNING analyzer loaded: after a restart that
            // picks up a detached library, its verdict must change too.
            self.lsp_state.lock().unwrap().linked_projects.hash(&mut h);
            for (rel, content) in &self.project_tree.user_src_files {
                rel.hash(&mut h);
                content.hash(&mut h);
            }
            // The externals toggle changes the node set (ghost nodes appended),
            // so it participates in the cache key — toggling rebuilds
            // instantly (parse is cheap) and restarts the call pass (node
            // indices shift).
            // An OPEN node is taller, so the geometry - and therefore the
            // cached layout - depends on which nodes are open, exactly as it
            // depends on the externals toggle.
            for n in &self.structure_view.expanded {
                n.hash(&mut h);
            }
            h.finish()
                ^ if self.structure_view.show_externals {
                    0x9E37_79B9_7F4A_7C15
                } else {
                    0
                }
        };
        if self.structure_cache.as_ref().map(|(h, _, _)| *h) != Some(hash) {
            // The LIBRARIES panel's own predicate, so the amber means the same
            // thing in both places.
            let members = self.built_lib_dirs();
            let files = &self.project_tree.user_src_files;
            // Read from the manifests, not guessed from folder names: a
            // detached library named like a crates.io dependency is neither
            // what `main.rs` calls nor a reason to hide that dependency.
            let links = parse::CrateLinks::from_manifests(&self.cargo_toml, files);
            let mut graph = parse::build_graph_with(&self.generated_code, files, &links);
            if self.structure_view.show_externals {
                parse::add_external_nodes_with(&mut graph, &self.generated_code, files, &links);
            }
            let ra_linked = self.lsp_state.lock().unwrap().linked_projects.clone();
            let detached: Vec<parse::DetachedLib> =
                crate::project_tree::extract_crate::detached_libs(files, &members)
                    .into_iter()
                    .map(|dir| {
                        let manifest = files
                            .iter()
                            .find(|(p, _)| *p == format!("{dir}/Cargo.toml"))
                            .map(|(_, c)| c.as_str())
                            .unwrap_or("");
                        // Traced means the RUNNING analyzer loaded it - not that
                        // the manifest would let the next one. `linkedProjects`
                        // is fixed when the analyzer starts.
                        let untraced = if crate::lsp::ra_links_detached(&ra_linked, &dir) {
                            None
                        } else if crate::lsp::cargo_can_load_detached(
                            &self.cargo_toml,
                            &dir,
                            manifest,
                        ) {
                            Some(parse::Untraced::NeedsRestart)
                        } else {
                            Some(parse::Untraced::Refused)
                        };
                        parse::DetachedLib { dir, untraced }
                    })
                    .collect();
            parse::mark_detached(&mut graph, &detached);
            let mut lay =
                layout::layout_with_calls_expanded(&graph, &[], &self.structure_view.expanded);
            layout::apply_overrides(&mut lay, &graph, &self.structure_overrides);
            self.structure_cache = Some((hash, graph, lay));
            self.structure_layout_calls = 0; // fresh layout knows no call edges
        }

        // ── Drive the call-graph pass (only while the tab is open) ─────────
        let calls_status = self.tick_structure_calls(hash);

        // ── Re-layout once the call pass settles ───────────────────────────
        // The initial layout only knows module edges; when the finished pass
        // contributes call pairs, one re-layout lets the ordering + transpose
        // minimize call-edge crossings too (see `layout_with_calls`).
        if let Some(pass) = &self.structure_calls {
            if pass.hash == hash && !pass.running() {
                let pairs: std::collections::BTreeSet<(usize, usize)> = pass
                    .edges
                    .iter()
                    .map(|e| (e.from_node, e.to_node))
                    .collect();
                if pairs.len() != self.structure_layout_calls {
                    // Cloned before the cache is borrowed mutably: the layout
                    // needs the open set, and `self` cannot be borrowed twice.
                    let expanded = self.structure_view.expanded.clone();
                    if let Some((_, graph, lay)) = self.structure_cache.as_mut() {
                        let pairs: Vec<(usize, usize)> = pairs.into_iter().collect();
                        *lay = layout::layout_with_calls_expanded(graph, &pairs, &expanded);
                        layout::apply_overrides(lay, graph, &self.structure_overrides);
                        self.structure_layout_calls = pairs.len();
                    }
                }
            }
        }
        // Keep frames coming while the pass works or waits for a save — LSP
        // replies repaint on arrival, but the NEXT request fires from here.
        if self.structure_calls.as_ref().is_some_and(|p| p.running()) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }

        // ── Draw + handle clicks / drags ───────────────────────────────────
        let Some((_, graph, lay)) = self.structure_cache.as_mut() else {
            return;
        };
        let call_edges: &[calls::CallEdge] = self
            .structure_calls
            .as_ref()
            .map(|p| p.edges.as_slice())
            .unwrap_or(&[]);
        // A module's own internal calls: the flow an EXPANDED node draws.
        let inner_edges: &[calls::CallEdge] = self
            .structure_calls
            .as_ref()
            .map(|p| p.inner_edges.as_slice())
            .unwrap_or(&[]);
        // Focused module for the call-edge filter = the currently selected
        // file's node (main by default — also for config files like
        // Cargo.toml, which have no node). Clicking a diagram node opens its
        // file, so the focus follows node clicks too.
        let focus_node = match self.selected_file {
            ProjectFileId::UserFile(i) => graph
                .nodes
                .iter()
                .position(|n| n.file == Some(i))
                .unwrap_or(0),
            _ => 0, // main.rs / config files → main
        };
        // A detached library rust-analyzer cannot load answers no reference
        // search, so its modules would simply show no calls, with nothing to
        // say why. The module's hover carries the one-line fix.
        let calls_status = match graph.nodes.get(focus_node).and_then(|n| n.untraced) {
            Some(parse::Untraced::NeedsRestart) => format!(
                "{calls_status}  \u{b7}  no calls traced yet: restart the analyzer to load this detached library"
            ),
            Some(parse::Untraced::Refused) => format!(
                "{calls_status}  \u{b7}  no calls traced: rust-analyzer cannot load this detached library (hover it)"
            ),
            None => calls_status,
        };
        // Per-node error flags (rust-analyzer + flycheck diagnostics, keyed by
        // the workspace-relative path) — nodes with errors blink a red border.
        let node_errors: Vec<bool> = {
            let lsp = self.lsp_state.lock().unwrap();
            graph
                .nodes
                .iter()
                .map(|n| lsp.error_count_for(&n.file_rel) > 0)
                .collect()
        };
        // Reference counts per symbol row (shown right-aligned in the rows) —
        // many sites aggregate into one edge, so the count keeps the total
        // visible (matches the editor's "N refs" pill).
        let empty_counts = std::collections::HashMap::new();
        let ref_counts = self
            .structure_calls
            .as_ref()
            .map(|p| &p.ref_counts)
            .unwrap_or(&empty_counts);
        let empty_pairs = std::collections::HashMap::new();
        let pair_counts = self
            .structure_calls
            .as_ref()
            .map(|p| &p.pair_counts)
            .unwrap_or(&empty_pairs);
        let result = gui::show(
            ui,
            &*graph,
            lay,
            &mut self.structure_view,
            call_edges,
            inner_edges,
            &calls_status,
            focus_node,
            &node_errors,
            ref_counts,
            pair_counts,
        );

        // A header drag ended → pin that node's position (keyed by its file,
        // which survives graph rebuilds). Saved with the project (mcu.config).
        if let Some(i) = result.moved {
            if let Some(node) = graph.nodes.get(i) {
                self.structure_overrides
                    .insert(node.file_rel.clone(), (lay.pos[i].x, lay.pos[i].y));
            }
        }

        // "Auto layout" → drop every pin and re-run the automatic arrangement
        // (with the current call pairs, when the pass has delivered them).
        if result.reset_layout {
            self.structure_overrides.clear();
            let pairs: Vec<(usize, usize)> = self
                .structure_calls
                .as_ref()
                .filter(|p| p.hash == hash && !p.running())
                .map(|p| {
                    let set: std::collections::BTreeSet<(usize, usize)> =
                        p.edges.iter().map(|e| (e.from_node, e.to_node)).collect();
                    set.into_iter().collect()
                })
                .unwrap_or_default();
            *lay =
                layout::layout_with_calls_expanded(&*graph, &pairs, &self.structure_view.expanded);
            self.structure_layout_calls = pairs.len();
        }

        if let Some(click) = result.click {
            let id = match click.file {
                None => ProjectFileId::MainRs,
                // Guard against a stale index (file list changed this frame).
                Some(i) if i < self.project_tree.user_src_files.len() => ProjectFileId::UserFile(i),
                Some(_) => return,
            };
            self.selected_file = id;
            // A symbol-row click also jumps to the item's line (same scroll +
            // highlight path the usages popup and F12 navigation use).
            if let Some(line) = click.line {
                self.ed.pending_scroll_to_line = Some((id, line));
                self.ed.highlighted_def_line = Some((id, line));
            } else if let Some(line) = self.first_error_line(&click.file_rel) {
                // Clicking the node itself (not a symbol row): a node marked
                // with the error border is clicked BECAUSE of that error, so
                // land on it instead of at the top of the file.
                //
                // The band MUST come from `diag_highlight_color` — it is a
                // translucent wash (alpha 26) painted OVER the text. A solid
                // colour here hid the very line it was pointing at.
                self.ed.pending_scroll_to_line = Some((id, line));
                self.ed.highlighted_error_line = Some((
                    id,
                    line,
                    crate::app::diag_highlight_color(crate::lsp::DiagSeverity::Error),
                ));
            }
        }
    }

    /// 1-based line of the FIRST error in `rel` (project-root-relative), or
    /// `None` when the file has none. Errors only - a warning is not what the
    /// red marker is pointing at, and jumping to one would be a lie.
    ///
    /// Rust-analyzer WINS whenever it has anything to say; cargo is only a
    /// fallback. Deliberately not the earliest line of the two: the cargo
    /// `BuildResult` is a snapshot from the last check, so after fixing line 10
    /// and introducing line 200 an earliest-of-both rule would land on the
    /// clean line 10 and wash it as an error. Rust-analyzer's map is live, and
    /// is also what paints the squiggle you see on arrival - preferring it
    /// keeps the jump and the marker agreeing.
    ///
    /// Cargo still matters as a fallback: the project tree badges a row on
    /// `build_result.has_errors_in(rel) || lsp.error_count_for(rel) > 0`, so an
    /// LSP-only lookup left a cargo-flagged row jumping nowhere. Both sources
    /// number lines from 1 (`LspDiagnostic.line` is converted from LSP's 0-based
    /// on the way in; rustc's JSON is already 1-based).
    ///
    /// The two locks are taken and released one at a time - never held together
    /// - so this cannot deadlock against code that locks them the other way.
    pub(super) fn first_error_line(&self, rel: &str) -> Option<usize> {
        let from_lsp = {
            let lsp = self.lsp_state.lock().unwrap();
            lsp.diagnostics.get(rel).and_then(|ds| {
                ds.iter()
                    .filter(|d| d.severity.is_error())
                    .map(|d| d.line as usize)
                    .min()
            })
        };
        if from_lsp.is_some() {
            return from_lsp;
        }
        let build = self.build_state.lock().unwrap();
        build.result().and_then(|r| {
            r.for_file(rel)
                .iter()
                .filter(|d| d.is_error())
                // A rustc diagnostic can carry a file with no primary span
                // (a link error, say). It still badges the row; it just has
                // nowhere to jump to, and the caller degrades to a plain
                // selection rather than inventing a line.
                .filter_map(|d| d.line.map(|l| l as usize))
                .min()
        })
    }

    /// One step of the call-graph pass: receive the in-flight reply, then fire
    /// the next symbol's references lookup. Returns the toolbar status text.
    ///
    /// Discipline (the rules that keep saves fast and Ctrl+Enter alive):
    /// NO did_change — a symbol is queried only while RA already holds its
    /// file's current text; one request in flight, and none while the usages
    /// pass runs its own search (`references_busy`).
    fn tick_structure_calls(&mut self, hash: u64) -> String {
        let Some((_, graph, _)) = &self.structure_cache else {
            return String::new();
        };
        if !self.structure_view.show_calls {
            return String::new(); // toggle off → don't spend requests
        }
        // (Re)start the pass when the content hash moved.
        if self.structure_calls.as_ref().map(|p| p.hash) != Some(hash) {
            let pass = calls::CallPass::new(graph, hash);
            crate::lsp::debug_log(&format!(
                "CALLS_PASS start total={} hash={hash:x}",
                pass.total
            ));
            self.structure_calls = Some(pass);
        }
        let pass = self.structure_calls.as_mut().unwrap();

        let mut lsp = self.lsp_state.lock().unwrap();

        // 0) A rust-analyzer restart clears the reply channel, so a request
        //    that was in flight can never be answered. Re-queue it rather than
        //    waiting for a reply that is not coming - without this the pass sat
        //    on "analyzing calls N/total..." until an unrelated edit rebuilt it.
        if pass.abandon_if_restarted(lsp.generation) {
            pass.log_once("CALLS_ABANDONED analyzer restarted mid-request".to_owned());
        }

        // 1) Receive the completed lookup (stale keys from a superseded pass
        //    are simply dropped by the key match).
        for (key, locs) in lsp.take_calls_reference_results() {
            if pass.in_flight.map(|(k, _, _)| k) == Some(key) {
                let (_, node, row) = pass.in_flight.take().unwrap();
                pass.add_references(graph, node, row, &locs);
                pass.log_once(format!(
                    "CALLS_RESP key={key} sites={} done={}/{} edges={}",
                    locs.len(),
                    pass.done,
                    pass.total,
                    pass.edges.len()
                ));
            }
        }

        // 2) Fire the next lookup — scanning the WHOLE queue for the first
        //    fireable symbol, so one open-but-edited file can't freeze the
        //    rest of the pass behind it (head-of-line blocking).
        let mut blocked: Option<&'static str> = None;
        if pass.in_flight.is_none() && !pass.queue.is_empty() {
            if !matches!(lsp.status, crate::lsp::LspStatus::Ready) {
                blocked = Some("waiting for rust-analyzer…");
            } else if lsp.references_busy() {
                blocked = Some("waiting for the usages pass…");
            } else {
                // Sync gate per symbol: RA must hold THIS text, or the symbol
                // positions (and the reply's site lines) would be stale. Never
                // a did_change (version bumps cancel other requests — they do
                // NOT re-trigger flycheck, which only a `did_save` starts) —
                // but a file RA hasn't opened AT ALL
                // (fresh app start: docs only open on Save/completion) is
                // seeded with did_open: a FIRST open bumps nothing, cancels
                // nothing, runs no flycheck. Open-but-EDITED files wait for
                // the next Project Save (and are skipped over meanwhile).
                let mut fired = false;
                for qi in 0..pass.queue.len() {
                    let (node_i, row) = pass.queue[qi];
                    let node = &graph.nodes[node_i];
                    let content: &str = match node.file {
                        None => &self.generated_code,
                        Some(i) => &self.project_tree.user_src_files[i].1,
                    };
                    let rel = node.file_rel.clone();
                    let synced = lsp.last_sent_matches(&rel, content);
                    let seed_open = !synced && !lsp.is_file_open(&rel);
                    if !(synced || seed_open) {
                        continue; // open but edited — retry after the next save
                    }
                    if seed_open {
                        lsp.did_open(&rel, content);
                    }
                    pass.queue.remove(qi);
                    let sym = &node.symbols[row];
                    let key = pass.take_key();
                    pass.log_once(format!(
                        "CALLS_REQ key={key} file={rel} line={} col={} sym={} seed={seed_open}",
                        sym.line - 1,
                        sym.col,
                        sym.name
                    ));
                    lsp.request_references_for_calls(
                        &rel,
                        (sym.line - 1) as u32,
                        sym.col as u32,
                        key,
                    );
                    pass.in_flight = Some((key, node_i, row));
                    pass.sent_at_generation = lsp.generation;
                    pass.waiting_sync = false;
                    fired = true;
                    break;
                }
                if !fired {
                    pass.waiting_sync = true;
                }
            }
        }

        // 3) Toolbar status — always says WHY nothing is moving, and never
        //    claims a completeness it does not have: a pass that hit the symbol
        //    cap says so in every state, because "512 call edges" on a
        //    truncated search reads as the whole answer.
        let capped = if pass.skipped == 0 {
            String::new()
        } else {
            format!(
                "  ·  {} symbol(s) past the {} cap not searched — some edges are missing",
                pass.skipped,
                crate::panels::structure_map::calls::MAX_SYMBOLS
            )
        };
        let status = if pass.running() {
            if let Some(b) = blocked {
                b.to_owned()
            } else if pass.waiting_sync {
                "unsaved changes — Save the project to update the call graph".to_owned()
            } else {
                format!("analyzing calls {}/{}…{capped}", pass.done, pass.total)
            }
        } else if !pass.edges.is_empty() {
            format!("{} call edges{capped}", pass.edges.len())
        } else if pass.skipped > 0 {
            // Nothing found AND the search was cut short: saying only "none
            // found" here would blame the code for the cap's omission.
            format!(
                "no cross-module calls found in the first {}{capped}",
                pass.total
            )
        } else {
            // Distinguish "finished, none found" from "not running" — an
            // empty diagram with silent status made failures undiagnosable.
            "no cross-module calls found".to_owned()
        };
        // Runs on EVERY frame the Structure tab is open, so don't build the
        // line (or the String it needs) unless something will read it.
        if crate::lsp::log_enabled() {
            pass.log_once(format!("CALLS_STATE {status}"));
        }
        status
    }
}
