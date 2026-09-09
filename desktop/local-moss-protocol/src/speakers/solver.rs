use super::{add, ComponentEvidence, MappingError, Work, MAX_SLOTS, SEARCH_LIMIT_PER_COMPONENT};

pub(super) struct Solution {
    pub columns: Vec<Option<usize>>,
    pub total_support_ms: u64,
    pub components: Vec<ComponentEvidence>,
    pub search_nodes: u64,
}

pub(super) fn solve(
    weights: &[Vec<u64>],
    edges: &[Vec<bool>],
    unknown: &[bool],
    work: &mut Work<'_>,
) -> Result<Solution, MappingError> {
    solve_with_limit(weights, edges, unknown, SEARCH_LIMIT_PER_COMPONENT, work)
}

pub(super) fn solve_with_limit(
    weights: &[Vec<u64>],
    edges: &[Vec<bool>],
    unknown: &[bool],
    limit: u64,
    work: &mut Work<'_>,
) -> Result<Solution, MappingError> {
    let count = weights.len();
    let globals = weights.first().map_or(0, Vec::len);
    if count > MAX_SLOTS
        || globals > MAX_SLOTS
        || edges.len() != count
        || unknown.len() != count
        || weights.iter().any(|row| row.len() != globals)
        || edges.iter().any(|row| row.len() != count)
        || limit == 0
        || limit > SEARCH_LIMIT_PER_COMPONENT
    {
        return Err(MappingError::InternalInvariant);
    }
    // Prove every possible accumulated objective fits before searching.
    weights.iter().try_fold(0, |sum, row| {
        add(sum, row.iter().copied().max().unwrap_or(0))
    })?;
    for (i, row) in edges.iter().enumerate() {
        for (j, &edge) in row.iter().enumerate() {
            work.tick()?;
            if edge != edges[j][i] || (i == j && edge) {
                return Err(MappingError::InternalInvariant);
            }
        }
    }
    let domains: Vec<Vec<usize>> = weights
        .iter()
        .enumerate()
        .map(|(vertex, row)| {
            row.iter()
                .enumerate()
                .filter_map(|(column, support)| (*support > 0).then_some(column))
                .chain(unknown[vertex].then_some(globals))
                .collect()
        })
        .collect();
    if domains.iter().any(Vec::is_empty) {
        return Err(MappingError::Infeasible);
    }
    let mut seen = vec![false; count];
    let mut components = Vec::new();
    for vertex in 0..count {
        if seen[vertex] {
            continue;
        }
        seen[vertex] = true;
        let mut pending = vec![vertex];
        let mut component = Vec::new();
        while let Some(current) = pending.pop() {
            component.push(current);
            for (neighbor, seen) in seen.iter_mut().enumerate() {
                work.tick()?;
                if edges[current][neighbor] && !*seen {
                    *seen = true;
                    pending.push(neighbor);
                }
            }
        }
        component.sort_unstable();
        components.push(component);
    }
    let mut assigned = vec![None; count];
    let mut evidence = Vec::new();
    let (mut total, mut nodes) = (0, 0);
    for component in components {
        let mut search = Search {
            weights,
            edges,
            domains: &domains,
            component: &component,
            assigned: &mut assigned,
            globals,
            best_score: None,
            best: None,
            nodes: 0,
            limit,
            work,
        };
        search.visit(0, 0)?;
        let best = search.best.as_ref().ok_or(MappingError::Infeasible)?;
        for (&vertex, &column) in component.iter().zip(best) {
            search.assigned[vertex] = Some(column);
        }
        let score = search.best_score.ok_or(MappingError::InternalInvariant)?;
        let component_nodes = search.nodes;
        total = add(total, score)?;
        nodes = add(nodes, component_nodes)?;
        evidence.push(ComponentEvidence {
            vertices: component,
            total_support_ms: score,
            search_nodes: component_nodes,
        });
    }
    let columns = assigned
        .into_iter()
        .map(|column| {
            let column = column.ok_or(MappingError::InternalInvariant)?;
            Ok((column != globals).then_some(column))
        })
        .collect::<Result<Vec<_>, MappingError>>()?;
    Ok(Solution {
        columns,
        total_support_ms: total,
        components: evidence,
        search_nodes: nodes,
    })
}

struct Search<'a, 'w, 'c> {
    weights: &'a [Vec<u64>],
    edges: &'a [Vec<bool>],
    domains: &'a [Vec<usize>],
    component: &'a [usize],
    assigned: &'a mut [Option<usize>],
    globals: usize,
    best_score: Option<u64>,
    best: Option<Vec<usize>>,
    nodes: u64,
    limit: u64,
    work: &'w mut Work<'c>,
}

impl Search<'_, '_, '_> {
    fn compatible(&mut self, vertex: usize, column: usize) -> Result<bool, MappingError> {
        for &other in self.component {
            self.work.tick()?;
            if self.edges[vertex][other] && self.assigned[other] == Some(column) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn weight(&self, vertex: usize, column: usize) -> u64 {
        if column == self.globals {
            0
        } else {
            self.weights[vertex][column]
        }
    }

    fn visit(&mut self, position: usize, score: u64) -> Result<(), MappingError> {
        self.work.tick()?;
        self.nodes = add(self.nodes, 1)?;
        if self.nodes > self.limit {
            return Err(MappingError::SearchLimit);
        }
        let mut upper_bound = score;
        for index in position..self.component.len() {
            let vertex = self.component[index];
            let mut maximum = None;
            for index in 0..self.domains[vertex].len() {
                let column = self.domains[vertex][index];
                if self.compatible(vertex, column)? {
                    let value = self.weight(vertex, column);
                    maximum = Some(maximum.map_or(value, |previous: u64| previous.max(value)));
                }
            }
            let Some(maximum) = maximum else {
                return Ok(());
            };
            upper_bound = add(upper_bound, maximum)?;
        }
        // Sorted DFS means an equal-scoring later branch cannot improve the tie.
        if self.best_score.is_some_and(|best| upper_bound <= best) {
            return Ok(());
        }
        if position == self.component.len() {
            self.best_score = Some(score);
            self.best = Some(
                self.component
                    .iter()
                    .map(|vertex| self.assigned[*vertex].ok_or(MappingError::InternalInvariant))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            return Ok(());
        }
        let vertex = self.component[position];
        for index in 0..self.domains[vertex].len() {
            let column = self.domains[vertex][index];
            if !self.compatible(vertex, column)? {
                continue;
            }
            self.assigned[vertex] = Some(column);
            self.visit(position + 1, add(score, self.weight(vertex, column))?)?;
            self.assigned[vertex] = None;
        }
        Ok(())
    }
}
