//! The SQLite file the Electron app wrote, read and written the same way: the same migrations and
//! JSON columns up to the task plan, which only this app adds.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::model::{
    AccountStatus, AgentId, StatusColor, StatusKind, Subscription, SubscriptionId, Task, TaskRun, TaskStatus, Thread, ThreadStatus,
    TimelineItem,
};
use crate::util::now_ms;

/// Append-only. Each entry runs once, in order, tracked by PRAGMA user_version.
pub const MIGRATIONS: [&str; 6] = [
    "CREATE TABLE IF NOT EXISTS threads (
       id TEXT PRIMARY KEY,
       agent_id TEXT NOT NULL,
       cwd TEXT NOT NULL,
       title TEXT NOT NULL,
       created_at INTEGER NOT NULL,
       updated_at INTEGER NOT NULL,
       acp_session_id TEXT,
       config_json TEXT NOT NULL,
       last_status TEXT NOT NULL
     );
     CREATE TABLE IF NOT EXISTS timeline_items (
       thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
       seq INTEGER NOT NULL,
       item_id TEXT NOT NULL,
       kind TEXT NOT NULL,
       json TEXT NOT NULL,
       PRIMARY KEY (thread_id, item_id)
     );
     CREATE INDEX IF NOT EXISTS timeline_items_order ON timeline_items(thread_id, seq);
     CREATE TABLE IF NOT EXISTS settings (
       key TEXT PRIMARY KEY,
       value_json TEXT NOT NULL
     );",
    // Seeding and assigning existing threads needs the filesystem, so SubscriptionStore does it after this runs.
    "CREATE TABLE IF NOT EXISTS subscriptions (
       id TEXT PRIMARY KEY,
       agent_id TEXT NOT NULL,
       label TEXT NOT NULL,
       config_dir TEXT NOT NULL,
       created_at INTEGER NOT NULL,
       account_json TEXT NOT NULL,
       UNIQUE (agent_id, config_dir)
     );
     ALTER TABLE threads ADD COLUMN subscription_id TEXT REFERENCES subscriptions(id);",
    // Default statuses are seeded per project when its board is first opened, see TaskStore.
    "CREATE TABLE IF NOT EXISTS task_statuses (
       id TEXT PRIMARY KEY,
       project TEXT NOT NULL,
       name TEXT NOT NULL,
       color TEXT NOT NULL,
       position REAL NOT NULL
     );
     CREATE TABLE IF NOT EXISTS tasks (
       id TEXT PRIMARY KEY,
       project TEXT NOT NULL,
       title TEXT NOT NULL,
       description TEXT NOT NULL,
       status_id TEXT NOT NULL REFERENCES task_statuses(id),
       position REAL NOT NULL,
       created_at INTEGER NOT NULL,
       updated_at INTEGER NOT NULL
     );
     CREATE INDEX IF NOT EXISTS tasks_status ON tasks(status_id, position);",
    // Numbers existing tasks per project in creation order; statuses named like the defaults get their kind.
    "ALTER TABLE task_statuses ADD COLUMN kind TEXT NOT NULL DEFAULT 'started';
     UPDATE task_statuses SET kind = CASE name WHEN 'Backlog' THEN 'backlog' WHEN 'To do' THEN 'unstarted' WHEN 'Done' THEN 'completed' ELSE 'started' END;
     ALTER TABLE tasks ADD COLUMN number INTEGER NOT NULL DEFAULT 0;
     UPDATE tasks SET number = (
       SELECT COUNT(*) FROM tasks AS t
       WHERE t.project = tasks.project AND (t.created_at < tasks.created_at OR (t.created_at = tasks.created_at AND t.id <= tasks.id))
     );",
    // No foreign key on the subscription: removing one sends its tasks back to the active subscription.
    "ALTER TABLE tasks ADD COLUMN subscription_id TEXT;
     ALTER TABLE tasks ADD COLUMN pull_request INTEGER NOT NULL DEFAULT 0;
     ALTER TABLE tasks ADD COLUMN run_json TEXT;",
    // From here on the schema is the Rust app's alone; the Electron app's migrations end above.
    "ALTER TABLE tasks ADD COLUMN model TEXT;
     CREATE TABLE IF NOT EXISTS task_dependencies (
       task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
       blocker_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
       PRIMARY KEY (task_id, blocker_id)
     );",
];

