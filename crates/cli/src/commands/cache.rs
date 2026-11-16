//! Cache subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use serde::Serialize;

use crate::cli::CacheCommand;
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(store: &str, action: &CacheCommand, style: Styler) -> anyhow::Result<()> {
    let mut instance = sekai_app::SekaiInstance::open(store).await?;

    match action {
        CacheCommand::Clean { json } => {
            let entries = instance
                .clean_cache()
                .await
                .with_context(|| format!("cache clean for {store} failed"))?;
            if *json {
                println!("{}", envelope_ok("cache", &CacheClean { entries })?);
                return Ok(());
            }
            println!(
                "cleared {} cached region image(s)",
                style.bold(&entries.to_string())
            );
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct CacheClean {
    entries: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_cache_clean_payload() {
        assert_eq!(
            serde_json::to_string(&CacheClean { entries: 3 }).unwrap(),
            r#"{"entries":3}"#
        );
    }
}
