use alloc::{borrow::ToOwned, format, string::String, vec::Vec};
use core::cmp::Ordering;

use crate::parser::Value;

/// Represents a structural change at a specific NBT path.
#[derive(Debug, Clone, PartialEq)]
pub enum NbtChange {
    Added(Value),
    Removed(Value),
    Modified { old: Value, new: Value },
}

/// A single diff entry containing the path and the change.
#[derive(Debug, Clone, PartialEq)]
pub struct NbtDiffEntry {
    /// NBT path (e.g. `"Status"`, `"sections[0].Y"`, `"TileEntities[2].id"`).
    pub path: String,
    pub change: NbtChange,
}

/// Compares two NBT values and produces a list of structural diffs.
///
/// Tags matching any entry in `ignore` are skipped at all depths.
pub fn diff(old: &Value, new: &Value, ignore: &[&str]) -> Vec<NbtDiffEntry> {
    let mut entries = Vec::new();
    diff_values(old, new, "", ignore, &mut entries);
    entries
}

fn diff_values(old: &Value, new: &Value, path: &str, ignore: &[&str], out: &mut Vec<NbtDiffEntry>) {
    if value_eq(old, new) {
        return;
    }

    match (old, new) {
        (Value::Compound(old_entries), Value::Compound(new_entries)) => {
            diff_compounds(old_entries, new_entries, path, ignore, out);
        }
        (Value::List(old_items), Value::List(new_items)) => {
            diff_lists(old_items, new_items, path, ignore, out);
        }

        _ => {
            out.push(NbtDiffEntry {
                path: path.to_owned(),
                change: NbtChange::Modified {
                    old: old.clone(),
                    new: new.clone(),
                },
            });
        }
    }
}

fn diff_compounds(
    old_entries: &[(String, Value)],
    new_entries: &[(String, Value)],
    parent_path: &str,
    ignore: &[&str],
    out: &mut Vec<NbtDiffEntry>,
) {
    let mut i = 0;
    let mut j = 0;

    while i < old_entries.len() && j < new_entries.len() {
        let (old_key, old_val) = &old_entries[i];
        let (new_key, new_val) = &new_entries[j];

        if ignore.contains(&old_key.as_str()) {
            i += 1;
            continue;
        }

        if ignore.contains(&new_key.as_str()) {
            j += 1;
            continue;
        }

        match old_key.cmp(new_key) {
            Ordering::Less => {
                let path = join_path(parent_path, old_key);
                out.push(NbtDiffEntry {
                    path,
                    change: NbtChange::Removed(old_val.clone()),
                });
                i += 1;
            }
            Ordering::Greater => {
                let path = join_path(parent_path, new_key);
                out.push(NbtDiffEntry {
                    path,
                    change: NbtChange::Added(new_val.clone()),
                });
                j += 1;
            }
            Ordering::Equal => {
                let path = join_path(parent_path, old_key);
                diff_values(old_val, new_val, &path, ignore, out);
                i += 1;
                j += 1;
            }
        }
    }

    while i < old_entries.len() {
        let (key, val) = &old_entries[i];
        if !ignore.contains(&key.as_str()) {
            let path = join_path(parent_path, key);
            out.push(NbtDiffEntry {
                path,
                change: NbtChange::Removed(val.clone()),
            });
        }
        i += 1;
    }

    while j < new_entries.len() {
        let (key, val) = &new_entries[j];
        if !ignore.contains(&key.as_str()) {
            let path = join_path(parent_path, key);
            out.push(NbtDiffEntry {
                path,
                change: NbtChange::Added(val.clone()),
            });
        }
        j += 1;
    }
}

fn diff_lists(
    old_items: &[Value],
    new_items: &[Value],
    parent_path: &str,
    ignore: &[&str],
    out: &mut Vec<NbtDiffEntry>,
) {
    let common_len = old_items.len().min(new_items.len());

    for index in 0..common_len {
        let path = format!("{parent_path}[{index}]");
        diff_values(&old_items[index], &new_items[index], &path, ignore, out);
    }

    for (index, item) in old_items.iter().enumerate().skip(common_len) {
        let path = format!("{parent_path}[{index}]");
        out.push(NbtDiffEntry {
            path,
            change: NbtChange::Removed(item.clone()),
        });
    }

    for (index, item) in new_items.iter().enumerate().skip(common_len) {
        let path = format!("{parent_path}[{index}]");
        out.push(NbtDiffEntry {
            path,
            change: NbtChange::Added(item.clone()),
        });
    }
}

fn join_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_owned()
    } else {
        format!("{parent}.{key}")
    }
}

/// Bit-exact equality for floats to correctly handle NaN comparisons.
fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(fa), Value::Float(fb)) => fa.to_bits() == fb.to_bits(),
        (Value::Double(da), Value::Double(db)) => da.to_bits() == db.to_bits(),
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn detects_scalar_modifications() {
        let old = Value::Compound(vec![("a".to_owned(), Value::Int(10))]);
        let new = Value::Compound(vec![("a".to_owned(), Value::Int(20))]);

        let diffs = diff(&old, &new, &[]);
        assert_eq!(
            diffs,
            vec![NbtDiffEntry {
                path: "a".to_owned(),
                change: NbtChange::Modified {
                    old: Value::Int(10),
                    new: Value::Int(20)
                }
            }]
        );
    }

    #[test]
    fn respects_ignore_list() {
        let old = Value::Compound(vec![
            ("InhabitedTime".to_owned(), Value::Long(100)),
            ("Status".to_owned(), Value::String("full".to_owned())),
        ]);
        let new = Value::Compound(vec![
            ("InhabitedTime".to_owned(), Value::Long(200)),
            ("Status".to_owned(), Value::String("full".to_owned())),
        ]);

        let diffs = diff(&old, &new, &["InhabitedTime"]);
        assert!(diffs.is_empty());
    }

    #[test]
    fn handles_nested_compounds_and_lists() {
        let old = Value::Compound(vec![(
            "sections".to_owned(),
            Value::List(vec![Value::Compound(vec![(
                "Y".to_owned(),
                Value::Byte(0),
            )])]),
        )]);
        let new = Value::Compound(vec![(
            "sections".to_owned(),
            Value::List(vec![Value::Compound(vec![(
                "Y".to_owned(),
                Value::Byte(1),
            )])]),
        )]);

        let diffs = diff(&old, &new, &[]);
        assert_eq!(
            diffs,
            vec![NbtDiffEntry {
                path: "sections[0].Y".to_owned(),
                change: NbtChange::Modified {
                    old: Value::Byte(0),
                    new: Value::Byte(1)
                }
            }]
        );
    }

    #[test]
    fn detects_additions_and_removals() {
        let old = Value::Compound(vec![
            ("keep".to_owned(), Value::Int(1)),
            ("remove".to_owned(), Value::Int(2)),
        ]);
        let new = Value::Compound(vec![
            ("add".to_owned(), Value::Int(3)),
            ("keep".to_owned(), Value::Int(1)),
        ]);

        let diffs = diff(&old, &new, &[]);
        assert_eq!(
            diffs,
            vec![
                NbtDiffEntry {
                    path: "add".to_owned(),
                    change: NbtChange::Added(Value::Int(3)),
                },
                NbtDiffEntry {
                    path: "remove".to_owned(),
                    change: NbtChange::Removed(Value::Int(2)),
                }
            ]
        );
    }
}
