//! Dependency resolution for Recipe package manager
//!
//! Implements topological sort with cycle detection, inspired by pacman/libalpm.
//! Uses iterative DFS with state tracking to avoid stack overflow on deep graphs.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::recipe_state;

/// Node state for DFS traversal
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeState {
    /// Not yet visited
    Unprocessed,
    /// Currently being processed (on the stack)
    Processing,
    /// Fully processed (all dependencies resolved)
    Processed,
}

/// A dependency graph for topological sorting
pub struct DepGraph {
    /// Map from package name to its dependencies
    edges: HashMap<String, Vec<String>>,
    /// Map from package name to recipe path
    paths: HashMap<String, PathBuf>,
}

impl DepGraph {
    /// Create a new empty dependency graph
    pub fn new() -> Self {
        Self {
            edges: HashMap::new(),
            paths: HashMap::new(),
        }
    }

    /// Add a package and its dependencies to the graph
    pub fn add_package(&mut self, name: String, deps: Vec<String>, path: PathBuf) {
        self.edges.insert(name.clone(), deps);
        self.paths.insert(name, path);
    }

    /// Get the recipe path for a package
    pub fn get_path(&self, name: &str) -> Option<&PathBuf> {
        self.paths.get(name)
    }

    /// Check if a package exists in the graph
    pub fn contains(&self, name: &str) -> bool {
        self.edges.contains_key(name)
    }

    /// Perform topological sort using iterative DFS
    ///
    /// Returns packages in dependency order (dependencies before dependents).
    /// Detects cycles and returns an error if found.
    pub fn topological_sort(&self, targets: &[String]) -> Result<Vec<String>> {
        let mut state: HashMap<String, NodeState> = HashMap::new();
        let mut result: Vec<String> = Vec::new();

        // Initialize all nodes as unprocessed
        for name in self.edges.keys() {
            state.insert(name.clone(), NodeState::Unprocessed);
        }

        // Process each target
        for target in targets {
            if !self.edges.contains_key(target) {
                bail!("Package not found: {}", target);
            }
            self.dfs_visit(target.clone(), &mut state, &mut result)?;
        }

        Ok(result)
    }

    /// Iterative DFS with explicit stack to avoid recursion limits
    fn dfs_visit(
        &self,
        start: String,
        state: &mut HashMap<String, NodeState>,
        result: &mut Vec<String>,
    ) -> Result<()> {
        // Stack holds (node_name, index_of_next_child_to_visit)
        let mut stack: Vec<(String, usize)> = vec![(start, 0)];

        while let Some((node, child_idx)) = stack.pop() {
            let deps = self.edges.get(&node).cloned().unwrap_or_default();

            match state.get(&node).copied().unwrap_or(NodeState::Unprocessed) {
                NodeState::Processed => {
                    // Already fully processed, skip
                    continue;
                }
                NodeState::Processing => {
                    // Check if all children are done
                    if child_idx >= deps.len() {
                        // All children processed - finalize this node
                        state.insert(node.clone(), NodeState::Processed);
                        result.push(node);
                        continue;
                    }
                    // Otherwise continue processing remaining children below
                }
                NodeState::Unprocessed => {
                    // First visit - mark as processing
                    state.insert(node.clone(), NodeState::Processing);
                }
            }

            // Check remaining children starting from child_idx
            let mut found_unprocessed = false;
            for i in child_idx..deps.len() {
                let dep = &deps[i];
                match state.get(dep).copied().unwrap_or(NodeState::Unprocessed) {
                    NodeState::Unprocessed => {
                        // Push current node back with next index, then push child
                        stack.push((node.clone(), i + 1));
                        stack.push((dep.clone(), 0));
                        found_unprocessed = true;
                        break;
                    }
                    NodeState::Processing => {
                        // Cycle detected!
                        bail!("Dependency cycle detected: {} -> {}", node, dep);
                    }
                    NodeState::Processed => {
                        // Already processed, continue to next child
                    }
                }
            }

            if !found_unprocessed {
                // All children processed - push back to finalize
                stack.push((node, deps.len()));
            }
        }

        Ok(())
    }
}

