use std::collections::{BTreeSet, HashMap, HashSet};

use super::Dependency;

pub(super) fn components<'a>(
    nodes: impl Iterator<Item = &'a str>,
    edges: &HashMap<&'a str, Vec<(&'a str, &'a Dependency)>>,
) -> Vec<Vec<&'a str>> {
    struct Tarjan<'a, 'b> {
        edges: &'b HashMap<&'a str, Vec<(&'a str, &'a Dependency)>>,
        next: usize,
        indices: HashMap<&'a str, usize>,
        low: HashMap<&'a str, usize>,
        stack: Vec<&'a str>,
        active: HashSet<&'a str>,
        output: Vec<Vec<&'a str>>,
    }

    impl<'a> Tarjan<'a, '_> {
        fn visit(&mut self, node: &'a str) {
            let index = self.next;
            self.next += 1;
            self.indices.insert(node, index);
            self.low.insert(node, index);
            self.stack.push(node);
            self.active.insert(node);
            for (target, _) in self.edges.get(node).into_iter().flatten() {
                if !self.indices.contains_key(target) {
                    self.visit(target);
                    let target_low = self.low[target];
                    self.low
                        .entry(node)
                        .and_modify(|value| *value = (*value).min(target_low));
                } else if self.active.contains(target) {
                    let target_index = self.indices[target];
                    self.low
                        .entry(node)
                        .and_modify(|value| *value = (*value).min(target_index));
                }
            }
            if self.low[node] == self.indices[node] {
                let mut component = Vec::new();
                loop {
                    let member = self.stack.pop().expect("active component has a member");
                    self.active.remove(member);
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                component.sort_unstable();
                self.output.push(component);
            }
        }
    }

    let mut state = Tarjan {
        edges,
        next: 0,
        indices: HashMap::new(),
        low: HashMap::new(),
        stack: Vec::new(),
        active: HashSet::new(),
        output: Vec::new(),
    };
    for node in nodes {
        if !state.indices.contains_key(node) {
            state.visit(node);
        }
    }
    state.output
}

pub(super) fn path<'a>(
    start: &'a str,
    members: &BTreeSet<&'a str>,
    edges: &HashMap<&'a str, Vec<(&'a str, &'a Dependency)>>,
) -> Option<Vec<&'a str>> {
    fn walk<'a>(
        node: &'a str,
        start: &'a str,
        members: &BTreeSet<&'a str>,
        edges: &HashMap<&'a str, Vec<(&'a str, &'a Dependency)>>,
        active: &mut HashSet<&'a str>,
        path: &mut Vec<&'a str>,
    ) -> bool {
        active.insert(node);
        path.push(node);
        for (target, _) in edges.get(node).into_iter().flatten() {
            if !members.contains(target) {
                continue;
            }
            if *target == start {
                path.push(start);
                return true;
            }
            if !active.contains(target) && walk(target, start, members, edges, active, path) {
                return true;
            }
        }
        path.pop();
        active.remove(node);
        false
    }

    let mut path = Vec::new();
    walk(start, start, members, edges, &mut HashSet::new(), &mut path).then_some(path)
}
