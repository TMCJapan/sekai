//! Init subcommand execution, DTOs, and output rendering.

use serde::Serialize;
use std::path::Path;

use crate::envelope::Printer;

/// Register the world at `world` in the store at `store`, or at the
/// world's `.sekai` (ADR-0035 *`init`*).
pub async fn run(
    world: impl AsRef<Path>,
    store: Option<impl AsRef<Path>>,
    printer: &Printer,
) -> anyhow::Result<()> {
    let world = world.as_ref();
    let initialized = sekai_app::SekaiInstance::init_world(world, store).await?;
    let store = initialized.store.display().to_string();
    if printer.json {
        printer.print_ok(
            "init",
            &InitPayload {
                store,
                created: initialized.created,
            },
        )?;
        return Ok(());
    }
    let what = if initialized.created {
        "Initialized empty"
    } else {
        "Reinitialized existing"
    };
    println!("{what} sekai store in {}", printer.style.bold(&store));
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
