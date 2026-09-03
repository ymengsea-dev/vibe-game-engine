//! A small render graph: passes declared with the named resources they
//! read/write, topologically ordered and run within one shared command
//! encoder — instead of a fixed function hand-threading each new pass
//! into exactly the right spot.
//!
//! Deliberately right-sized, not a full Bevy/Unity-SRP-style typed-
//! resource-handle graph with automatic barriers/pooling — that's more
//! infrastructure than a solo engine at this scale needs yet. Resources
//! are plain string labels (`"shadow_map"`, `"hdr_target"`,
//! `"swapchain"`), enough to order passes correctly without a virtual
//! resource-lifetime system. [`GpuContext::render_scene`] is the first
//! (and so far only) consumer: a shadow/scene/post-process sequence,
//! expressed as three declared passes instead of hardcoded order — the
//! third node was a plain tonemap pass until Stage 5 dropped the whole
//! post-processing stack (bloom, color grade, toon outline) into it,
//! declaring the same read/write, no rewrite of that function — exactly
//! the composability this graph was added for.
//!
//! The ordering logic ([`resolve_execution_order`]) is pure — plain
//! `{name, reads, writes}` data, no closures, no `wgpu` — so it's
//! unit-tested directly; actual pass execution needs a real
//! [`wgpu::CommandEncoder`] (a real GPU device), so — like every other
//! pipeline/pass-execution code in this crate — it isn't.

use std::collections::{HashMap, VecDeque};

use crate::error::RendererError;

/// One node's declared name and dependencies, decoupled from its
/// `execute` payload so [`resolve_execution_order`] can be pure and
/// GPU-free.
#[derive(Debug, Clone, Copy)]
struct PassDependencies<'a> {
    name: &'a str,
    reads: &'a [&'a str],
    writes: &'a [&'a str],
}

/// Resolves an execution order for `passes`: a pass that writes a
/// resource always runs before any pass that reads it. A resource no
/// pass writes (e.g. the swapchain view, acquired outside any pass) is
/// treated as an always-available external input, not an error.
///
/// Ties (passes with no dependency relationship to each other) are
/// resolved in declaration order — deterministic, and keeps independent
/// passes in the order they were added rather than an arbitrary one.
///
/// Kahn's algorithm: O(passes + total reads/writes), not O(passes²) —
/// each pass and each read/write edge is visited once.
///
/// # Errors
///
/// Returns [`RendererError::RenderGraphCycle`] (naming one pass in the
/// cycle) if two or more passes' declared reads/writes form a cycle.
fn resolve_execution_order(passes: &[PassDependencies<'_>]) -> Result<Vec<usize>, RendererError> {
    let count = passes.len();

    let mut writers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, pass) in passes.iter().enumerate() {
        for &resource in pass.writes {
            writers.entry(resource).or_default().push(index);
        }
    }

    let mut in_degree = vec![0usize; count];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (reader_index, pass) in passes.iter().enumerate() {
        for &resource in pass.reads {
            let Some(writer_indices) = writers.get(resource) else {
                continue;
            };
            for &writer_index in writer_indices {
                // A pass reading and writing the same resource doesn't
                // depend on itself.
                if writer_index != reader_index {
                    dependents[writer_index].push(reader_index);
                    in_degree[reader_index] += 1;
                }
            }
        }
    }

    let mut ready: VecDeque<usize> = (0..count).filter(|&index| in_degree[index] == 0).collect();
    let mut order = Vec::with_capacity(count);
    while let Some(index) = ready.pop_front() {
        order.push(index);
        for &next in &dependents[index] {
            in_degree[next] -= 1;
            if in_degree[next] == 0 {
                ready.push_back(next);
            }
        }
    }

    if order.len() != count {
        // Any pass whose in-degree never reached zero is part of a cycle.
        let cyclic_pass = (0..count)
            .find(|&index| in_degree[index] != 0)
            .map(|index| passes[index].name.to_string())
            .unwrap_or_default();
        return Err(RendererError::RenderGraphCycle { pass: cyclic_pass });
    }

    Ok(order)
}

