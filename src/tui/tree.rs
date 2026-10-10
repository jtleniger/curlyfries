//! Request tree rows: folders plus the requests nested under them.

use crate::project::Entry;

/// One rendered tree row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRow {
    /// A non-selectable folder line, e.g. `auth/` at `depth` indentation.
    Folder {
        /// Nesting level, `0` for a top-level folder.
        depth: usize,
        /// Folder name with a trailing `/`.
        label: String,
    },
    /// A selectable request; `index` is the position in `App::entries`.
    Request {
        /// Nesting level, `0` for a top-level request.
        depth: usize,
        /// Index into the entry (and label) list.
        index: usize,
    },
}

/// Builds folder + request rows from `entries` (already sorted by id).
///
/// A folder row is emitted the first time a path prefix is seen, so nested
/// folders appear exactly once, in id order.
pub fn build(entries: &[Entry]) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let mut folders: Vec<&str> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let segments: Vec<&str> = entry.id.split('/').collect();
        for (depth, segment) in segments[..segments.len() - 1].iter().enumerate() {
            let end = prefix_end(&segments, depth);
            let prefix = &entry.id[..end];
            if folders.contains(&prefix) {
                continue;
            }
            folders.push(prefix);
            rows.push(TreeRow::Folder {
                depth,
                label: format!("{segment}/"),
            });
        }
        rows.push(TreeRow::Request {
            depth: segments.len() - 1,
            index,
        });
    }
    rows
}

/// Byte offset past the `depth`-th path segment of `segments` in the joined id.
fn prefix_end(segments: &[&str], depth: usize) -> usize {
    segments[..=depth].iter().map(|s| s.len()).sum::<usize>() + depth
}

/// Keeps only rows whose request id or display label contains `query` (ASCII
/// case-insensitive), together with the folder rows needed to reach them.
/// Matching the id keeps folder names searchable even though the labels no
/// longer show the path. An empty `query` returns `rows` unchanged.
pub fn filter(rows: &[TreeRow], entries: &[Entry], labels: &[String], query: &str) -> Vec<TreeRow> {
    if query.is_empty() {
        return rows.to_vec();
    }
    let needle = query.to_ascii_lowercase();
    let mut keep = vec![false; rows.len()];
    // Open ancestors of the row being visited, innermost last.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for (position, row) in rows.iter().enumerate() {
        let depth = match row {
            TreeRow::Folder { depth, .. } | TreeRow::Request { depth, .. } => *depth,
        };
        stack.retain(|(_, open)| *open < depth);
        match row {
            TreeRow::Folder { depth, .. } => stack.push((position, *depth)),
            TreeRow::Request { index, .. } => {
                let matches_id = entries
                    .get(*index)
                    .is_some_and(|entry| entry.id.to_ascii_lowercase().contains(&needle));
                let matches_label = labels
                    .get(*index)
                    .is_some_and(|label| label.to_ascii_lowercase().contains(&needle));
                if matches_id || matches_label {
                    keep[position] = true;
                    for (folder, _) in &stack {
                        keep[*folder] = true;
                    }
                }
            }
        }
    }
    rows.iter()
        .zip(&keep)
        .filter(|(_, keep)| **keep)
        .map(|(row, _)| row.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entries(ids: &[&str]) -> Vec<Entry> {
        ids.iter()
            .map(|id| Entry {
                id: (*id).to_string(),
                file: PathBuf::from(format!("requests/{id}.json")),
            })
            .collect()
    }

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| {
                let leaf = entry.id.rsplit('/').next().unwrap_or(&entry.id);
                format!("GET {leaf}")
            })
            .collect()
    }

    #[test]
    fn nested_folders_appear_once() {
        let entries = entries(&["auth/login", "auth/me", "ships/list", "ping"]);
        let rows = build(&entries);
        assert_eq!(
            rows,
            vec![
                TreeRow::Folder {
                    depth: 0,
                    label: "auth/".to_string()
                },
                TreeRow::Request { depth: 1, index: 0 },
                TreeRow::Request { depth: 1, index: 1 },
                TreeRow::Folder {
                    depth: 0,
                    label: "ships/".to_string()
                },
                TreeRow::Request { depth: 1, index: 2 },
                TreeRow::Request { depth: 0, index: 3 },
            ]
        );
    }

    #[test]
    fn deeper_nesting_repeats_only_new_prefixes() {
        let entries = entries(&["a/b/c", "a/b/d", "a/e"]);
        let rows = build(&entries);
        assert_eq!(
            rows,
            vec![
                TreeRow::Folder {
                    depth: 0,
                    label: "a/".to_string()
                },
                TreeRow::Folder {
                    depth: 1,
                    label: "b/".to_string()
                },
                TreeRow::Request { depth: 2, index: 0 },
                TreeRow::Request { depth: 2, index: 1 },
                TreeRow::Request { depth: 1, index: 2 },
            ]
        );
    }

    #[test]
    fn empty_query_is_identity() {
        let entries = entries(&["auth/login", "ships/list"]);
        let rows = build(&entries);
        assert_eq!(filter(&rows, &entries, &labels(&entries), ""), rows);
    }

    #[test]
    fn filter_drops_folders_with_no_matching_request() {
        let entries = entries(&["a/b/c", "a/d"]);
        let labels = labels(&entries);
        let rows = build(&entries);
        assert_eq!(
            filter(&rows, &entries, &labels, "a/d"),
            vec![
                TreeRow::Folder {
                    depth: 0,
                    label: "a/".to_string()
                },
                TreeRow::Request { depth: 1, index: 1 },
            ]
        );
        assert!(filter(&rows, &entries, &labels, "nothing").is_empty());
    }

    #[test]
    fn filter_matches_ids_and_labels() {
        let entries = entries(&["Ships/List", "ships/create"]);
        let labels = vec!["GET List".to_string(), "POST Create ship".to_string()];
        let rows = build(&entries);
        // A folder/id fragment that no longer appears in the label still matches.
        assert_eq!(filter(&rows, &entries, &labels, "ships/list").len(), 2);
        assert_eq!(filter(&rows, &entries, &labels, "SHIPS").len(), 4);
        // A friendly name matches through the label.
        assert_eq!(filter(&rows, &entries, &labels, "create ship").len(), 2);
        assert!(filter(&rows, &entries, &labels, "missing").is_empty());
    }
}
