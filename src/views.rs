use anyhow::Result;
use duckdb::Connection;
use std::path::PathBuf;

pub fn register_views(_conn: &Connection, _files: &[PathBuf]) -> Result<()> {
    Ok(())
}
