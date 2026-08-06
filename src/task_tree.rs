//! Task tree: the parent/child task hierarchy loaded from the database.
//!
//! [`TaskTree::load`] fetches only the rows reachable from the root task
//! (a recursive CTE over `todos.parent`, bounded by `UNION` de-duplication)
//! and assembles them into [`TaskTreeNode`]s. Nothing in the CLI writes
//! `parent` yet — the tree is currently read-only, for future views.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use sqlx::SqlitePool;

use crate::sql::TaskRow;

/// A task and its descendants, rooted at one task.
#[derive(Debug, Clone)]
pub struct TaskTree {
    pub root: TaskTreeNode,
}

/// One node of a task tree: the underlying row plus its children.
#[derive(Debug, Clone)]
pub struct TaskTreeNode {
    pub row: TaskRow,
    pub children: Vec<TaskTreeNode>,
}

impl TaskTree {
    /// Load the subtree rooted at `root_id`: the task itself plus every
    /// descendant, in a single query.
    ///
    /// Returns `None` when no task with that id exists. The recursive CTE
    /// uses `UNION` (row de-duplication), so a parent cycle in the data
    /// cannot make the query loop; assembly additionally tracks seen ids so
    /// a corrupt parent link is clipped rather than recursed forever.
    pub async fn load(pool: &SqlitePool, root_id: i64) -> Result<Option<TaskTree>> {
        let now = crate::date::now();
        let rows = sqlx::query_as::<_, TaskRow>(
            r#"WITH RECURSIVE subtree(id) AS (
                   SELECT ? AS id
                   UNION
                   SELECT t.id FROM todos t JOIN subtree s ON t.parent = s.id
               )
               SELECT t.*, SUM(tc.count) AS completions
               FROM todos t
               LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                   AND tc.time >= CASE
                       WHEN t.interval_secs > 0 AND t.start_time IS NOT NULL THEN
                           CASE WHEN ? <= t.start_time THEN t.start_time
                                ELSE t.start_time + ((? - t.start_time) / t.interval_secs) * t.interval_secs END
                       ELSE 0 END
               WHERE t.id IN (SELECT id FROM subtree)
               GROUP BY t.id
               ORDER BY t.priority DESC, t.start_time ASC, t.id ASC"#,
        )
        .bind(root_id)
        .bind(now)
        .bind(now)
        .fetch_all(pool)
        .await
        .context("Failed to fetch task tree")?;

        if rows.is_empty() {
            return Ok(None);
        }

        let mut nodes: HashMap<i64, TaskRow> = HashMap::with_capacity(rows.len());
        let mut children_of: HashMap<i64, Vec<i64>> = HashMap::new();
        // Iterate the rows in query order (priority, start time, id) so the
        // sibling order in each `children` vec is stable and meaningful;
        // HashMap iteration order would randomize it.
        for r in rows {
            if let Some(parent) = r.parent {
                children_of.entry(parent).or_default().push(r.id);
            }
            nodes.insert(r.id, r);
        }

        let mut seen = HashSet::new();
        let root = assemble(root_id, &mut nodes, &children_of, &mut seen)
            .expect("the CTE seed guarantees the root row is present");
        Ok(Some(TaskTree { root }))
    }

    /// Render the tree as one single-line [`Line`] per node, in
    /// depth-first order: each row is `- badge name`, where `badge` is
    /// computed per row by the supplied closure (e.g. the completion
    /// marker `◯` / `●` / `● n/m`). The closure returns the badge text
    /// including any trailing space; return an empty string for a bare
    /// row. With `body` set, a non-empty task body is inserted underneath
    /// its row, each line indented to the start of the badge.
    pub fn render(&self, body: bool, badge: impl Fn(&TaskRow) -> String) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        push_node(&self.root, body, &badge, &mut out);
        out
    }
}

