//! Loads an existing oniko.db and prints what it holds: a quick check that a database written by
//! the Electron app reads back intact. `cargo run -p oniko-core --example read_db -- path/to/oniko.db`
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("pass the path to oniko.db");
    let db = oniko_core::db::Db::open(std::path::Path::new(&path))?;
    let threads = db.load_threads()?;
    let items: usize = threads.iter().map(|t| t.timeline.len()).sum();
    println!("schema version {}", db.user_version()?);
    println!("{} threads, {items} timeline items", threads.len());
    println!("{} subscriptions", db.load_subscriptions()?.len());
    println!("{} task statuses, {} tasks", db.load_task_statuses()?.len(), db.load_tasks()?.len());
    Ok(())
}