impl Default for DepGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a dependency graph from all recipes in a directory
pub fn build_graph(recipes_path: &Path) -> Result<DepGraph> {
    let mut graph = DepGraph::new();

    if !recipes_path.exists() {
        return Ok(graph);
    }

    for entry in std::fs::read_dir(recipes_path)
        .with_context(|| format!("Failed to read recipes directory: {}", recipes_path.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map(|e| e == "rhai").unwrap_or(false) {
            // Get package name from filename
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_default();

            if name.is_empty() {
                continue;
            }

            // Get dependencies from recipe
            let deps: Vec<String> = recipe_state::get_var(&path, "deps")
                .unwrap_or(None)
                .unwrap_or_default();

            graph.add_package(name, deps, path);
        }
    }

    Ok(graph)
}

/// Resolve dependencies for a package and return install order
///
/// Returns a list of (package_name, recipe_path) in the order they should be installed.
/// The target package is last in the list.
pub fn resolve_deps(
    target: &str,
    recipes_path: &Path,
) -> Result<Vec<(String, PathBuf)>> {
    let graph = build_graph(recipes_path)?;

    if !graph.contains(target) {
        bail!("Recipe not found: {}", target);
    }

    let order = graph.topological_sort(&[target.to_string()])?;

    // Map names to paths
    let mut result = Vec::new();
    for name in order {
        let path = graph
            .get_path(&name)
            .ok_or_else(|| anyhow::anyhow!("Missing path for package: {}", name))?
            .clone();
        result.push((name, path));
    }

    Ok(result)
}

/// Check which dependencies are already installed
pub fn filter_uninstalled(deps: Vec<(String, PathBuf)>) -> Result<Vec<(String, PathBuf)>> {
    let mut uninstalled = Vec::new();

    for (name, path) in deps {
        let installed: Option<bool> = recipe_state::get_var(&path, "installed").unwrap_or(None);
        if installed != Some(true) {
            uninstalled.push((name, path));
        }
    }

    Ok(uninstalled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_recipe(dir: &Path, name: &str, deps: &[&str]) {
        let deps_str = deps
            .iter()
            .map(|d| format!("\"{}\"", d))
            .collect::<Vec<_>>()
            .join(", ");
        let content = format!(
            r#"let name = "{}";
let version = "1.0";
let deps = [{}];
"#,
            name, deps_str
        );
        std::fs::write(dir.join(format!("{}.rhai", name)), content).unwrap();
    }

    #[test]
    fn test_empty_graph() {
        let graph = DepGraph::new();
        let result = graph.topological_sort(&[]);
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_single_package_no_deps() {
        let mut graph = DepGraph::new();
        graph.add_package("foo".into(), vec![], PathBuf::from("foo.rhai"));

        let order = graph.topological_sort(&["foo".into()]).unwrap();
        assert_eq!(order, vec!["foo"]);
    }

    #[test]
    fn test_linear_deps() {
        // c -> b -> a (c depends on b, b depends on a)
        let mut graph = DepGraph::new();
        graph.add_package("a".into(), vec![], PathBuf::from("a.rhai"));
        graph.add_package("b".into(), vec!["a".into()], PathBuf::from("b.rhai"));
        graph.add_package("c".into(), vec!["b".into()], PathBuf::from("c.rhai"));

        let order = graph.topological_sort(&["c".into()]).unwrap();
        // Order should be: a, b, c
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_diamond_deps() {
        //     d
        //    / \
        //   b   c
        //    \ /
        //     a
        let mut graph = DepGraph::new();
        graph.add_package("a".into(), vec![], PathBuf::from("a.rhai"));
        graph.add_package("b".into(), vec!["a".into()], PathBuf::from("b.rhai"));
        graph.add_package("c".into(), vec!["a".into()], PathBuf::from("c.rhai"));
        graph.add_package(
            "d".into(),
            vec!["b".into(), "c".into()],
            PathBuf::from("d.rhai"),
        );

        let order = graph.topological_sort(&["d".into()]).unwrap();
        // a must come before b and c, b and c must come before d
        let a_pos = order.iter().position(|x| x == "a").unwrap();
        let b_pos = order.iter().position(|x| x == "b").unwrap();
        let c_pos = order.iter().position(|x| x == "c").unwrap();
        let d_pos = order.iter().position(|x| x == "d").unwrap();

        assert!(a_pos < b_pos);
        assert!(a_pos < c_pos);
        assert!(b_pos < d_pos);
        assert!(c_pos < d_pos);
    }

    #[test]
    fn test_cycle_detection() {
        // a -> b -> c -> a (cycle)
        let mut graph = DepGraph::new();
        graph.add_package("a".into(), vec!["c".into()], PathBuf::from("a.rhai"));
        graph.add_package("b".into(), vec!["a".into()], PathBuf::from("b.rhai"));
        graph.add_package("c".into(), vec!["b".into()], PathBuf::from("c.rhai"));

        let result = graph.topological_sort(&["a".into()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn test_self_cycle() {
        // a -> a (self-cycle)
        let mut graph = DepGraph::new();
        graph.add_package("a".into(), vec!["a".into()], PathBuf::from("a.rhai"));

        let result = graph.topological_sort(&["a".into()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn test_missing_package() {
        let graph = DepGraph::new();
        let result = graph.topological_sort(&["nonexistent".into()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_build_graph_from_recipes() {
        let dir = TempDir::new().unwrap();
        let recipes_path = dir.path();

        write_recipe(recipes_path, "app", &["lib1", "lib2"]);
        write_recipe(recipes_path, "lib1", &["core"]);
        write_recipe(recipes_path, "lib2", &["core"]);
        write_recipe(recipes_path, "core", &[]);

        let graph = build_graph(recipes_path).unwrap();

        assert!(graph.contains("app"));
        assert!(graph.contains("lib1"));
        assert!(graph.contains("lib2"));
        assert!(graph.contains("core"));

        let order = graph.topological_sort(&["app".into()]).unwrap();
        // core must come first, then lib1/lib2, then app
        let core_pos = order.iter().position(|x| x == "core").unwrap();
        let lib1_pos = order.iter().position(|x| x == "lib1").unwrap();
        let lib2_pos = order.iter().position(|x| x == "lib2").unwrap();
        let app_pos = order.iter().position(|x| x == "app").unwrap();

        assert!(core_pos < lib1_pos);
        assert!(core_pos < lib2_pos);
        assert!(lib1_pos < app_pos);
        assert!(lib2_pos < app_pos);
    }

    #[test]
    fn test_resolve_deps() {
        let dir = TempDir::new().unwrap();
        let recipes_path = dir.path();

        write_recipe(recipes_path, "app", &["lib"]);
        write_recipe(recipes_path, "lib", &[]);

        let result = resolve_deps("app", recipes_path).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "lib");
        assert_eq!(result[1].0, "app");
    }

    #[test]
    fn test_filter_uninstalled() {
        let dir = TempDir::new().unwrap();
        let recipes_path = dir.path();

        // Create installed package
        std::fs::write(
            recipes_path.join("installed.rhai"),
            r#"let name = "installed";
let version = "1.0";
let deps = [];
let installed = true;
"#,
        )
        .unwrap();

        // Create uninstalled package
        std::fs::write(
            recipes_path.join("notinstalled.rhai"),
            r#"let name = "notinstalled";
let version = "1.0";
let deps = [];
let installed = false;
"#,
        )
        .unwrap();

        let deps = vec![
            ("installed".into(), recipes_path.join("installed.rhai")),
            ("notinstalled".into(), recipes_path.join("notinstalled.rhai")),
        ];

        let uninstalled = filter_uninstalled(deps).unwrap();
        assert_eq!(uninstalled.len(), 1);
        assert_eq!(uninstalled[0].0, "notinstalled");
    }

    #[test]
    fn test_multiple_targets() {
        let mut graph = DepGraph::new();
        graph.add_package("a".into(), vec![], PathBuf::from("a.rhai"));
        graph.add_package("b".into(), vec![], PathBuf::from("b.rhai"));
        graph.add_package("c".into(), vec!["a".into()], PathBuf::from("c.rhai"));

        let order = graph
            .topological_sort(&["b".into(), "c".into()])
            .unwrap();
        // Should include a, b, c (a before c, b can be anywhere)
        assert!(order.contains(&"a".to_string()));
        assert!(order.contains(&"b".to_string()));
        assert!(order.contains(&"c".to_string()));

        let a_pos = order.iter().position(|x| x == "a").unwrap();
        let c_pos = order.iter().position(|x| x == "c").unwrap();
        assert!(a_pos < c_pos);
    }

    // ==================== Deep Dependency Chains ====================

    #[test]
    fn test_deep_chain_5_levels() {
        // e -> d -> c -> b -> a
        let mut graph = DepGraph::new();
        graph.add_package("a".into(), vec![], PathBuf::from("a.rhai"));
        graph.add_package("b".into(), vec!["a".into()], PathBuf::from("b.rhai"));
        graph.add_package("c".into(), vec!["b".into()], PathBuf::from("c.rhai"));
        graph.add_package("d".into(), vec!["c".into()], PathBuf::from("d.rhai"));
        graph.add_package("e".into(), vec!["d".into()], PathBuf::from("e.rhai"));

        let order = graph.topological_sort(&["e".into()]).unwrap();
        assert_eq!(order, vec!["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn test_deep_chain_10_levels() {
        let mut graph = DepGraph::new();
        let names: Vec<String> = (0..10).map(|i| format!("pkg{}", i)).collect();

        // pkg0 has no deps, pkg1 depends on pkg0, etc.
        graph.add_package(names[0].clone(), vec![], PathBuf::from("pkg0.rhai"));
        for i in 1..10 {
            graph.add_package(
                names[i].clone(),
                vec![names[i - 1].clone()],
                PathBuf::from(format!("pkg{}.rhai", i)),
            );
        }

        let order = graph.topological_sort(&["pkg9".into()]).unwrap();
        assert_eq!(order.len(), 10);
        // Verify order: pkg0, pkg1, ..., pkg9
        for i in 0..10 {
            assert_eq!(order[i], format!("pkg{}", i));
        }
    }

    // ==================== Wide Dependency Graphs ====================

    #[test]
    fn test_wide_deps_many_siblings() {
        // app depends on lib1, lib2, lib3, lib4, lib5
        let mut graph = DepGraph::new();
        let libs: Vec<String> = (1..=5).map(|i| format!("lib{}", i)).collect();

        for lib in &libs {
            graph.add_package(lib.clone(), vec![], PathBuf::from(format!("{}.rhai", lib)));
        }
        graph.add_package("app".into(), libs.clone(), PathBuf::from("app.rhai"));

        let order = graph.topological_sort(&["app".into()]).unwrap();
        assert_eq!(order.len(), 6);

        // All libs must come before app
        let app_pos = order.iter().position(|x| x == "app").unwrap();
        for lib in &libs {
            let lib_pos = order.iter().position(|x| x == lib).unwrap();
            assert!(lib_pos < app_pos, "{} should come before app", lib);
        }
    }

    #[test]
    fn test_wide_deps_10_siblings() {
        let mut graph = DepGraph::new();
        let deps: Vec<String> = (0..10).map(|i| format!("dep{}", i)).collect();

        for dep in &deps {
            graph.add_package(dep.clone(), vec![], PathBuf::from(format!("{}.rhai", dep)));
        }
        graph.add_package("root".into(), deps.clone(), PathBuf::from("root.rhai"));

        let order = graph.topological_sort(&["root".into()]).unwrap();
        assert_eq!(order.len(), 11);
        assert_eq!(order.last().unwrap(), "root");
    }

    // ==================== Complex Graph Patterns ====================

    #[test]
    fn test_complex_real_world_scenario() {
        // Simulates a realistic dependency tree:
        //
        //           myapp
        //          /  |  \
        //      web  db   auth
        //       |    |    |
        //      http json crypto
        //        \   |   /
        //         \  |  /
        //          core
        let mut graph = DepGraph::new();

        graph.add_package("core".into(), vec![], PathBuf::from("core.rhai"));
        graph.add_package("http".into(), vec!["core".into()], PathBuf::from("http.rhai"));
        graph.add_package("json".into(), vec!["core".into()], PathBuf::from("json.rhai"));
        graph.add_package("crypto".into(), vec!["core".into()], PathBuf::from("crypto.rhai"));
        graph.add_package("web".into(), vec!["http".into()], PathBuf::from("web.rhai"));
        graph.add_package("db".into(), vec!["json".into()], PathBuf::from("db.rhai"));
        graph.add_package("auth".into(), vec!["crypto".into()], PathBuf::from("auth.rhai"));
        graph.add_package(
            "myapp".into(),
            vec!["web".into(), "db".into(), "auth".into()],
            PathBuf::from("myapp.rhai"),
        );

        let order = graph.topological_sort(&["myapp".into()]).unwrap();
        assert_eq!(order.len(), 8);

        // Verify ordering constraints
        let pos = |name: &str| order.iter().position(|x| x == name).unwrap();

        // core must be first
        assert_eq!(pos("core"), 0);

        // http, json, crypto must come after core
        assert!(pos("http") > pos("core"));
        assert!(pos("json") > pos("core"));
        assert!(pos("crypto") > pos("core"));

        // web, db, auth must come after their deps
        assert!(pos("web") > pos("http"));
        assert!(pos("db") > pos("json"));
        assert!(pos("auth") > pos("crypto"));

        // myapp must be last
        assert_eq!(pos("myapp"), 7);
    }

    #[test]
    fn test_two_independent_trees() {
        //   tree1     tree2
        //     |         |
        //   leaf1     leaf2
        let mut graph = DepGraph::new();
        graph.add_package("leaf1".into(), vec![], PathBuf::from("leaf1.rhai"));
        graph.add_package("tree1".into(), vec!["leaf1".into()], PathBuf::from("tree1.rhai"));
        graph.add_package("leaf2".into(), vec![], PathBuf::from("leaf2.rhai"));
        graph.add_package("tree2".into(), vec!["leaf2".into()], PathBuf::from("tree2.rhai"));

        // Request both trees
        let order = graph
            .topological_sort(&["tree1".into(), "tree2".into()])
            .unwrap();
        assert_eq!(order.len(), 4);

        let pos = |name: &str| order.iter().position(|x| x == name).unwrap();
        assert!(pos("leaf1") < pos("tree1"));
        assert!(pos("leaf2") < pos("tree2"));
    }

    #[test]
    fn test_shared_deep_dependency() {
        //    a       b
        //     \     /
        //      \   /
        //       \ /
        //        c
        //        |
        //        d
        //        |
        //        e
        let mut graph = DepGraph::new();
        graph.add_package("e".into(), vec![], PathBuf::from("e.rhai"));
        graph.add_package("d".into(), vec!["e".into()], PathBuf::from("d.rhai"));
        graph.add_package("c".into(), vec!["d".into()], PathBuf::from("c.rhai"));
        graph.add_package("a".into(), vec!["c".into()], PathBuf::from("a.rhai"));
        graph.add_package("b".into(), vec!["c".into()], PathBuf::from("b.rhai"));

        let order = graph.topological_sort(&["a".into(), "b".into()]).unwrap();
        assert_eq!(order.len(), 5);

        let pos = |name: &str| order.iter().position(|x| x == name).unwrap();
        // e -> d -> c must be in order
        assert!(pos("e") < pos("d"));
        assert!(pos("d") < pos("c"));
        // c must come before both a and b
        assert!(pos("c") < pos("a"));
        assert!(pos("c") < pos("b"));
    }

    // ==================== Cycle Detection Edge Cases ====================

    #[test]
    fn test_cycle_in_middle_of_chain() {
        // a -> b -> c -> d -> b (cycle at b)
        let mut graph = DepGraph::new();
        graph.add_package("a".into(), vec!["b".into()], PathBuf::from("a.rhai"));
        graph.add_package("b".into(), vec!["c".into()], PathBuf::from("b.rhai"));
        graph.add_package("c".into(), vec!["d".into()], PathBuf::from("c.rhai"));
        graph.add_package("d".into(), vec!["b".into()], PathBuf::from("d.rhai"));

        let result = graph.topological_sort(&["a".into()]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cycle"), "Expected cycle error, got: {}", err);
    }

    #[test]
    fn test_two_node_cycle() {
        // a <-> b
        let mut graph = DepGraph::new();
        graph.add_package("a".into(), vec!["b".into()], PathBuf::from("a.rhai"));
        graph.add_package("b".into(), vec!["a".into()], PathBuf::from("b.rhai"));

        let result = graph.topological_sort(&["a".into()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn test_cycle_not_involving_target() {
        // target -> a, but b <-> c form a cycle (not reachable from target)
        let mut graph = DepGraph::new();
        graph.add_package("a".into(), vec![], PathBuf::from("a.rhai"));
        graph.add_package("target".into(), vec!["a".into()], PathBuf::from("target.rhai"));
        graph.add_package("b".into(), vec!["c".into()], PathBuf::from("b.rhai"));
        graph.add_package("c".into(), vec!["b".into()], PathBuf::from("c.rhai"));

        // Should succeed since cycle is not reachable
        let order = graph.topological_sort(&["target".into()]).unwrap();
        assert_eq!(order, vec!["a", "target"]);
    }

    // ==================== Missing Dependencies ====================

    #[test]
    fn test_missing_dependency_in_chain() {
        // a -> b -> missing
        let mut graph = DepGraph::new();
        graph.add_package("a".into(), vec!["b".into()], PathBuf::from("a.rhai"));
        graph.add_package("b".into(), vec!["missing".into()], PathBuf::from("b.rhai"));
        // "missing" is not added to graph

        let order = graph.topological_sort(&["a".into()]).unwrap();
        // Should include a and b, but "missing" is just ignored since it's not in graph
        // (it's referenced but not defined - this is allowed for flexibility)
        assert!(order.contains(&"a".to_string()));
        assert!(order.contains(&"b".to_string()));
    }

    #[test]
    fn test_resolve_deps_missing_recipe() {
        let dir = TempDir::new().unwrap();
        let result = resolve_deps("nonexistent", dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    // ==================== Duplicate Dependencies ====================

    #[test]
    fn test_duplicate_deps_in_array() {
        // a depends on [b, b, b] (duplicates)
        let mut graph = DepGraph::new();
        graph.add_package("b".into(), vec![], PathBuf::from("b.rhai"));
        graph.add_package(
            "a".into(),
            vec!["b".into(), "b".into(), "b".into()],
            PathBuf::from("a.rhai"),
        );

        let order = graph.topological_sort(&["a".into()]).unwrap();
        // Should handle duplicates gracefully
        assert_eq!(order.len(), 2);
        assert_eq!(order, vec!["b", "a"]);
    }

    #[test]
    fn test_same_dep_multiple_paths() {
        //      root
        //     / | \
        //    a  b  c
        //     \ | /
        //      dep
        let mut graph = DepGraph::new();
        graph.add_package("dep".into(), vec![], PathBuf::from("dep.rhai"));
        graph.add_package("a".into(), vec!["dep".into()], PathBuf::from("a.rhai"));
        graph.add_package("b".into(), vec!["dep".into()], PathBuf::from("b.rhai"));
        graph.add_package("c".into(), vec!["dep".into()], PathBuf::from("c.rhai"));
        graph.add_package(
            "root".into(),
            vec!["a".into(), "b".into(), "c".into()],
            PathBuf::from("root.rhai"),
        );

        let order = graph.topological_sort(&["root".into()]).unwrap();
        assert_eq!(order.len(), 5);

        // dep must appear exactly once and before a, b, c
        assert_eq!(order.iter().filter(|x| *x == "dep").count(), 1);
        let dep_pos = order.iter().position(|x| x == "dep").unwrap();
        assert_eq!(dep_pos, 0);
    }

    // ==================== Graph Building Edge Cases ====================

    #[test]
    fn test_build_graph_empty_directory() {
        let dir = TempDir::new().unwrap();
        let graph = build_graph(dir.path()).unwrap();
        assert!(!graph.contains("anything"));
    }

    #[test]
    fn test_build_graph_ignores_non_rhai_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("readme.md"), "# README").unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        write_recipe(dir.path(), "valid", &[]);

        let graph = build_graph(dir.path()).unwrap();
        assert!(graph.contains("valid"));
        assert!(!graph.contains("readme"));
        assert!(!graph.contains("config"));
    }

    #[test]
    fn test_build_graph_nonexistent_directory() {
        let graph = build_graph(Path::new("/nonexistent/path")).unwrap();
        assert!(!graph.contains("anything"));
    }

    #[test]
    fn test_build_graph_recipe_without_deps() {
        let dir = TempDir::new().unwrap();
        // Recipe with no deps field at all
        std::fs::write(
            dir.path().join("nodeps.rhai"),
            r#"let name = "nodeps";
let version = "1.0";
"#,
        )
        .unwrap();

        let graph = build_graph(dir.path()).unwrap();
        assert!(graph.contains("nodeps"));

        let order = graph.topological_sort(&["nodeps".into()]).unwrap();
        assert_eq!(order, vec!["nodeps"]);
    }

    // ==================== Filter Uninstalled Edge Cases ====================

    #[test]
    fn test_filter_uninstalled_no_installed_field() {
        let dir = TempDir::new().unwrap();
        // Recipe without installed field
        std::fs::write(
            dir.path().join("pkg.rhai"),
            r#"let name = "pkg";
let version = "1.0";
"#,
        )
        .unwrap();

        let deps = vec![("pkg".into(), dir.path().join("pkg.rhai"))];
        let uninstalled = filter_uninstalled(deps).unwrap();
        // No installed field means not installed
        assert_eq!(uninstalled.len(), 1);
    }

    #[test]
    fn test_filter_uninstalled_all_installed() {
        let dir = TempDir::new().unwrap();
        for i in 0..3 {
            std::fs::write(
                dir.path().join(format!("pkg{}.rhai", i)),
                format!(
                    r#"let name = "pkg{}";
let version = "1.0";
let installed = true;
"#,
                    i
                ),
            )
            .unwrap();
        }

        let deps: Vec<_> = (0..3)
            .map(|i| (format!("pkg{}", i), dir.path().join(format!("pkg{}.rhai", i))))
            .collect();

        let uninstalled = filter_uninstalled(deps).unwrap();
        assert!(uninstalled.is_empty());
    }

    #[test]
    fn test_filter_uninstalled_mixed() {
        let dir = TempDir::new().unwrap();

        std::fs::write(
            dir.path().join("installed1.rhai"),
            r#"let installed = true;"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("installed2.rhai"),
            r#"let installed = true;"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("notinstalled.rhai"),
            r#"let installed = false;"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("nofield.rhai"), r#"let name = "x";"#).unwrap();

        let deps = vec![
            ("installed1".into(), dir.path().join("installed1.rhai")),
            ("notinstalled".into(), dir.path().join("notinstalled.rhai")),
            ("installed2".into(), dir.path().join("installed2.rhai")),
            ("nofield".into(), dir.path().join("nofield.rhai")),
        ];

        let uninstalled = filter_uninstalled(deps).unwrap();
        assert_eq!(uninstalled.len(), 2);
        assert!(uninstalled.iter().any(|(n, _)| n == "notinstalled"));
        assert!(uninstalled.iter().any(|(n, _)| n == "nofield"));
    }

    // ==================== Package Name Edge Cases ====================

    #[test]
    fn test_package_names_with_hyphens() {
        let mut graph = DepGraph::new();
        graph.add_package("my-app".into(), vec!["my-lib".into()], PathBuf::from("my-app.rhai"));
        graph.add_package("my-lib".into(), vec![], PathBuf::from("my-lib.rhai"));

        let order = graph.topological_sort(&["my-app".into()]).unwrap();
        assert_eq!(order, vec!["my-lib", "my-app"]);
    }

    #[test]
    fn test_package_names_with_numbers() {
        let mut graph = DepGraph::new();
        graph.add_package("lib2".into(), vec!["lib1".into()], PathBuf::from("lib2.rhai"));
        graph.add_package("lib1".into(), vec![], PathBuf::from("lib1.rhai"));

        let order = graph.topological_sort(&["lib2".into()]).unwrap();
        assert_eq!(order, vec!["lib1", "lib2"]);
    }

    #[test]
    fn test_long_package_names() {
        let long_name = "a".repeat(100);
        let mut graph = DepGraph::new();
        graph.add_package(long_name.clone(), vec![], PathBuf::from("long.rhai"));

        let order = graph.topological_sort(&[long_name.clone()]).unwrap();
        assert_eq!(order, vec![long_name]);
    }

    // ==================== DepGraph API Tests ====================

    #[test]
    fn test_depgraph_default() {
        let graph = DepGraph::default();
        assert!(!graph.contains("anything"));
    }

    #[test]
    fn test_depgraph_get_path() {
        let mut graph = DepGraph::new();
        graph.add_package("pkg".into(), vec![], PathBuf::from("/path/to/pkg.rhai"));

        assert_eq!(
            graph.get_path("pkg"),
            Some(&PathBuf::from("/path/to/pkg.rhai"))
        );
        assert_eq!(graph.get_path("nonexistent"), None);
    }

    #[test]
    fn test_depgraph_contains() {
        let mut graph = DepGraph::new();
        assert!(!graph.contains("pkg"));

        graph.add_package("pkg".into(), vec![], PathBuf::from("pkg.rhai"));
        assert!(graph.contains("pkg"));
        assert!(!graph.contains("other"));
    }

    #[test]
    fn test_depgraph_add_package_overwrites() {
        let mut graph = DepGraph::new();
        graph.add_package("pkg".into(), vec!["old-dep".into()], PathBuf::from("old.rhai"));
        graph.add_package("pkg".into(), vec!["new-dep".into()], PathBuf::from("new.rhai"));

        // Should have the new values
        assert_eq!(graph.get_path("pkg"), Some(&PathBuf::from("new.rhai")));
    }

    // ==================== Integration Tests ====================

    #[test]
    fn test_full_workflow_from_recipes() {
        let dir = TempDir::new().unwrap();

        // Create a realistic set of recipes
        std::fs::write(
            dir.path().join("openssl.rhai"),
            r#"let name = "openssl";
let version = "3.0.0";
let deps = [];
let installed = true;
"#,
        )
        .unwrap();

        std::fs::write(
            dir.path().join("curl.rhai"),
            r#"let name = "curl";
let version = "8.0.0";
let deps = ["openssl"];
let installed = false;
"#,
        )
        .unwrap();

        std::fs::write(
            dir.path().join("git.rhai"),
            r#"let name = "git";
let version = "2.40.0";
let deps = ["curl", "openssl"];
let installed = false;
"#,
        )
        .unwrap();

        // Resolve deps for git
        let all_deps = resolve_deps("git", dir.path()).unwrap();
        assert_eq!(all_deps.len(), 3);
        assert_eq!(all_deps[0].0, "openssl");
        assert_eq!(all_deps[1].0, "curl");
        assert_eq!(all_deps[2].0, "git");

        // Filter to only uninstalled
        let to_install = filter_uninstalled(all_deps).unwrap();
        assert_eq!(to_install.len(), 2);
        assert!(to_install.iter().any(|(n, _)| n == "curl"));
        assert!(to_install.iter().any(|(n, _)| n == "git"));
        assert!(!to_install.iter().any(|(n, _)| n == "openssl"));
    }

    #[test]
    fn test_resolve_deps_returns_correct_paths() {
        let dir = TempDir::new().unwrap();
        write_recipe(dir.path(), "a", &["b"]);
        write_recipe(dir.path(), "b", &[]);

        let result = resolve_deps("a", dir.path()).unwrap();

        // Verify paths are correct
        assert!(result[0].1.ends_with("b.rhai"));
        assert!(result[1].1.ends_with("a.rhai"));
    }
}
