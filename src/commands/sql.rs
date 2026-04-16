use anyhow::Result;
use duckdb::Connection;

use crate::output::{self, OutputFormat};

pub fn run(conn: &Connection, query: &str, format: &OutputFormat, wide: bool) -> Result<()> {
    let mut stmt = conn.prepare(query)?;
    output::print_results(&mut stmt, &[], format, wide)
}
