use anyhow::Result;
use work_tracker::{
    domain::Status,
    ledger::{LedgerConfig, ListFilter},
};

#[test]
fn sqlite_lifecycle_is_available_through_the_ledger_seam() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let config = LedgerConfig::sqlite(directory.path().join("tracker.db"));
    let mut ledger = config.open()?;

    let item = ledger.create(
        "Watch CI",
        Some("Wait for the queued suite"),
        Status::Pending,
        "agent-a",
        None,
    )?;
    ledger.set_status(item.id, Status::Waiting, "agent-a", Some("CI queued"))?;

    assert_eq!(ledger.get(item.id)?.status, Status::Waiting);
    assert_eq!(ledger.list(ListFilter::Actionable, false, 100)?.len(), 1);
    assert_eq!(ledger.history(item.id)?.len(), 2);
    Ok(())
}