/// A subscription as stored: the login command is derived from the agent spec on load.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSubscription {
    pub id: SubscriptionId,
    pub agent_id: AgentId,
    pub label: String,
    pub config_dir: String,
    pub created_at: i64,
    pub account: AccountStatus,
}

impl From<&Subscription> for StoredSubscription {
    fn from(s: &Subscription) -> Self {
        StoredSubscription {
            id: s.id.clone(),
            agent_id: s.agent_id,
            label: s.label.clone(),
            config_dir: s.config_dir.clone(),
            created_at: s.created_at,
            account: s.account.clone(),
        }
    }
}

fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("Oniko's own types always serialize")
}

fn parse<T: DeserializeOwned>(text: &str, what: &str) -> Result<T> {
    serde_json::from_str(text).with_context(|| format!("unreadable {what} in the database"))
}

fn agent(row: &Row, column: &str) -> Result<AgentId> {
    let raw: String = row.get(column)?;
    AgentId::parse(&raw).ok_or_else(|| anyhow!("unknown agent {raw}"))
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::with_connection(conn)
    }

    pub fn in_memory() -> Result<Db> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    fn with_connection(conn: Connection) -> Result<Db> {
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        // The Electron app may hold the file too; wait for its writes instead of failing.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut db = Db { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&mut self) -> Result<()> {
        let current: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (version, sql) in MIGRATIONS.iter().enumerate().skip(current.max(0) as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", version + 1))?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn user_version(&self) -> Result<i64> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// Rows are trusted as Oniko's own writes, but one unreadable row is skipped rather than losing
    /// every thread. A thread without a subscription waits for the first one to be added (see
    /// `assign_orphan_threads`) and is not loaded until then.
    pub fn load_threads(&self) -> Result<Vec<Thread>> {
        let mut items: std::collections::HashMap<String, Vec<TimelineItem>> = std::collections::HashMap::new();
        let mut stmt = self.conn.prepare("SELECT thread_id, json FROM timeline_items ORDER BY thread_id, seq")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let thread_id: String = row.get(0)?;
            let text: String = row.get(1)?;
            match parse::<TimelineItem>(&text, "timeline item") {
                Ok(item) => items.entry(thread_id).or_default().push(item),
                Err(error) => eprintln!("oniko: skipping a timeline item of {thread_id}: {error:#}"),
            }
        }
        let mut stmt = self.conn.prepare("SELECT * FROM threads WHERE subscription_id IS NOT NULL ORDER BY created_at DESC")?;
        let mut rows = stmt.query([])?;
        let mut threads = Vec::new();
        while let Some(row) = rows.next()? {
            let mut read = || -> Result<Thread> {
                let id: String = row.get("id")?;
                Ok(Thread {
                    agent_id: agent(row, "agent_id")?,
                    subscription_id: row.get("subscription_id")?,
                    cwd: row.get("cwd")?,
                    title: row.get("title")?,
                    created_at: row.get("created_at")?,
                    acp_session_id: row.get("acp_session_id")?,
                    status: parse::<ThreadStatus>(&row.get::<_, String>("last_status")?, "thread status")?,
                    timeline: items.remove(&id).unwrap_or_default(),
                    config: parse(&row.get::<_, String>("config_json")?, "thread config")?,
                    commands: vec![],
                    id,
                })
            };
            match read() {
                Ok(thread) => threads.push(thread),
                Err(error) => eprintln!("oniko: skipping an unreadable thread: {error:#}"),
            }
        }
        Ok(threads)
    }

    /// Writes the thread row and the timeline positions an event touched.
    pub fn save_thread(&self, thread: &Thread, items: impl IntoIterator<Item = usize>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO threads (id, agent_id, subscription_id, cwd, title, created_at, updated_at, acp_session_id, config_json, last_status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, updated_at = excluded.updated_at,
               acp_session_id = excluded.acp_session_id, config_json = excluded.config_json, last_status = excluded.last_status",
            params![
                thread.id,
                thread.agent_id.as_str(),
                thread.subscription_id,
                thread.cwd,
                thread.title,
                thread.created_at,
                now_ms(),
                thread.acp_session_id,
                json(&thread.config),
                json(&thread.status),
            ],
        )?;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO timeline_items (thread_id, seq, item_id, kind, json) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(thread_id, item_id) DO UPDATE SET seq = excluded.seq, kind = excluded.kind, json = excluded.json",
            )?;
            for seq in items {
                let Some(item) = thread.timeline.get(seq) else { continue };
                upsert.execute(params![thread.id, seq as i64, item.id(), item.kind(), json(item)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_thread(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM threads WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn load_subscriptions(&self) -> Result<Vec<StoredSubscription>> {
        let mut stmt = self.conn.prepare("SELECT * FROM subscriptions ORDER BY created_at")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(StoredSubscription {
                id: row.get("id")?,
                agent_id: agent(row, "agent_id")?,
                label: row.get("label")?,
                config_dir: row.get("config_dir")?,
                created_at: row.get("created_at")?,
                account: parse::<AccountStatus>(&row.get::<_, String>("account_json")?, "account").unwrap_or(AccountStatus::Unknown),
            });
        }
        Ok(out)
    }

    pub fn save_subscription(&self, sub: &StoredSubscription) -> Result<()> {
        self.conn.execute(
            "INSERT INTO subscriptions (id, agent_id, label, config_dir, created_at, account_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET label = excluded.label, account_json = excluded.account_json",
            params![sub.id, sub.agent_id.as_str(), sub.label, sub.config_dir, sub.created_at, json(&sub.account)],
        )?;
        Ok(())
    }

    pub fn delete_subscription(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM subscriptions WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Gives every thread that predates subscriptions to this one; returns how many it adopted.
    pub fn assign_orphan_threads(&self, id: &str) -> Result<usize> {
        Ok(self.conn.execute("UPDATE threads SET subscription_id = ?1 WHERE subscription_id IS NULL", [id])?)
    }

    pub fn load_task_statuses(&self) -> Result<Vec<TaskStatus>> {
        let mut stmt = self.conn.prepare("SELECT * FROM task_statuses ORDER BY project, position")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let color: String = row.get("color")?;
            let kind: String = row.get("kind")?;
            out.push(TaskStatus {
                id: row.get("id")?,
                project: row.get("project")?,
                name: row.get("name")?,
                color: StatusColor::parse(&color).ok_or_else(|| anyhow!("unknown status color {color}"))?,
                kind: StatusKind::parse(&kind).ok_or_else(|| anyhow!("unknown status kind {kind}"))?,
                position: row.get("position")?,
            });
        }
        Ok(out)
    }

    pub fn save_task_status(&self, status: &TaskStatus) -> Result<()> {
        self.conn.execute(
            "INSERT INTO task_statuses (id, project, name, color, kind, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, color = excluded.color, kind = excluded.kind, position = excluded.position",
            params![status.id, status.project, status.name, status.color.as_str(), status.kind.as_str(), status.position],
        )?;
        Ok(())
    }

    /// Moves the status's tasks to the end of `fallback`, keeping their order, then drops the status.
    pub fn delete_task_status(&self, id: &str, fallback: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let last: Option<f64> = tx.query_row("SELECT MAX(position) FROM tasks WHERE status_id = ?1", [fallback], |r| r.get(0))?;
        let first: Option<f64> = tx.query_row("SELECT MIN(position) FROM tasks WHERE status_id = ?1", [id], |r| r.get(0))?;
        let shift = match (last, first) {
            (Some(last), Some(first)) => last - first + 1.0,
            _ => 0.0,
        };
        tx.execute("UPDATE tasks SET status_id = ?1, position = position + ?2 WHERE status_id = ?3", params![fallback, shift, id])?;
        tx.execute("DELETE FROM task_statuses WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_tasks(&self) -> Result<Vec<Task>> {
        let mut blockers: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        let mut stmt = self.conn.prepare("SELECT task_id, blocker_id FROM task_dependencies ORDER BY rowid")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            blockers.entry(row.get(0)?).or_default().push(row.get(1)?);
        }
        let mut stmt = self.conn.prepare("SELECT * FROM tasks ORDER BY project, position")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let run: Option<String> = row.get("run_json")?;
            let id: String = row.get("id")?;
            out.push(Task {
                blocked_by: blockers.remove(&id).unwrap_or_default(),
                id,
                project: row.get("project")?,
                number: row.get("number")?,
                title: row.get("title")?,
                description: row.get("description")?,
                status_id: row.get("status_id")?,
                position: row.get("position")?,
                created_at: row.get("created_at")?,
                updated_at: row.get("updated_at")?,
                subscription_id: row.get("subscription_id")?,
                pull_request: row.get::<_, i64>("pull_request")? == 1,
                model: row.get("model")?,
                run: run.map(|text| parse::<TaskRun>(&text, "task run")).transpose()?,
            });
        }
        Ok(out)
    }

    pub fn save_task(&self, task: &Task) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tasks (id, project, number, title, description, status_id, position, created_at, updated_at, subscription_id, pull_request, model, run_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, description = excluded.description, status_id = excluded.status_id,
               position = excluded.position, updated_at = excluded.updated_at, subscription_id = excluded.subscription_id,
               pull_request = excluded.pull_request, model = excluded.model, run_json = excluded.run_json",
            params![
                task.id,
                task.project,
                task.number,
                task.title,
                task.description,
                task.status_id,
                task.position,
                task.created_at,
                task.updated_at,
                task.subscription_id,
                task.pull_request as i64,
                task.model,
                task.run.as_ref().map(json),
            ],
        )?;
        Ok(())
    }

    /// Its dependency rows in both directions go with it.
    pub fn delete_task(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn add_dependency(&self, task_id: &str, blocker_id: &str) -> Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO task_dependencies (task_id, blocker_id) VALUES (?1, ?2)", [task_id, blocker_id])?;
        Ok(())
    }

    pub fn remove_dependency(&self, task_id: &str, blocker_id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM task_dependencies WHERE task_id = ?1 AND blocker_id = ?2", [task_id, blocker_id])?;
        Ok(())
    }

    /// None when the key is missing or holds something of another shape.
    pub fn get_setting<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let raw: Option<String> =
            self.conn.query_row("SELECT value_json FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten();
        serde_json::from_str(&raw?).ok()
    }

    pub fn set_setting<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value_json) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
            params![key, json(&value)],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::reducer::tests::{chunk, new_thread};
    use crate::model::reducer::{Outcome, ThreadMap, reduce_threads};
    use crate::model::{INTERRUPTED_NOTICE, NoticeLevel};

    fn with_subscription(db: &Db) {
        db.save_subscription(&StoredSubscription {
            id: "sub-1".into(),
            agent_id: AgentId::Claude,
            label: "Default".into(),
            config_dir: "/home/.claude".into(),
            created_at: 1,
            account: AccountStatus::Unknown,
        })
        .unwrap();
    }

    #[test]
    fn round_trips_a_thread_across_reopening_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oniko.db");
        let mut thread = new_thread();
        thread.title = "hello".into();
        thread.timeline.push(TimelineItem::User { id: "u".into(), text: "hi".into() });
        {
            let db = Db::open(&path).unwrap();
            with_subscription(&db);
            db.save_thread(&thread, 0..thread.timeline.len()).unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.load_threads().unwrap(), vec![thread]);
        assert_eq!(db.user_version().unwrap(), MIGRATIONS.len() as i64);
    }

    #[test]
    fn coalesces_streaming_chunks_into_one_row_per_item() {
        let db = Db::in_memory().unwrap();
        with_subscription(&db);
        let mut map = ThreadMap::new();
        map.insert("t-1".into(), new_thread());
        db.save_thread(&map["t-1"], []).unwrap();
        for text in ["a", "b", "c"] {
            if let Outcome::Updated { items } = reduce_threads(&mut map, &chunk(text)) {
                db.save_thread(&map["t-1"], items).unwrap();
            }
        }
        let rows: i64 = db.conn.query_row("SELECT COUNT(*) FROM timeline_items", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1);
        assert_eq!(db.load_threads().unwrap()[0].timeline, vec![TimelineItem::Assistant { id: "assistant-0".into(), text: "abc".into() }]);
    }

    #[test]
    fn deletes_a_removed_thread_with_its_items() {
        let db = Db::in_memory().unwrap();
        with_subscription(&db);
        let mut thread = new_thread();
        thread.timeline.push(TimelineItem::Notice { id: "n".into(), level: NoticeLevel::Info, text: "x".into() });
        db.save_thread(&thread, [0]).unwrap();
        db.delete_thread("t-1").unwrap();
        let rows: i64 = db.conn.query_row("SELECT COUNT(*) FROM timeline_items", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0);
        assert!(db.load_threads().unwrap().is_empty());
    }

    #[test]
    fn threads_without_a_subscription_wait_until_adopted() {
        let db = Db::in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO threads (id, agent_id, cwd, title, created_at, updated_at, config_json, last_status)
                 VALUES ('old', 'claude', '/p', 'old', 1, 1, '[]', '{\"kind\":\"idle\"}')",
                [],
            )
            .unwrap();
        assert!(db.load_threads().unwrap().is_empty());
        with_subscription(&db);
        assert_eq!(db.assign_orphan_threads("sub-1").unwrap(), 1);
        assert_eq!(db.load_threads().unwrap()[0].subscription_id, "sub-1");
    }

    #[test]
    fn turns_an_interrupted_thread_dormant_with_a_notice() {
        let db = Db::in_memory().unwrap();
        with_subscription(&db);
        let mut thread = new_thread();
        thread.status = ThreadStatus::Running { started_at: 5 };
        db.save_thread(&thread, []).unwrap();
        let restored = db.load_threads().unwrap().remove(0).restored();
        assert_eq!(restored.status, ThreadStatus::Dormant);
        assert!(matches!(&restored.timeline[0], TimelineItem::Notice { text, .. } if text == INTERRUPTED_NOTICE));
    }

    #[test]
    fn migrating_from_the_previous_version_keeps_existing_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oniko.db");
        {
            let conn = Connection::open(&path).unwrap();
            for sql in &MIGRATIONS[..MIGRATIONS.len() - 1] {
                conn.execute_batch(sql).unwrap();
            }
            conn.execute_batch(&format!("PRAGMA user_version = {}", MIGRATIONS.len() - 1)).unwrap();
            conn.execute_batch(
                "INSERT INTO task_statuses (id, project, name, color, kind, position) VALUES ('s', '/p', 'To do', 'slate', 'unstarted', 1);
                 INSERT INTO tasks (id, project, title, description, status_id, position, created_at, updated_at, number, pull_request, run_json)
                 VALUES ('t', '/p', 'Old task', '', 's', 1, 1, 1, 1, 1, '{\"stage\":\"failed\",\"error\":\"x\"}');",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.user_version().unwrap(), MIGRATIONS.len() as i64);
        let tasks = db.load_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert_eq!((task.title.as_str(), task.pull_request, task.model.as_deref()), ("Old task", true, None));
        assert!(task.blocked_by.is_empty());
        assert_eq!(task.run, Some(TaskRun::Failed { error: "x".into() }));
    }

    #[test]
    fn settings_ignore_values_of_another_shape() {
        let db = Db::in_memory().unwrap();
        db.set_setting("k", &vec!["a".to_string()]).unwrap();
        assert_eq!(db.get_setting::<Vec<String>>("k"), Some(vec!["a".into()]));
        assert_eq!(db.get_setting::<bool>("k"), None);
        assert_eq!(db.get_setting::<bool>("missing"), None);
    }
}
