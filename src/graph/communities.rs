//! Step 5 of the concept graph: communities of linked concepts, by the
//! Louvain method (Blondel, Guillaume, Lambiotte and Lefebvre, 2008).

/// A node moves to another community only if modularity rises by more than
/// this, so rounding cannot make it move back and forth.
const MIN_GAIN: f64 = 1e-7;

/// The community, numbered from 0, of each of `nodes` nodes of the
/// undirected graph whose links are `edges` as `(a, b, weight)`. Nodes are
/// visited in index order, so the same graph always gives the same
/// communities. A node without links is a community of its own.
pub fn louvain(nodes: usize, edges: &[(usize, usize, f64)]) -> Vec<usize> {
    let mut membership: Vec<usize> = (0..nodes).collect();
    let mut graph = Graph::new(nodes, edges);
    while let Some(local) = graph.move_nodes() {
        for community in &mut membership {
            *community = local[*community];
        }
        graph = graph.aggregate(&local);
    }
    membership
}

/// A weighted undirected graph as adjacency lists sorted by neighbour. A
/// self-loop holds the weight inside a node twice over, as the adjacency
/// matrix of the graph it was merged from would.
struct Graph {
    adjacency: Vec<Vec<(usize, f64)>>,
}

impl Graph {
    fn new(nodes: usize, edges: &[(usize, usize, f64)]) -> Graph {
        let mut matrix = vec![std::collections::BTreeMap::new(); nodes];
        for &(a, b, weight) in edges {
            *matrix[a].entry(b).or_insert(0.0) += weight;
            *matrix[b].entry(a).or_insert(0.0) += weight;
        }
        Graph {
            adjacency: matrix
                .into_iter()
                .map(|m| m.into_iter().collect())
                .collect(),
        }
    }

    /// Moves each node, in index order, into the neighbouring community that
    /// raises modularity most, until no move helps. Returns each node's
    /// community renumbered in order of first appearance, or `None` when no
    /// node moved.
    fn move_nodes(&self) -> Option<Vec<usize>> {
        let n = self.adjacency.len();
        let degree: Vec<f64> = self
            .adjacency
            .iter()
            .map(|row| row.iter().map(|(_, w)| w).sum())
            .collect();
        let m = degree.iter().sum::<f64>() / 2.0;
        if m <= 0.0 {
            return None;
        }
        let mut community: Vec<usize> = (0..n).collect();
        let mut total = degree.clone();
        let mut weight_to = vec![0.0; n];
        let mut moved = false;
        loop {
            let mut moved_now = false;
            for i in 0..n {
                let current = community[i];
                let mut neighbours = Vec::new();
                for &(j, w) in &self.adjacency[i] {
                    if j == i {
                        continue;
                    }
                    let c = community[j];
                    if weight_to[c] == 0.0 {
                        neighbours.push(c);
                    }
                    weight_to[c] += w;
                }
                total[current] -= degree[i];
                let gain = |c: usize| weight_to[c] / m - total[c] * degree[i] / (2.0 * m * m);
                let mut best = current;
                let mut best_gain = gain(current);
                for &c in &neighbours {
                    let g = gain(c);
                    if g > best_gain + MIN_GAIN {
                        best = c;
                        best_gain = g;
                    }
                }
                total[best] += degree[i];
                community[i] = best;
                if best != current {
                    moved_now = true;
                    moved = true;
                }
                for c in neighbours {
                    weight_to[c] = 0.0;
                }
            }
            if !moved_now {
                break;
            }
        }
        moved.then(|| renumber(&community))
    }

    /// The graph whose nodes are the communities in `local`, each link
    /// weighing what the links between the two communities weighed.
    fn aggregate(&self, local: &[usize]) -> Graph {
        let communities = local.iter().max().map_or(0, |c| c + 1);
        let mut matrix = vec![std::collections::BTreeMap::new(); communities];
        for (i, row) in self.adjacency.iter().enumerate() {
            for &(j, w) in row {
                *matrix[local[i]].entry(local[j]).or_insert(0.0) += w;
            }
        }
        Graph {
            adjacency: matrix
                .into_iter()
                .map(|m| m.into_iter().collect())
                .collect(),
        }
    }
}

/// Community labels renumbered from 0 in order of first appearance.
fn renumber(community: &[usize]) -> Vec<usize> {
    let mut seen = std::collections::HashMap::new();
    community
        .iter()
        .map(|&c| {
            let next = seen.len();
            *seen.entry(c).or_insert(next)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_cliques_joined_by_one_link_split_and_a_lone_node_stays_alone() {
        let mut edges: Vec<(usize, usize, f64)> = [(0, 1), (0, 2), (1, 2), (3, 4), (3, 5), (4, 5)]
            .iter()
            .map(|&(a, b)| (a, b, 1.0))
            .collect();
        edges.push((2, 3, 0.2));
        let c = louvain(7, &edges);
        assert!(
            c[0] == c[1] && c[1] == c[2] && c[3] == c[4] && c[4] == c[5],
            "{c:?}"
        );
        assert_ne!(c[0], c[3], "{c:?}");
        assert!((0..6).all(|i| c[i] != c[6]), "{c:?}");
    }
}
