//! Init subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use serde::Serialize;

use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(store: &str, json: bool, style: Styler) -> anyhow::Result<()> {
    // Checked before init so a re-initialization reports itself as one;
    // the marker check never opens (and locks) the store.
    let existed = sekai_app::local_store_exists(store);
    sekai_app::SekaiInstance::init(store)
        .await
        .with_context(|| format!("init of {store} failed"))?;
    if json {
        println!(
            "{}",
            envelope_ok(
                "init",
                &InitPayload {
                    store: store.to_owned(),
                    created: !existed,
                }
            )?
        );
        return Ok(());
    }
    let what = if existed {
        "Reinitialized existing"
    } else {
        "Initialized empty"
    };
    println!("{what} sekai store in {}", style.bold(store));
    Ok(())
}

#[derive(Serialize)]
struct InitPayload {
    store: String,
    created: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_init_payload() {
        let payload = InitPayload {
            store: ".sekai".to_owned(),
            created: true,
        };
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"store":".sekai","created":true}"#
        );
        let payload = InitPayload {
            store: ".sekai".to_owned(),
            created: false,
        };
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"store":".sekai","created":false}"#
        );
    }
}