/// Reorders `items` according to `order` (a permutation of
/// `0..items.len()`, as [`resolve_execution_order`] produces) — pulled
/// out as a pure, generic step so [`RenderGraph::execute`]'s indexing
/// logic is unit-testable without a real `wgpu::CommandEncoder`.
fn reorder<T>(items: Vec<T>, order: &[usize]) -> Vec<T> {
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    order
        .iter()
        .filter_map(|&index| slots[index].take())
        .collect()
}

struct Pass<'a> {
    name: &'a str,
    reads: &'a [&'a str],
    writes: &'a [&'a str],
    execute: Box<dyn FnOnce(&mut wgpu::CommandEncoder) + 'a>,
}

/// A graph of GPU rendering passes, ordered by their declared resource
/// dependencies rather than a hardcoded sequence. See the module docs.
#[derive(Default)]
pub struct RenderGraph<'a> {
    passes: Vec<Pass<'a>>,
}

impl<'a> RenderGraph<'a> {
    /// An empty graph.
    pub fn new() -> Self {
        Self { passes: Vec::new() }
    }

    /// Declares one pass: `name` (for error messages), the resources it
    /// `reads`/`writes`, and the `execute` closure that records its
    /// actual GPU work into the encoder [`RenderGraph::execute`] passes
    /// it. Returns `self` for chaining multiple `add_pass` calls.
    pub fn add_pass(
        &mut self,
        name: &'a str,
        reads: &'a [&'a str],
        writes: &'a [&'a str],
        execute: impl FnOnce(&mut wgpu::CommandEncoder) + 'a,
    ) -> &mut Self {
        self.passes.push(Pass {
            name,
            reads,
            writes,
            execute: Box::new(execute),
        });
        self
    }

    /// Resolves execution order (topologically, by declared reads/writes
    /// — see the module docs) and runs each pass's `execute` into
    /// `encoder` in that order — one
    /// shared command buffer, the same as calling each pass's GPU work
    /// inline in the right order by hand.
    ///
    /// # Errors
    ///
    /// Returns [`RendererError::RenderGraphCycle`] if the declared passes
    /// form a cycle — nothing runs in that case (checked before any pass
    /// executes, not discovered partway through).
    pub fn execute(self, encoder: &mut wgpu::CommandEncoder) -> Result<(), RendererError> {
        let dependencies: Vec<PassDependencies<'_>> = self
            .passes
            .iter()
            .map(|pass| PassDependencies {
                name: pass.name,
                reads: pass.reads,
                writes: pass.writes,
            })
            .collect();
        let order = resolve_execution_order(&dependencies)?;

        for pass in reorder(self.passes, &order) {
            (pass.execute)(encoder);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deps<'a>(
        name: &'a str,
        reads: &'a [&'a str],
        writes: &'a [&'a str],
    ) -> PassDependencies<'a> {
        PassDependencies {
            name,
            reads,
            writes,
        }
    }

    fn names<'a>(order: &[usize], passes: &[PassDependencies<'a>]) -> Vec<&'a str> {
        order.iter().map(|&index| passes[index].name).collect()
    }

    #[test]
    fn empty_graph_resolves_to_empty_order() {
        let passes: Vec<PassDependencies<'_>> = Vec::new();
        assert_eq!(
            resolve_execution_order(&passes).unwrap(),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn single_pass_resolves_trivially() {
        let passes = [deps("a", &[], &["x"])];
        assert_eq!(resolve_execution_order(&passes).unwrap(), vec![0]);
    }

    #[test]
    fn linear_chain_resolves_in_dependency_order() {
        // c reads what b writes; b reads what a writes.
        let passes = [
            deps("c", &["y"], &["z"]),
            deps("a", &[], &["x"]),
            deps("b", &["x"], &["y"]),
        ];
        let order = resolve_execution_order(&passes).unwrap();
        assert_eq!(names(&order, &passes), vec!["a", "b", "c"]);
    }

    #[test]
    fn independent_passes_keep_declaration_order() {
        // Neither reads what the other writes — no dependency between
        // them, so declaration order (a before b) should be preserved.
        let passes = [deps("a", &[], &["x"]), deps("b", &[], &["y"])];
        let order = resolve_execution_order(&passes).unwrap();
        assert_eq!(names(&order, &passes), vec!["a", "b"]);
    }

    #[test]
    fn reading_a_resource_nothing_writes_is_not_an_error() {
        // "swapchain" is never written by any pass — an external input.
        let passes = [deps(
            "tonemap",
            &["hdr_target", "swapchain"],
            &["swapchain"],
        )];
        assert_eq!(resolve_execution_order(&passes).unwrap(), vec![0]);
    }

    #[test]
    fn reading_and_writing_the_same_resource_is_not_a_self_cycle() {
        let passes = [deps("a", &["x"], &["x"])];
        assert_eq!(resolve_execution_order(&passes).unwrap(), vec![0]);
    }

    #[test]
    fn two_pass_cycle_is_detected() {
        let passes = [deps("a", &["y"], &["x"]), deps("b", &["x"], &["y"])];
        let err = resolve_execution_order(&passes).unwrap_err();
        assert!(matches!(err, RendererError::RenderGraphCycle { .. }));
    }

    #[test]
    fn three_pass_cycle_is_detected() {
        let passes = [
            deps("a", &["z"], &["x"]),
            deps("b", &["x"], &["y"]),
            deps("c", &["y"], &["z"]),
        ];
        let err = resolve_execution_order(&passes).unwrap_err();
        assert!(matches!(err, RendererError::RenderGraphCycle { .. }));
    }

    #[test]
    fn diamond_dependency_resolves_with_shared_source_first_and_sink_last() {
        // a writes x; b and c both read x and write their own outputs;
        // d reads both b's and c's outputs.
        let passes = [
            deps("a", &[], &["x"]),
            deps("b", &["x"], &["y"]),
            deps("c", &["x"], &["z"]),
            deps("d", &["y", "z"], &["w"]),
        ];
        let order = resolve_execution_order(&passes).unwrap();
        let ordered_names = names(&order, &passes);
        assert_eq!(ordered_names[0], "a");
        assert_eq!(ordered_names[3], "d");
        assert!(ordered_names.contains(&"b"));
        assert!(ordered_names.contains(&"c"));
    }

    #[test]
    fn multiple_writers_of_the_same_resource_all_precede_its_reader() {
        let passes = [
            deps("a", &[], &["x"]),
            deps("b", &[], &["x"]),
            deps("c", &["x"], &[]),
        ];
        let order = resolve_execution_order(&passes).unwrap();
        let ordered_names = names(&order, &passes);
        let c_position = ordered_names.iter().position(|&n| n == "c").unwrap();
        assert!(c_position > ordered_names.iter().position(|&n| n == "a").unwrap());
        assert!(c_position > ordered_names.iter().position(|&n| n == "b").unwrap());
    }

    #[test]
    fn reorder_applies_the_given_permutation() {
        let items = vec!["a", "b", "c"];
        assert_eq!(reorder(items, &[2, 0, 1]), vec!["c", "a", "b"]);
    }

    #[test]
    fn reorder_of_empty_items_is_empty() {
        let items: Vec<&str> = Vec::new();
        assert_eq!(reorder(items, &[]), Vec::<&str>::new());
    }

    #[test]
    fn render_scene_pass_shape_resolves_shadow_then_scene_then_post() {
        // The exact three passes `GpuContext::render_scene` declares, in a
        // deliberately scrambled order — the resolver must still produce
        // shadow -> scene -> post_process, since each reads the previous
        // one's output. Locks the Stage 5 post-processing node into place
        // behind the scene pass without needing a GPU.
        let passes = [
            deps("post_process", &["hdr_target"], &["swapchain"]),
            deps("scene", &["shadow_map"], &["hdr_target"]),
            deps("shadow", &[], &["shadow_map"]),
        ];
        let order = resolve_execution_order(&passes).unwrap();
        assert_eq!(
            names(&order, &passes),
            vec!["shadow", "scene", "post_process"]
        );
    }

    #[test]
    fn reorder_composed_with_resolve_execution_order_runs_dependency_first() {
        // The exact composition `RenderGraph::execute` uses internally:
        // resolve the order, then reorder the declared items by it — the
        // full non-`wgpu` half of `execute`'s logic, end to end.
        let passes = [deps("b", &["x"], &[]), deps("a", &[], &["x"])];
        let order = resolve_execution_order(&passes).unwrap();
        let reordered = reorder(vec!["b", "a"], &order);
        assert_eq!(reordered, vec!["a", "b"]);
    }
}
