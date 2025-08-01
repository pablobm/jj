// Copyright 2021-2023 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![allow(missing_docs)]

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::hash::Hash;

/// Node and edges pair of type `N` and `ID` respectively.
///
/// `ID` uniquely identifies a node within the graph. It's usually cheap to
/// clone. There should be a pure `(&N) -> &ID` function.
pub type GraphNode<N, ID = N> = (N, Vec<GraphEdge<ID>>);

#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub struct GraphEdge<N> {
    pub target: N,
    pub edge_type: GraphEdgeType,
}

impl<N> GraphEdge<N> {
    pub fn missing(target: N) -> Self {
        Self {
            target,
            edge_type: GraphEdgeType::Missing,
        }
    }

    pub fn direct(target: N) -> Self {
        Self {
            target,
            edge_type: GraphEdgeType::Direct,
        }
    }

    pub fn indirect(target: N) -> Self {
        Self {
            target,
            edge_type: GraphEdgeType::Indirect,
        }
    }

    pub fn map<M>(self, f: impl FnOnce(N) -> M) -> GraphEdge<M> {
        GraphEdge {
            target: f(self.target),
            edge_type: self.edge_type,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub enum GraphEdgeType {
    Missing,
    Direct,
    Indirect,
}

fn reachable_targets<N>(edges: &[GraphEdge<N>]) -> impl DoubleEndedIterator<Item = &N> {
    edges
        .iter()
        .filter(|edge| edge.edge_type != GraphEdgeType::Missing)
        .map(|edge| &edge.target)
}

/// Creates new graph in which nodes and edges are reversed.
pub fn reverse_graph<N, ID: Clone + Eq + Hash, E>(
    input: impl Iterator<Item = Result<GraphNode<N, ID>, E>>,
    as_id: impl Fn(&N) -> &ID,
) -> Result<Vec<GraphNode<N, ID>>, E> {
    let mut entries = vec![];
    let mut reverse_edges: HashMap<ID, Vec<GraphEdge<ID>>> = HashMap::new();
    for item in input {
        let (node, edges) = item?;
        for GraphEdge { target, edge_type } in edges {
            reverse_edges.entry(target).or_default().push(GraphEdge {
                target: as_id(&node).clone(),
                edge_type,
            });
        }
        entries.push(node);
    }

    let mut items = vec![];
    for node in entries.into_iter().rev() {
        let edges = reverse_edges.remove(as_id(&node)).unwrap_or_default();
        items.push((node, edges));
    }
    Ok(items)
}

/// Graph iterator adapter to group topological branches.
///
/// Basic idea is DFS from the heads. At fork point, the other descendant
/// branches will be visited. At merge point, the second (or the last) ancestor
/// branch will be visited first. This is practically [the same as Git][Git].
///
/// If no branches are prioritized, the branch containing the first commit in
/// the input iterator will be emitted first. It is often the working-copy
/// ancestor branch. The other head branches won't be enqueued eagerly, and will
/// be emitted as late as possible.
///
/// [Git]: https://github.blog/2022-08-30-gits-database-internals-ii-commit-history-queries/#topological-sorting
#[derive(Clone, Debug)]
pub struct TopoGroupedGraphIterator<N, ID, I, F> {
    input_iter: I,
    as_id: F,
    /// Graph nodes read from the input iterator but not yet emitted.
    nodes: HashMap<ID, TopoGroupedGraphNode<N, ID>>,
    /// Stack of graph nodes to be emitted.
    emittable_ids: Vec<ID>,
    /// List of new head nodes found while processing unpopulated nodes, or
    /// prioritized branch nodes added by caller.
    new_head_ids: VecDeque<ID>,
    /// Set of nodes which may be ancestors of `new_head_ids`.
    blocked_ids: HashSet<ID>,
}

#[derive(Clone, Debug)]
struct TopoGroupedGraphNode<N, ID> {
    /// Graph nodes which must be emitted before.
    child_ids: HashSet<ID>,
    /// Graph node data and edges to parent nodes. `None` until this node is
    /// populated.
    item: Option<GraphNode<N, ID>>,
}

impl<N, ID> Default for TopoGroupedGraphNode<N, ID> {
    fn default() -> Self {
        Self {
            child_ids: Default::default(),
            item: None,
        }
    }
}

impl<N, ID, E, I, F> TopoGroupedGraphIterator<N, ID, I, F>
where
    ID: Clone + Hash + Eq,
    I: Iterator<Item = Result<GraphNode<N, ID>, E>>,
    F: Fn(&N) -> &ID,
{
    /// Wraps the given iterator to group topological branches. The input
    /// iterator must be topologically ordered.
    pub fn new(input_iter: I, as_id: F) -> Self {
        Self {
            input_iter,
            as_id,
            nodes: HashMap::new(),
            emittable_ids: Vec::new(),
            new_head_ids: VecDeque::new(),
            blocked_ids: HashSet::new(),
        }
    }

    /// Makes the branch containing the specified node be emitted earlier than
    /// the others.
    ///
    /// The `id` usually points to a head node, but this isn't a requirement.
    /// If the specified node isn't a head, all preceding nodes will be queued.
    ///
    /// The specified node must exist in the input iterator. If it didn't, the
    /// iterator would panic.
    pub fn prioritize_branch(&mut self, id: ID) {
        // Mark existence of unpopulated node
        self.nodes.entry(id.clone()).or_default();
        // Push to non-emitting list so the prioritized heads wouldn't be
        // interleaved
        self.new_head_ids.push_back(id);
    }

    fn populate_one(&mut self) -> Result<Option<()>, E> {
        let item = match self.input_iter.next() {
            Some(Ok(item)) => item,
            Some(Err(err)) => {
                return Err(err);
            }
            None => {
                return Ok(None);
            }
        };
        let (data, edges) = &item;
        let current_id = (self.as_id)(data);

        // Set up reverse reference
        for parent_id in reachable_targets(edges) {
            let parent_node = self.nodes.entry(parent_id.clone()).or_default();
            parent_node.child_ids.insert(current_id.clone());
        }

        // Populate the current node
        if let Some(current_node) = self.nodes.get_mut(current_id) {
            assert!(current_node.item.is_none());
            current_node.item = Some(item);
        } else {
            let current_id = current_id.clone();
            let current_node = TopoGroupedGraphNode {
                item: Some(item),
                ..Default::default()
            };
            self.nodes.insert(current_id.clone(), current_node);
            // Push to non-emitting list so the new head wouldn't be interleaved
            self.new_head_ids.push_back(current_id);
        }

        Ok(Some(()))
    }

    /// Enqueues the first new head which will unblock the waiting ancestors.
    ///
    /// This does not move multiple head nodes to the queue at once because
    /// heads may be connected to the fork points in arbitrary order.
    fn flush_new_head(&mut self) {
        assert!(!self.new_head_ids.is_empty());
        if self.blocked_ids.is_empty() || self.new_head_ids.len() <= 1 {
            // Fast path: orphaned or no choice
            let new_head_id = self.new_head_ids.pop_front().unwrap();
            self.emittable_ids.push(new_head_id);
            self.blocked_ids.clear();
            return;
        }

        // Mark descendant nodes reachable from the blocking nodes
        let mut to_visit: Vec<&ID> = self
            .blocked_ids
            .iter()
            .map(|id| {
                // Borrow from self.nodes so self.blocked_ids can be mutated later
                let (id, _) = self.nodes.get_key_value(id).unwrap();
                id
            })
            .collect();
        let mut visited: HashSet<&ID> = to_visit.iter().copied().collect();
        while let Some(id) = to_visit.pop() {
            let node = self.nodes.get(id).unwrap();
            to_visit.extend(node.child_ids.iter().filter(|id| visited.insert(id)));
        }

        // Pick the first reachable head
        let index = self
            .new_head_ids
            .iter()
            .position(|id| visited.contains(id))
            .expect("blocking head should exist");
        let new_head_id = self.new_head_ids.remove(index).unwrap();

        // Unmark ancestors of the selected head so they won't contribute to future
        // new-head resolution within the newly-unblocked sub graph. The sub graph
        // can have many fork points, and the corresponding heads should be picked in
        // the fork-point order, not in the head appearance order.
        to_visit.push(&new_head_id);
        visited.remove(&new_head_id);
        while let Some(id) = to_visit.pop() {
            let node = self.nodes.get(id).unwrap();
            if let Some((_, edges)) = &node.item {
                to_visit.extend(reachable_targets(edges).filter(|id| visited.remove(id)));
            }
        }
        self.blocked_ids.retain(|id| visited.contains(id));
        self.emittable_ids.push(new_head_id);
    }

    fn next_node(&mut self) -> Result<Option<GraphNode<N, ID>>, E> {
        // Based on Kahn's algorithm
        loop {
            if let Some(current_id) = self.emittable_ids.last() {
                let Some(current_node) = self.nodes.get_mut(current_id) else {
                    // Queued twice because new children populated and emitted
                    self.emittable_ids.pop().unwrap();
                    continue;
                };
                if !current_node.child_ids.is_empty() {
                    // New children populated after emitting the other
                    let current_id = self.emittable_ids.pop().unwrap();
                    self.blocked_ids.insert(current_id);
                    continue;
                }
                let Some(item) = current_node.item.take() else {
                    // Not yet populated
                    self.populate_one()?
                        .expect("parent or prioritized node should exist");
                    continue;
                };
                // The second (or the last) parent will be visited first
                let current_id = self.emittable_ids.pop().unwrap();
                self.nodes.remove(&current_id).unwrap();
                let (_, edges) = &item;
                for parent_id in reachable_targets(edges) {
                    let parent_node = self.nodes.get_mut(parent_id).unwrap();
                    parent_node.child_ids.remove(&current_id);
                    if parent_node.child_ids.is_empty() {
                        let reusable_id = self.blocked_ids.take(parent_id);
                        let parent_id = reusable_id.unwrap_or_else(|| parent_id.clone());
                        self.emittable_ids.push(parent_id);
                    } else {
                        self.blocked_ids.insert(parent_id.clone());
                    }
                }
                return Ok(Some(item));
            } else if !self.new_head_ids.is_empty() {
                self.flush_new_head();
            } else {
                // Populate the first or orphan head
                if self.populate_one()?.is_none() {
                    return Ok(None);
                }
            }
        }
    }
}

impl<N, ID, E, I, F> Iterator for TopoGroupedGraphIterator<N, ID, I, F>
where
    ID: Clone + Hash + Eq,
    I: Iterator<Item = Result<GraphNode<N, ID>, E>>,
    F: Fn(&N) -> &ID,
{
    type Item = Result<GraphNode<N, ID>, E>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_node() {
            Ok(Some(node)) => Some(Ok(node)),
            Ok(None) => {
                assert!(self.nodes.is_empty(), "all nodes should have been emitted");
                None
            }
            Err(err) => Some(Err(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use itertools::Itertools as _;
    use renderdag::Ancestor;
    use renderdag::GraphRowRenderer;
    use renderdag::Renderer as _;

    use super::*;

    fn missing(c: char) -> GraphEdge<char> {
        GraphEdge::missing(c)
    }

    fn direct(c: char) -> GraphEdge<char> {
        GraphEdge::direct(c)
    }

    fn indirect(c: char) -> GraphEdge<char> {
        GraphEdge::indirect(c)
    }

    fn format_edge(edge: &GraphEdge<char>) -> String {
        let c = edge.target;
        match edge.edge_type {
            GraphEdgeType::Missing => format!("missing({c})"),
            GraphEdgeType::Direct => format!("direct({c})"),
            GraphEdgeType::Indirect => format!("indirect({c})"),
        }
    }

    fn format_graph(
        graph_iter: impl IntoIterator<Item = Result<GraphNode<char>, Infallible>>,
    ) -> String {
        let mut renderer = GraphRowRenderer::new()
            .output()
            .with_min_row_height(2)
            .build_box_drawing();
        graph_iter
            .into_iter()
            .map(|item| match item {
                Ok(node) => node,
                Err(err) => match err {},
            })
            .map(|(id, edges)| {
                let glyph = id.to_string();
                let message = edges.iter().map(format_edge).join(", ");
                let parents = edges
                    .into_iter()
                    .map(|edge| match edge.edge_type {
                        GraphEdgeType::Missing => Ancestor::Anonymous,
                        GraphEdgeType::Direct => Ancestor::Parent(edge.target),
                        GraphEdgeType::Indirect => Ancestor::Ancestor(edge.target),
                    })
                    .collect();
                renderer.next_row(id, parents, glyph, message)
            })
            .collect()
    }

    #[test]
    fn test_format_graph() {
        let graph = [
            ('D', vec![direct('C'), indirect('B')]),
            ('C', vec![direct('A')]),
            ('B', vec![missing('X')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph), @r"
        D    direct(C), indirect(B)
        ├─╮
        C ╷  direct(A)
        │ ╷
        │ B  missing(X)
        │ │
        │ ~
        │
        A
        ");
    }

    type TopoGrouped<N, I> = TopoGroupedGraphIterator<N, N, I, fn(&N) -> &N>;

    fn topo_grouped<I, E>(graph_iter: I) -> TopoGrouped<char, I::IntoIter>
    where
        I: IntoIterator<Item = Result<GraphNode<char>, E>>,
    {
        TopoGroupedGraphIterator::new(graph_iter.into_iter(), |c| c)
    }

    #[test]
    fn test_topo_grouped_multiple_roots() {
        let graph = [
            ('C', vec![missing('Y')]),
            ('B', vec![missing('X')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        C  missing(Y)
        │
        ~

        B  missing(X)
        │
        ~

        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        C  missing(Y)
        │
        ~

        B  missing(X)
        │
        ~

        A
        ");

        // All nodes can be lazily emitted.
        let mut iter = topo_grouped(graph.iter().cloned().peekable());
        assert_eq!(iter.next().unwrap().unwrap().0, 'C');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'B');
        assert_eq!(iter.next().unwrap().unwrap().0, 'B');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'A');
    }

    #[test]
    fn test_topo_grouped_trivial_fork() {
        let graph = [
            ('E', vec![direct('B')]),
            ('D', vec![direct('A')]),
            ('C', vec![direct('B')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        E  direct(B)
        │
        │ D  direct(A)
        │ │
        │ │ C  direct(B)
        ├───╯
        B │  direct(A)
        ├─╯
        A
        ");
        // D-A is found earlier than B-A, but B is emitted first because it belongs to
        // the emitting branch.
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        E  direct(B)
        │
        │ C  direct(B)
        ├─╯
        B  direct(A)
        │
        │ D  direct(A)
        ├─╯
        A
        ");

        // E can be lazy, then D and C will be queued.
        let mut iter = topo_grouped(graph.iter().cloned().peekable());
        assert_eq!(iter.next().unwrap().unwrap().0, 'E');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'D');
        assert_eq!(iter.next().unwrap().unwrap().0, 'C');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'B');
        assert_eq!(iter.next().unwrap().unwrap().0, 'B');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'A');
    }

    #[test]
    fn test_topo_grouped_fork_interleaved() {
        let graph = [
            ('F', vec![direct('D')]),
            ('E', vec![direct('C')]),
            ('D', vec![direct('B')]),
            ('C', vec![direct('B')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        F  direct(D)
        │
        │ E  direct(C)
        │ │
        D │  direct(B)
        │ │
        │ C  direct(B)
        ├─╯
        B  direct(A)
        │
        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        F  direct(D)
        │
        D  direct(B)
        │
        │ E  direct(C)
        │ │
        │ C  direct(B)
        ├─╯
        B  direct(A)
        │
        A
        ");

        // F can be lazy, then E will be queued, then C.
        let mut iter = topo_grouped(graph.iter().cloned().peekable());
        assert_eq!(iter.next().unwrap().unwrap().0, 'F');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'E');
        assert_eq!(iter.next().unwrap().unwrap().0, 'D');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'C');
        assert_eq!(iter.next().unwrap().unwrap().0, 'E');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'B');
    }

    #[test]
    fn test_topo_grouped_fork_multiple_heads() {
        let graph = [
            ('I', vec![direct('E')]),
            ('H', vec![direct('C')]),
            ('G', vec![direct('A')]),
            ('F', vec![direct('E')]),
            ('E', vec![direct('C')]),
            ('D', vec![direct('C')]),
            ('C', vec![direct('A')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        I  direct(E)
        │
        │ H  direct(C)
        │ │
        │ │ G  direct(A)
        │ │ │
        │ │ │ F  direct(E)
        ├─────╯
        E │ │  direct(C)
        ├─╯ │
        │ D │  direct(C)
        ├─╯ │
        C   │  direct(A)
        ├───╯
        │ B  direct(A)
        ├─╯
        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        I  direct(E)
        │
        │ F  direct(E)
        ├─╯
        E  direct(C)
        │
        │ H  direct(C)
        ├─╯
        │ D  direct(C)
        ├─╯
        C  direct(A)
        │
        │ G  direct(A)
        ├─╯
        │ B  direct(A)
        ├─╯
        A
        ");

        // I can be lazy, then H, G, and F will be queued.
        let mut iter = topo_grouped(graph.iter().cloned().peekable());
        assert_eq!(iter.next().unwrap().unwrap().0, 'I');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'H');
        assert_eq!(iter.next().unwrap().unwrap().0, 'F');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'E');
    }

    #[test]
    fn test_topo_grouped_fork_parallel() {
        let graph = [
            // Pull all sub graphs in reverse order:
            ('I', vec![direct('A')]),
            ('H', vec![direct('C')]),
            ('G', vec![direct('E')]),
            // Orphan sub graph G,F-E:
            ('F', vec![direct('E')]),
            ('E', vec![missing('Y')]),
            // Orphan sub graph H,D-C:
            ('D', vec![direct('C')]),
            ('C', vec![missing('X')]),
            // Orphan sub graph I,B-A:
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        I  direct(A)
        │
        │ H  direct(C)
        │ │
        │ │ G  direct(E)
        │ │ │
        │ │ │ F  direct(E)
        │ │ ├─╯
        │ │ E  missing(Y)
        │ │ │
        │ │ ~
        │ │
        │ │ D  direct(C)
        │ ├─╯
        │ C  missing(X)
        │ │
        │ ~
        │
        │ B  direct(A)
        ├─╯
        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        I  direct(A)
        │
        │ B  direct(A)
        ├─╯
        A

        H  direct(C)
        │
        │ D  direct(C)
        ├─╯
        C  missing(X)
        │
        ~

        G  direct(E)
        │
        │ F  direct(E)
        ├─╯
        E  missing(Y)
        │
        ~
        ");
    }

    #[test]
    fn test_topo_grouped_fork_nested() {
        fn sub_graph(
            chars: impl IntoIterator<Item = char>,
            root_edges: Vec<GraphEdge<char>>,
        ) -> Vec<GraphNode<char>> {
            let [b, c, d, e, f]: [char; 5] = chars.into_iter().collect_vec().try_into().unwrap();
            vec![
                (f, vec![direct(c)]),
                (e, vec![direct(b)]),
                (d, vec![direct(c)]),
                (c, vec![direct(b)]),
                (b, root_edges),
            ]
        }

        // One nested fork sub graph from A
        let graph = itertools::chain!(
            vec![('G', vec![direct('A')])],
            sub_graph('B'..='F', vec![direct('A')]),
            vec![('A', vec![])],
        )
        .map(Ok)
        .collect_vec();
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        G  direct(A)
        │
        │ F  direct(C)
        │ │
        │ │ E  direct(B)
        │ │ │
        │ │ │ D  direct(C)
        │ ├───╯
        │ C │  direct(B)
        │ ├─╯
        │ B  direct(A)
        ├─╯
        A
        ");
        // A::F is picked at A, and A will be unblocked. Then, C::D at C, ...
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        G  direct(A)
        │
        │ F  direct(C)
        │ │
        │ │ D  direct(C)
        │ ├─╯
        │ C  direct(B)
        │ │
        │ │ E  direct(B)
        │ ├─╯
        │ B  direct(A)
        ├─╯
        A
        ");

        // Two nested fork sub graphs from A
        let graph = itertools::chain!(
            vec![('L', vec![direct('A')])],
            sub_graph('G'..='K', vec![direct('A')]),
            sub_graph('B'..='F', vec![direct('A')]),
            vec![('A', vec![])],
        )
        .map(Ok)
        .collect_vec();
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        L  direct(A)
        │
        │ K  direct(H)
        │ │
        │ │ J  direct(G)
        │ │ │
        │ │ │ I  direct(H)
        │ ├───╯
        │ H │  direct(G)
        │ ├─╯
        │ G  direct(A)
        ├─╯
        │ F  direct(C)
        │ │
        │ │ E  direct(B)
        │ │ │
        │ │ │ D  direct(C)
        │ ├───╯
        │ C │  direct(B)
        │ ├─╯
        │ B  direct(A)
        ├─╯
        A
        ");
        // A::K is picked at A, and A will be unblocked. Then, H::I at H, ...
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        L  direct(A)
        │
        │ K  direct(H)
        │ │
        │ │ I  direct(H)
        │ ├─╯
        │ H  direct(G)
        │ │
        │ │ J  direct(G)
        │ ├─╯
        │ G  direct(A)
        ├─╯
        │ F  direct(C)
        │ │
        │ │ D  direct(C)
        │ ├─╯
        │ C  direct(B)
        │ │
        │ │ E  direct(B)
        │ ├─╯
        │ B  direct(A)
        ├─╯
        A
        ");

        // Two nested fork sub graphs from A, interleaved
        let graph = itertools::chain!(
            vec![('L', vec![direct('A')])],
            sub_graph(['C', 'E', 'G', 'I', 'K'], vec![direct('A')]),
            sub_graph(['B', 'D', 'F', 'H', 'J'], vec![direct('A')]),
            vec![('A', vec![])],
        )
        .sorted_by(|(id1, _), (id2, _)| id2.cmp(id1))
        .map(Ok)
        .collect_vec();
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        L  direct(A)
        │
        │ K  direct(E)
        │ │
        │ │ J  direct(D)
        │ │ │
        │ │ │ I  direct(C)
        │ │ │ │
        │ │ │ │ H  direct(B)
        │ │ │ │ │
        │ │ │ │ │ G  direct(E)
        │ ├───────╯
        │ │ │ │ │ F  direct(D)
        │ │ ├─────╯
        │ E │ │ │  direct(C)
        │ ├───╯ │
        │ │ D   │  direct(B)
        │ │ ├───╯
        │ C │  direct(A)
        ├─╯ │
        │   B  direct(A)
        ├───╯
        A
        ");
        // A::K is picked at A, and A will be unblocked. Then, E::G at E, ...
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        L  direct(A)
        │
        │ K  direct(E)
        │ │
        │ │ G  direct(E)
        │ ├─╯
        │ E  direct(C)
        │ │
        │ │ I  direct(C)
        │ ├─╯
        │ C  direct(A)
        ├─╯
        │ J  direct(D)
        │ │
        │ │ F  direct(D)
        │ ├─╯
        │ D  direct(B)
        │ │
        │ │ H  direct(B)
        │ ├─╯
        │ B  direct(A)
        ├─╯
        A
        ");

        // Merged fork sub graphs at K
        let graph = itertools::chain!(
            vec![('K', vec![direct('E'), direct('J')])],
            sub_graph('F'..='J', vec![missing('Y')]),
            sub_graph('A'..='E', vec![missing('X')]),
        )
        .map(Ok)
        .collect_vec();
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        K    direct(E), direct(J)
        ├─╮
        │ J  direct(G)
        │ │
        │ │ I  direct(F)
        │ │ │
        │ │ │ H  direct(G)
        │ ├───╯
        │ G │  direct(F)
        │ ├─╯
        │ F  missing(Y)
        │ │
        │ ~
        │
        E  direct(B)
        │
        │ D  direct(A)
        │ │
        │ │ C  direct(B)
        ├───╯
        B │  direct(A)
        ├─╯
        A  missing(X)
        │
        ~
        ");
        // K-E,J is resolved without queuing new heads. Then, G::H, F::I, B::C, and
        // A::D.
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        K    direct(E), direct(J)
        ├─╮
        │ J  direct(G)
        │ │
        E │  direct(B)
        │ │
        │ │ H  direct(G)
        │ ├─╯
        │ G  direct(F)
        │ │
        │ │ I  direct(F)
        │ ├─╯
        │ F  missing(Y)
        │ │
        │ ~
        │
        │ C  direct(B)
        ├─╯
        B  direct(A)
        │
        │ D  direct(A)
        ├─╯
        A  missing(X)
        │
        ~
        ");

        // Merged fork sub graphs at K, interleaved
        let graph = itertools::chain!(
            vec![('K', vec![direct('I'), direct('J')])],
            sub_graph(['B', 'D', 'F', 'H', 'J'], vec![missing('Y')]),
            sub_graph(['A', 'C', 'E', 'G', 'I'], vec![missing('X')]),
        )
        .sorted_by(|(id1, _), (id2, _)| id2.cmp(id1))
        .map(Ok)
        .collect_vec();
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        K    direct(I), direct(J)
        ├─╮
        │ J  direct(D)
        │ │
        I │  direct(C)
        │ │
        │ │ H  direct(B)
        │ │ │
        │ │ │ G  direct(A)
        │ │ │ │
        │ │ │ │ F  direct(D)
        │ ├─────╯
        │ │ │ │ E  direct(C)
        ├───────╯
        │ D │ │  direct(B)
        │ ├─╯ │
        C │   │  direct(A)
        ├─────╯
        │ B  missing(Y)
        │ │
        │ ~
        │
        A  missing(X)
        │
        ~
        ");
        // K-I,J is resolved without queuing new heads. Then, D::F, B::H, C::E, and
        // A::G.
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        K    direct(I), direct(J)
        ├─╮
        │ J  direct(D)
        │ │
        I │  direct(C)
        │ │
        │ │ F  direct(D)
        │ ├─╯
        │ D  direct(B)
        │ │
        │ │ H  direct(B)
        │ ├─╯
        │ B  missing(Y)
        │ │
        │ ~
        │
        │ E  direct(C)
        ├─╯
        C  direct(A)
        │
        │ G  direct(A)
        ├─╯
        A  missing(X)
        │
        ~
        ");
    }

    #[test]
    fn test_topo_grouped_merge_interleaved() {
        let graph = [
            ('F', vec![direct('E')]),
            ('E', vec![direct('C'), direct('D')]),
            ('D', vec![direct('B')]),
            ('C', vec![direct('A')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        F  direct(E)
        │
        E    direct(C), direct(D)
        ├─╮
        │ D  direct(B)
        │ │
        C │  direct(A)
        │ │
        │ B  direct(A)
        ├─╯
        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        F  direct(E)
        │
        E    direct(C), direct(D)
        ├─╮
        │ D  direct(B)
        │ │
        │ B  direct(A)
        │ │
        C │  direct(A)
        ├─╯
        A
        ");

        // F, E, and D can be lazy, then C will be queued, then B.
        let mut iter = topo_grouped(graph.iter().cloned().peekable());
        assert_eq!(iter.next().unwrap().unwrap().0, 'F');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'E');
        assert_eq!(iter.next().unwrap().unwrap().0, 'E');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'D');
        assert_eq!(iter.next().unwrap().unwrap().0, 'D');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'C');
        assert_eq!(iter.next().unwrap().unwrap().0, 'B');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'A');
    }

    #[test]
    fn test_topo_grouped_merge_but_missing() {
        let graph = [
            ('E', vec![direct('D')]),
            ('D', vec![missing('Y'), direct('C')]),
            ('C', vec![direct('B'), missing('X')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        E  direct(D)
        │
        D    missing(Y), direct(C)
        ├─╮
        │ │
        ~ │
          │
          C  direct(B), missing(X)
        ╭─┤
        │ │
        ~ │
          │
          B  direct(A)
          │
          A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        E  direct(D)
        │
        D    missing(Y), direct(C)
        ├─╮
        │ │
        ~ │
          │
          C  direct(B), missing(X)
        ╭─┤
        │ │
        ~ │
          │
          B  direct(A)
          │
          A
        ");

        // All nodes can be lazily emitted.
        let mut iter = topo_grouped(graph.iter().cloned().peekable());
        assert_eq!(iter.next().unwrap().unwrap().0, 'E');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'D');
        assert_eq!(iter.next().unwrap().unwrap().0, 'D');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'C');
        assert_eq!(iter.next().unwrap().unwrap().0, 'C');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'B');
        assert_eq!(iter.next().unwrap().unwrap().0, 'B');
        assert_eq!(iter.input_iter.peek().unwrap().as_ref().unwrap().0, 'A');
    }

    #[test]
    fn test_topo_grouped_merge_criss_cross() {
        let graph = [
            ('G', vec![direct('E')]),
            ('F', vec![direct('D')]),
            ('E', vec![direct('B'), direct('C')]),
            ('D', vec![direct('B'), direct('C')]),
            ('C', vec![direct('A')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        G  direct(E)
        │
        │ F  direct(D)
        │ │
        E │    direct(B), direct(C)
        ├───╮
        │ D │  direct(B), direct(C)
        ╭─┴─╮
        │   C  direct(A)
        │   │
        B   │  direct(A)
        ├───╯
        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        G  direct(E)
        │
        E    direct(B), direct(C)
        ├─╮
        │ │ F  direct(D)
        │ │ │
        │ │ D  direct(B), direct(C)
        ╭─┬─╯
        │ C  direct(A)
        │ │
        B │  direct(A)
        ├─╯
        A
        ");
    }

    #[test]
    fn test_topo_grouped_merge_descendants_interleaved() {
        let graph = [
            ('H', vec![direct('F')]),
            ('G', vec![direct('E')]),
            ('F', vec![direct('D')]),
            ('E', vec![direct('C')]),
            ('D', vec![direct('C'), direct('B')]),
            ('C', vec![direct('A')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        H  direct(F)
        │
        │ G  direct(E)
        │ │
        F │  direct(D)
        │ │
        │ E  direct(C)
        │ │
        D │  direct(C), direct(B)
        ├─╮
        │ C  direct(A)
        │ │
        B │  direct(A)
        ├─╯
        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        H  direct(F)
        │
        F  direct(D)
        │
        D    direct(C), direct(B)
        ├─╮
        │ B  direct(A)
        │ │
        │ │ G  direct(E)
        │ │ │
        │ │ E  direct(C)
        ├───╯
        C │  direct(A)
        ├─╯
        A
        ");
    }

    #[test]
    fn test_topo_grouped_merge_multiple_roots() {
        let graph = [
            ('D', vec![direct('C')]),
            ('C', vec![direct('B'), direct('A')]),
            ('B', vec![missing('X')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        D  direct(C)
        │
        C    direct(B), direct(A)
        ├─╮
        B │  missing(X)
        │ │
        ~ │
          │
          A
        ");
        // A is emitted first because it's the second parent.
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        D  direct(C)
        │
        C    direct(B), direct(A)
        ├─╮
        │ A
        │
        B  missing(X)
        │
        ~
        ");
    }

    #[test]
    fn test_topo_grouped_merge_stairs() {
        let graph = [
            // Merge topic branches one by one:
            ('J', vec![direct('I'), direct('G')]),
            ('I', vec![direct('H'), direct('E')]),
            ('H', vec![direct('D'), direct('F')]),
            // Topic branches:
            ('G', vec![direct('D')]),
            ('F', vec![direct('C')]),
            ('E', vec![direct('B')]),
            // Base nodes:
            ('D', vec![direct('C')]),
            ('C', vec![direct('B')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        J    direct(I), direct(G)
        ├─╮
        I │    direct(H), direct(E)
        ├───╮
        H │ │    direct(D), direct(F)
        ├─────╮
        │ G │ │  direct(D)
        ├─╯ │ │
        │   │ F  direct(C)
        │   │ │
        │   E │  direct(B)
        │   │ │
        D   │ │  direct(C)
        ├─────╯
        C   │  direct(B)
        ├───╯
        B  direct(A)
        │
        A
        ");
        // Second branches are visited first.
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        J    direct(I), direct(G)
        ├─╮
        │ G  direct(D)
        │ │
        I │    direct(H), direct(E)
        ├───╮
        │ │ E  direct(B)
        │ │ │
        H │ │  direct(D), direct(F)
        ├─╮ │
        F │ │  direct(C)
        │ │ │
        │ D │  direct(C)
        ├─╯ │
        C   │  direct(B)
        ├───╯
        B  direct(A)
        │
        A
        ");
    }

    #[test]
    fn test_topo_grouped_merge_and_fork() {
        let graph = [
            ('J', vec![direct('F')]),
            ('I', vec![direct('E')]),
            ('H', vec![direct('G')]),
            ('G', vec![direct('D'), direct('E')]),
            ('F', vec![direct('C')]),
            ('E', vec![direct('B')]),
            ('D', vec![direct('B')]),
            ('C', vec![direct('A')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        J  direct(F)
        │
        │ I  direct(E)
        │ │
        │ │ H  direct(G)
        │ │ │
        │ │ G  direct(D), direct(E)
        │ ╭─┤
        F │ │  direct(C)
        │ │ │
        │ E │  direct(B)
        │ │ │
        │ │ D  direct(B)
        │ ├─╯
        C │  direct(A)
        │ │
        │ B  direct(A)
        ├─╯
        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        J  direct(F)
        │
        F  direct(C)
        │
        C  direct(A)
        │
        │ I  direct(E)
        │ │
        │ │ H  direct(G)
        │ │ │
        │ │ G  direct(D), direct(E)
        │ ╭─┤
        │ E │  direct(B)
        │ │ │
        │ │ D  direct(B)
        │ ├─╯
        │ B  direct(A)
        ├─╯
        A
        ");
    }

    #[test]
    fn test_topo_grouped_merge_and_fork_multiple_roots() {
        let graph = [
            ('J', vec![direct('F')]),
            ('I', vec![direct('G')]),
            ('H', vec![direct('E')]),
            ('G', vec![direct('E'), direct('B')]),
            ('F', vec![direct('D')]),
            ('E', vec![direct('C')]),
            ('D', vec![direct('A')]),
            ('C', vec![direct('A')]),
            ('B', vec![missing('X')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        J  direct(F)
        │
        │ I  direct(G)
        │ │
        │ │ H  direct(E)
        │ │ │
        │ G │  direct(E), direct(B)
        │ ├─╮
        F │ │  direct(D)
        │ │ │
        │ │ E  direct(C)
        │ │ │
        D │ │  direct(A)
        │ │ │
        │ │ C  direct(A)
        ├───╯
        │ B  missing(X)
        │ │
        │ ~
        │
        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        J  direct(F)
        │
        F  direct(D)
        │
        D  direct(A)
        │
        │ I  direct(G)
        │ │
        │ G    direct(E), direct(B)
        │ ├─╮
        │ │ B  missing(X)
        │ │ │
        │ │ ~
        │ │
        │ │ H  direct(E)
        │ ├─╯
        │ E  direct(C)
        │ │
        │ C  direct(A)
        ├─╯
        A
        ");
    }

    #[test]
    fn test_topo_grouped_parallel_interleaved() {
        let graph = [
            ('E', vec![direct('C')]),
            ('D', vec![direct('B')]),
            ('C', vec![direct('A')]),
            ('B', vec![missing('X')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        E  direct(C)
        │
        │ D  direct(B)
        │ │
        C │  direct(A)
        │ │
        │ B  missing(X)
        │ │
        │ ~
        │
        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        E  direct(C)
        │
        C  direct(A)
        │
        A

        D  direct(B)
        │
        B  missing(X)
        │
        ~
        ");
    }

    #[test]
    fn test_topo_grouped_multiple_child_dependencies() {
        let graph = [
            ('I', vec![direct('H'), direct('G')]),
            ('H', vec![direct('D')]),
            ('G', vec![direct('B')]),
            ('F', vec![direct('E'), direct('C')]),
            ('E', vec![direct('D')]),
            ('D', vec![direct('B')]),
            ('C', vec![direct('B')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        I    direct(H), direct(G)
        ├─╮
        H │  direct(D)
        │ │
        │ G  direct(B)
        │ │
        │ │ F    direct(E), direct(C)
        │ │ ├─╮
        │ │ E │  direct(D)
        ├───╯ │
        D │   │  direct(B)
        ├─╯   │
        │     C  direct(B)
        ├─────╯
        B  direct(A)
        │
        A
        ");
        // Topological order must be preserved. Depending on the implementation,
        // E might be requested more than once by paths D->E and B->D->E.
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        I    direct(H), direct(G)
        ├─╮
        │ G  direct(B)
        │ │
        H │  direct(D)
        │ │
        │ │ F    direct(E), direct(C)
        │ │ ├─╮
        │ │ │ C  direct(B)
        │ ├───╯
        │ │ E  direct(D)
        ├───╯
        D │  direct(B)
        ├─╯
        B  direct(A)
        │
        A
        ");
    }

    #[test]
    fn test_topo_grouped_prioritized_branches_trivial_fork() {
        // The same setup as test_topo_grouped_trivial_fork()
        let graph = [
            ('E', vec![direct('B')]),
            ('D', vec![direct('A')]),
            ('C', vec![direct('B')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        E  direct(B)
        │
        │ D  direct(A)
        │ │
        │ │ C  direct(B)
        ├───╯
        B │  direct(A)
        ├─╯
        A
        ");

        // Emit the branch C first
        let mut iter = topo_grouped(graph.iter().cloned());
        iter.prioritize_branch('C');
        insta::assert_snapshot!(format_graph(iter), @r"
        C  direct(B)
        │
        │ E  direct(B)
        ├─╯
        B  direct(A)
        │
        │ D  direct(A)
        ├─╯
        A
        ");

        // Emit the branch D first
        let mut iter = topo_grouped(graph.iter().cloned());
        iter.prioritize_branch('D');
        insta::assert_snapshot!(format_graph(iter), @r"
        D  direct(A)
        │
        │ E  direct(B)
        │ │
        │ │ C  direct(B)
        │ ├─╯
        │ B  direct(A)
        ├─╯
        A
        ");

        // Emit the branch C first, then D. E is emitted earlier than D because
        // E belongs to the branch C compared to the branch D.
        let mut iter = topo_grouped(graph.iter().cloned());
        iter.prioritize_branch('C');
        iter.prioritize_branch('D');
        insta::assert_snapshot!(format_graph(iter), @r"
        C  direct(B)
        │
        │ E  direct(B)
        ├─╯
        B  direct(A)
        │
        │ D  direct(A)
        ├─╯
        A
        ");

        // Non-head node can be prioritized
        let mut iter = topo_grouped(graph.iter().cloned());
        iter.prioritize_branch('B');
        insta::assert_snapshot!(format_graph(iter), @r"
        E  direct(B)
        │
        │ C  direct(B)
        ├─╯
        B  direct(A)
        │
        │ D  direct(A)
        ├─╯
        A
        ");

        // Root node can be prioritized
        let mut iter = topo_grouped(graph.iter().cloned());
        iter.prioritize_branch('A');
        insta::assert_snapshot!(format_graph(iter), @r"
        D  direct(A)
        │
        │ E  direct(B)
        │ │
        │ │ C  direct(B)
        │ ├─╯
        │ B  direct(A)
        ├─╯
        A
        ");
    }

    #[test]
    fn test_topo_grouped_prioritized_branches_fork_multiple_heads() {
        // The same setup as test_topo_grouped_fork_multiple_heads()
        let graph = [
            ('I', vec![direct('E')]),
            ('H', vec![direct('C')]),
            ('G', vec![direct('A')]),
            ('F', vec![direct('E')]),
            ('E', vec![direct('C')]),
            ('D', vec![direct('C')]),
            ('C', vec![direct('A')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        I  direct(E)
        │
        │ H  direct(C)
        │ │
        │ │ G  direct(A)
        │ │ │
        │ │ │ F  direct(E)
        ├─────╯
        E │ │  direct(C)
        ├─╯ │
        │ D │  direct(C)
        ├─╯ │
        C   │  direct(A)
        ├───╯
        │ B  direct(A)
        ├─╯
        A
        ");

        // Emit B, G, then remainders
        let mut iter = topo_grouped(graph.iter().cloned());
        iter.prioritize_branch('B');
        iter.prioritize_branch('G');
        insta::assert_snapshot!(format_graph(iter), @r"
        B  direct(A)
        │
        │ G  direct(A)
        ├─╯
        │ I  direct(E)
        │ │
        │ │ F  direct(E)
        │ ├─╯
        │ E  direct(C)
        │ │
        │ │ H  direct(C)
        │ ├─╯
        │ │ D  direct(C)
        │ ├─╯
        │ C  direct(A)
        ├─╯
        A
        ");

        // Emit D, H, then descendants of C. The order of B and G is not
        // respected because G can be found earlier through C->A->G. At this
        // point, B is not populated yet, so A is blocked only by {G}. This is
        // a limitation of the current node reordering logic.
        let mut iter = topo_grouped(graph.iter().cloned());
        iter.prioritize_branch('D');
        iter.prioritize_branch('H');
        iter.prioritize_branch('B');
        iter.prioritize_branch('G');
        insta::assert_snapshot!(format_graph(iter), @r"
        D  direct(C)
        │
        │ H  direct(C)
        ├─╯
        │ I  direct(E)
        │ │
        │ │ F  direct(E)
        │ ├─╯
        │ E  direct(C)
        ├─╯
        C  direct(A)
        │
        │ G  direct(A)
        ├─╯
        │ B  direct(A)
        ├─╯
        A
        ");
    }

    #[test]
    fn test_topo_grouped_prioritized_branches_fork_parallel() {
        // The same setup as test_topo_grouped_fork_parallel()
        let graph = [
            // Pull all sub graphs in reverse order:
            ('I', vec![direct('A')]),
            ('H', vec![direct('C')]),
            ('G', vec![direct('E')]),
            // Orphan sub graph G,F-E:
            ('F', vec![direct('E')]),
            ('E', vec![missing('Y')]),
            // Orphan sub graph H,D-C:
            ('D', vec![direct('C')]),
            ('C', vec![missing('X')]),
            // Orphan sub graph I,B-A:
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        I  direct(A)
        │
        │ H  direct(C)
        │ │
        │ │ G  direct(E)
        │ │ │
        │ │ │ F  direct(E)
        │ │ ├─╯
        │ │ E  missing(Y)
        │ │ │
        │ │ ~
        │ │
        │ │ D  direct(C)
        │ ├─╯
        │ C  missing(X)
        │ │
        │ ~
        │
        │ B  direct(A)
        ├─╯
        A
        ");

        // Emit the sub graph G first
        let mut iter = topo_grouped(graph.iter().cloned());
        iter.prioritize_branch('G');
        insta::assert_snapshot!(format_graph(iter), @r"
        G  direct(E)
        │
        │ F  direct(E)
        ├─╯
        E  missing(Y)
        │
        ~

        I  direct(A)
        │
        │ B  direct(A)
        ├─╯
        A

        H  direct(C)
        │
        │ D  direct(C)
        ├─╯
        C  missing(X)
        │
        ~
        ");

        // Emit sub graphs in reverse order by selecting roots
        let mut iter = topo_grouped(graph.iter().cloned());
        iter.prioritize_branch('E');
        iter.prioritize_branch('C');
        iter.prioritize_branch('A');
        insta::assert_snapshot!(format_graph(iter), @r"
        G  direct(E)
        │
        │ F  direct(E)
        ├─╯
        E  missing(Y)
        │
        ~

        H  direct(C)
        │
        │ D  direct(C)
        ├─╯
        C  missing(X)
        │
        ~

        I  direct(A)
        │
        │ B  direct(A)
        ├─╯
        A
        ");
    }

    #[test]
    fn test_topo_grouped_requeue_unpopulated() {
        let graph = [
            ('C', vec![direct('A'), direct('B')]),
            ('B', vec![direct('A')]),
            ('A', vec![]),
        ]
        .map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        C    direct(A), direct(B)
        ├─╮
        │ B  direct(A)
        ├─╯
        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        C    direct(A), direct(B)
        ├─╮
        │ B  direct(A)
        ├─╯
        A
        ");

        // A is queued once by C-A because B isn't populated at this point. Since
        // B is the second parent, B-A is processed next and A is queued again. So
        // one of them in the queue has to be ignored.
        let mut iter = topo_grouped(graph.iter().cloned());
        assert_eq!(iter.next().unwrap().unwrap().0, 'C');
        assert_eq!(iter.emittable_ids, vec!['A', 'B']);
        assert_eq!(iter.next().unwrap().unwrap().0, 'B');
        assert_eq!(iter.emittable_ids, vec!['A', 'A']);
        assert_eq!(iter.next().unwrap().unwrap().0, 'A');
        assert!(iter.next().is_none());
        assert!(iter.emittable_ids.is_empty());
    }

    #[test]
    fn test_topo_grouped_duplicated_edges() {
        // The graph shouldn't have duplicated parent->child edges, but topo-grouped
        // iterator can handle it anyway.
        let graph = [('B', vec![direct('A'), direct('A')]), ('A', vec![])].map(Ok);
        insta::assert_snapshot!(format_graph(graph.iter().cloned()), @r"
        B  direct(A), direct(A)
        │
        A
        ");
        insta::assert_snapshot!(format_graph(topo_grouped(graph.iter().cloned())), @r"
        B  direct(A), direct(A)
        │
        A
        ");

        let mut iter = topo_grouped(graph.iter().cloned());
        assert_eq!(iter.next().unwrap().unwrap().0, 'B');
        assert_eq!(iter.emittable_ids, vec!['A', 'A']);
        assert_eq!(iter.next().unwrap().unwrap().0, 'A');
        assert!(iter.next().is_none());
        assert!(iter.emittable_ids.is_empty());
    }
}