/// Append `node` and its subtree to `out`, depth-first.
fn push_node(
    node: &TaskTreeNode,
    body: bool,
    badge: &impl Fn(&TaskRow) -> String,
    out: &mut Vec<Line<'static>>,
) {
    out.push(Line::from(vec![
        Span::raw("- "),
        Span::styled(
            format!("{}{}", badge(&node.row), node.row.name),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    if body && !node.row.body.is_empty() {
        for body_line in node.row.body.lines() {
            out.push(Line::raw(format!("  {body_line}")));
        }
    }

    for child in &node.children {
        push_node(child, body, badge, out);
    }
}

/// Build the node subtree rooted at `id`, removing visited rows from
/// `nodes` so each row is emitted exactly once. Returns `None` when the id
/// was already visited on this path (a parent cycle) — the back edge is
/// clipped.
fn assemble(
    id: i64,
    nodes: &mut HashMap<i64, TaskRow>,
    children_of: &HashMap<i64, Vec<i64>>,
    seen: &mut HashSet<i64>,
) -> Option<TaskTreeNode> {
    if !seen.insert(id) {
        return None;
    }
    let mut node = TaskTreeNode {
        row: nodes.remove(&id)?,
        children: Vec::new(),
    };
    if let Some(kids) = children_of.get(&id) {
        for kid in kids {
            if let Some(child) = assemble(*kid, nodes, children_of, seen) {
                node.children.push(child);
            }
        }
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use crate::db::test_pool;
    use crate::sql::{create_task, TaskObject};
    use crate::types::TaskKind;

    use super::*;

    /// Insert a task via the typed API; returns its row id.
    async fn seed_task(
        pool: &sqlx::SqlitePool,
        name: &str,
        parent: Option<i64>,
        target_count: i32,
        interval_secs: Option<i64>,
    ) -> i64 {
        let (id, _short) = create_task(
            pool,
            &TaskObject {
                id: None,
                short_id: None,
                name: name.to_string(),
                body: format!("body of {name}"),
                priority: 5,
                start_time: Some(1_700_000_000),
                available_duration_secs: None,
                interval_secs,
                target_count,
                optional: false,
                end_time: None,
                parent,
            },
        )
        .await
        .unwrap();
        id
    }

    #[tokio::test]
    async fn test_load_builds_subtree() {
        let pool = test_pool().await.unwrap();
        let root = seed_task(&pool, "root", None, 0, None).await;
        let child = seed_task(&pool, "child", Some(root), 2, None).await;
        let grandchild = seed_task(&pool, "grandchild", Some(child), 0, None).await;
        let recurring = seed_task(&pool, "recurring", Some(child), 0, Some(86_400)).await;
        // Unrelated task must not leak into the tree.
        seed_task(&pool, "other", None, 0, None).await;

        let tree = TaskTree::load(&pool, root).await.unwrap().unwrap();
        assert_eq!(tree.root.row.id, root);
        assert_eq!(tree.root.row.name, "root");
        assert_eq!(tree.root.row.kind(), TaskKind::Oneshot);
        // No completions: SUM over zero rows is NULL, so the raw row holds
        // `None` (render treats it as 0).
        assert_eq!(tree.root.row.completions, None);
        assert_eq!(tree.root.children.len(), 1);

        let child_node = &tree.root.children[0];
        assert_eq!(child_node.row.id, child);
        assert_eq!(child_node.row.body, "body of child");
        assert_eq!(child_node.row.target_count, 2);
        assert_eq!(child_node.row.kind(), TaskKind::Oneshot);
        assert_eq!(child_node.children.len(), 2);
        assert_eq!(child_node.children[0].row.id, grandchild);
        assert_eq!(child_node.children[0].row.kind(), TaskKind::Oneshot);
        assert_eq!(child_node.children[1].row.id, recurring);
        assert_eq!(child_node.children[1].row.kind(), TaskKind::Recurring);
    }

    #[tokio::test]
    async fn test_load_missing_root_returns_none() {
        let pool = test_pool().await.unwrap();
        assert!(TaskTree::load(&pool, 42).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_load_clips_parent_cycles() {
        let pool = test_pool().await.unwrap();
        let a = seed_task(&pool, "a", None, 0, None).await;
        let b = seed_task(&pool, "b", Some(a), 0, None).await;
        // Corrupt a's parent to point back at b: a <-> b cycle.
        sqlx::query("UPDATE todos SET parent = ? WHERE id = ?")
            .bind(b)
            .bind(a)
            .execute(&pool)
            .await
            .unwrap();

        let tree = TaskTree::load(&pool, a).await.unwrap().unwrap();
        assert_eq!(tree.root.row.id, a);
        assert_eq!(tree.root.children.len(), 1);
        let b_node = &tree.root.children[0];
        assert_eq!(b_node.row.id, b);
        // The back edge a <- b is clipped: b has no children.
        assert!(b_node.children.is_empty());
    }

    #[tokio::test]
    async fn test_load_scopes_recurring_completions_to_interval() {
        let pool = test_pool().await.unwrap();
        // Recurring task anchored in the past; interval 1 day.
        let (id, _) = create_task(
            &pool,
            &TaskObject {
                id: None,
                short_id: None,
                name: "daily".to_string(),
                body: String::new(),
                priority: 5,
                start_time: Some(1_600_000_000),
                available_duration_secs: None,
                interval_secs: Some(86_400),
                target_count: 1,
                optional: false,
                end_time: None,
                parent: None,
            },
        )
        .await
        .unwrap();
        // Completion in a *previous* interval (2 days before the start of
        // the current one) must not count.
        sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, ?)")
            .bind(id)
            .bind(1_600_000_000 + 100)
            .bind(1)
            .execute(&pool)
            .await
            .unwrap();

        let tree = TaskTree::load(&pool, id).await.unwrap().unwrap();
        assert_eq!(tree.root.row.completions, None);
    }

    #[tokio::test]
    async fn test_render_rows_with_optional_body() {
        let pool = test_pool().await.unwrap();
        let root = seed_task(&pool, "root", None, 0, None).await;
        let _child = seed_task(&pool, "child", Some(root), 3, None).await;
        seed_task(&pool, "sibling", Some(root), 0, None).await;

        let tree = TaskTree::load(&pool, root).await.unwrap().unwrap();
        let text = |lines: &[Line<'static>]| {
            lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<String>>()
        };

        // Without body: one row per task, "- name" (empty badge lambda).
        let lines = text(&tree.render(false, |_| String::new()));
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "- root");
        assert_eq!(lines[1], "- child");
        assert_eq!(lines[2], "- sibling");

        // With body: each non-empty body is indented to the badge start
        // ("  ") and inserted under its row.
        let lines = text(&tree.render(true, |_| String::new()));
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], "- root");
        assert_eq!(lines[1], "  body of root");
        assert_eq!(lines[2], "- child");
        assert_eq!(lines[3], "  body of child");
        assert_eq!(lines[4], "- sibling");
        assert_eq!(lines[5], "  body of sibling");
    }

    #[tokio::test]
    async fn test_render_applies_badge_lambda_per_row() {
        let pool = test_pool().await.unwrap();
        let root = seed_task(&pool, "root", None, 0, None).await;
        seed_task(&pool, "child", Some(root), 0, None).await;

        let tree = TaskTree::load(&pool, root).await.unwrap().unwrap();
        // The badge closure runs per row, with access to the whole row
        // (here: a completion marker for one specific task).
        let lines = tree.render(false, |row: &TaskRow| {
            if row.name == "child" {
                "● ".to_string()
            } else {
                String::new()
            }
        });
        let text = |i: usize| {
            lines[i]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert_eq!(text(0), "- root");
        assert_eq!(text(1), "- ● child");
    }
}
