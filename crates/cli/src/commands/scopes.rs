//! Scope subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{Dimension, NamedScope, Rect};
use serde::Serialize;

use crate::cli::{RegionSpec, ScopeCommand};
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(store: &str, action: &ScopeCommand, style: Styler) -> anyhow::Result<()> {
    let mut instance = sekai_app::SekaiInstance::open(store).await?;

    match action {
        ScopeCommand::Create {
            name,
            areas,
            region,
            force,
            json,
        } => {
            let areas = areas
                .iter()
                .map(|spec| (spec.dim, spec.rect))
                .chain(region.iter().map(RegionSpec::area))
                .collect();
            let record = instance
                .create_scope(name, areas, *force)
                .await
                .with_context(|| format!("scope {name} failed"))?;
            if *json {
                println!("{}", envelope_ok("scope", &scope_payload(&record))?);
                return Ok(());
            }
            println!(
                "scope {} covers {}",
                style.bold(record.name.as_str()),
                spell_areas(&record)
            );
        }
        ScopeCommand::Delete { name, json } => {
            let removed = instance
                .delete_scope(name)
                .await
                .with_context(|| format!("delete of scope {name} failed"))?;
            if !removed {
                anyhow::bail!("unknown scope: {name}");
            }
            if *json {
                println!(
                    "{}",
                    envelope_ok(
                        "scope",
                        &ScopeDelete {
                            name: name.as_str().to_owned(),
                        }
                    )?
                );
                return Ok(());
            }
            println!("deleted scope {}", style.bold(name.as_str()));
        }
        ScopeCommand::List { json } => {
            let scopes = instance
                .list_scopes()
                .await
                .with_context(|| format!("scope list for {store} failed"))?;
            if *json {
                println!(
                    "{}",
                    envelope_ok(
                        "scope",
                        &scopes.iter().map(scope_payload).collect::<Vec<_>>()
                    )?
                );
                return Ok(());
            }
            for record in &scopes {
                println!(
                    "{}\t{}",
                    style.bold(record.name.as_str()),
                    spell_areas(record)
                );
            }
        }
    }
    Ok(())
}

/// Every area as the rectangle spelling `--in` accepts
/// (`overworld:0,0..0,0`), in stored order. A chunk or region given at
/// creation is stored as its rectangle, so the original spelling is gone.
fn spell_areas(record: &NamedScope) -> String {
    record
        .areas()
        .iter()
        .map(|&(dim, rect)| spell_area(dim, rect))
        .collect::<Vec<_>>()
        .join(" ")
}

fn spell_area(dim: Dimension, rect: Rect) -> String {
    format!("{dim}:{},{}..{},{}", rect.x0, rect.z0, rect.x1, rect.z1)
}

#[derive(Serialize)]
struct ScopeJson {
    name: String,
    areas: Vec<AreaJson>,
    created_at_ms: u64,
}

/// One area with the raw integer `dim` code, as `diff` coordinates carry it.
#[derive(Serialize)]
struct AreaJson {
    dim: i32,
    x0: i32,
    z0: i32,
    x1: i32,
    z1: i32,
}

#[derive(Serialize)]
struct ScopeDelete {
    name: String,
}

fn scope_payload(record: &NamedScope) -> ScopeJson {
    ScopeJson {
        name: record.name.as_str().to_owned(),
        areas: record
            .areas()
            .iter()
            .map(|&(dim, rect)| AreaJson {
                dim: dim.raw(),
                x0: rect.x0,
                z0: rect.z0,
                x1: rect.x1,
                z1: rect.z1,
            })
            .collect(),
        created_at_ms: record.created_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_app::ScopeName;

    fn record() -> NamedScope {
        NamedScope::new(
            ScopeName::parse("spawn").unwrap(),
            vec![
                (Dimension::NETHER, Rect::new(-1, -2, 3, 4)),
                (Dimension::OVERWORLD, Rect::new(5, 6, 5, 6)),
            ],
            1_700_000_000_000,
        )
        .unwrap()
    }

    #[test]
    fn renders_scope_payloads() {
        assert_eq!(
            serde_json::to_string(&scope_payload(&record())).unwrap(),
            format!(
                r#"{{"name":"spawn","areas":[{{"dim":{},"x0":5,"z0":6,"x1":5,"z1":6}},{{"dim":{},"x0":-1,"z0":-2,"x1":3,"z1":4}}],"created_at_ms":1700000000000}}"#,
                Dimension::OVERWORLD.raw(),
                Dimension::NETHER.raw(),
            )
        );
    }

    #[test]
    fn spells_areas_as_rectangles_in_stored_order() {
        assert_eq!(
            spell_areas(&record()),
            "overworld:5,6..5,6 nether:-1,-2..3,4"
        );
    }
}
