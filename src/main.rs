mod error;

pub use error::ChBackupError;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    println!("chbackup - ClickHouse backup tool");
    Ok(())
}
