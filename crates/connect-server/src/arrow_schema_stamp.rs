//! Rewrite DuckDB-produced Arrow schema field names to match τ's analyzer
//! `resolved_schema` view. Data buffers unchanged — this is a metadata-only
//! swap at the ExecutePlan response boundary.
//!
//! Motivation (arr-012, diagnostic pass 88): τ's analyzer records
//! `Struct<tags, tags>` for `arrays_zip("tags", "tags")` (duplicates legal in
//! Spark's `StructType`), but DuckDB rejects every duplicate-struct-field
//! substrate — `CAST(... AS STRUCT("tags" VARCHAR, "tags" VARCHAR))` and
//! `{tags:'a', tags:'b'}` both raise `Binder Error: Duplicate STRUCT type
//! argument name`. τ's emission consequently falls back to positional field
//! names (`struct_pack("0" := …, "1" := …)`) — correct data buffers, but the
//! resulting Arrow schema disagrees with τ's `resolved_schema`. PySpark's
//! `createDataFrame(rows, schema)` then invents disambiguated names
//! (`tags_0`, `tags_1`). The stamp reconciles both views once, at the wire
//! boundary.
//!
//! **Bijection contract.** τ's `resolved_schema` and DuckDB's Arrow schema
//! MUST have structurally identical shape (same primitives at same tree
//! positions, same nesting depth). They may differ ONLY on `Field` names at
//! any nesting level. On a structural mismatch the stamp
//! `debug_assert!`s (debug) or soft-falls back to the original batches +
//! `tracing::warn!` (release).
//!
//! **Duplicate name handling.** Spark's `df.schema` (from AnalyzePlan)
//! preserves duplicate struct field names (`Struct<tags, tags>` from
//! `arrays_zip("tags","tags")`), but Spark's Arrow **wire** schema uses
//! disambiguated names (`Struct<tags_0, tags_1>`) — the reference server
//! runs `_deduplicate_field_names` before Arrow serialization
//! (`pyspark/sql/pandas/types.py::_dedup_names`). PySpark's client-side
//! `ArrowTableToRowsConversion.convert` then re-dedups `df.schema.names`
//! and pairs the dedup'd names against the Arrow column dicts. The stamp
//! must match this contract: dedup within every `Struct` level (top-level
//! `Schema` included) so pyarrow's `Array.to_pylist()` succeeds and the
//! dict keys line up with `_dedup_names(df.schema.names)`.

use std::sync::Arc;

use arrow::array::RecordBatch;
use arrow::datatypes::{DataType as ArrowDt, Field, Fields, Schema};
use arrow::record_batch::RecordBatchOptions;
use thunderduck_core::types::pyspark_parity::dedup_names;
use thunderduck_core::types::{DataType as TdckDt, StructType as TdckStruct};

/// A structural mismatch between τ's analyzer `resolved_schema` and the
/// DuckDB-produced Arrow schema at some position in the schema tree.
///
/// Populated by [`rewrite_top_schema`] and consumed by [`stamp_batch_schemas`]
/// (which converts it into a `debug_assert!` in debug builds and a
/// `tracing::warn!` + soft-fallback in release).
#[derive(Debug, Clone)]
struct SchemaShapeMismatch {
    /// Dot-separated path from the root to the mismatch (e.g. `z[].tags`).
    path: String,
    /// Textual dump of τ's `DataType` (or shape descriptor) at `path`.
    tdck: String,
    /// Textual dump of Arrow's `DataType` (or shape descriptor) at `path`.
    arrow: String,
}

/// Rewrite each batch's Arrow schema so field names at every nested position
/// match τ's `resolved_schema`. Data buffers are unchanged — the rebuilt
/// [`RecordBatch`] shares the same underlying `ArrayData` as the input.
///
/// On structural mismatch (length disagreement, or a compound where τ expects
/// e.g. `Struct` but Arrow has a primitive) the stamp `debug_assert!`s in
/// debug builds and soft-falls back to the original batches + `tracing::warn!`
/// in release — no prior-green corpus case may regress. Any regression
/// surfaces the exact mismatch path in the log so the fix can be traced back
/// to the τ analyzer.
pub fn stamp_batch_schemas(
    batches: Vec<RecordBatch>,
    resolved_schema: &TdckStruct,
) -> Vec<RecordBatch> {
    if batches.is_empty() {
        return batches;
    }

    let arrow_schema = batches[0].schema();
    let rebuilt = match rewrite_top_schema(&arrow_schema, resolved_schema) {
        Ok(schema) => Arc::new(schema),
        Err(mm) => {
            debug_assert!(
                false,
                "arrow_schema_stamp: shape mismatch at `{}` — tdck={} arrow={}",
                mm.path, mm.tdck, mm.arrow,
            );
            tracing::warn!(
                path = %mm.path,
                tdck = %mm.tdck,
                arrow = %mm.arrow,
                "arrow_schema_stamp: shape mismatch — falling back to DuckDB-produced Arrow schema"
            );
            return batches;
        }
    };

    // Rebuild each batch with the stamped schema. `match_field_names(false)`
    // makes `try_new_with_options` compare column and field data types via
    // `equals_datatype`, which ignores nested field names — required because
    // the stamp only changes names, not shapes.
    let mut out = Vec::with_capacity(batches.len());
    for batch in &batches {
        let opts = RecordBatchOptions::new()
            .with_match_field_names(false)
            .with_row_count(Some(batch.num_rows()));
        match RecordBatch::try_new_with_options(
            Arc::clone(&rebuilt),
            batch.columns().to_vec(),
            &opts,
        ) {
            Ok(new_batch) => out.push(new_batch),
            Err(err) => {
                debug_assert!(
                    false,
                    "arrow_schema_stamp: RecordBatch::try_new_with_options failed after \
                     shape-compatible rewrite: {err}"
                );
                tracing::warn!(
                    error = %err,
                    "arrow_schema_stamp: batch reconstruction failed — falling back to \
                     DuckDB-produced Arrow schema",
                );
                return batches;
            }
        }
    }
    out
}

/// Rewrite one Arrow `Schema` against τ's row `StructType`.
///
/// Emits a new `Schema` whose top-level field names come from `tdck`; nested
/// field names at every depth are rewritten by [`rewrite_data_type`].
/// Preserves the input schema's metadata unchanged.
fn rewrite_top_schema(arrow: &Schema, tdck: &TdckStruct) -> Result<Schema, SchemaShapeMismatch> {
    let arrow_fields = arrow.fields();
    if arrow_fields.len() != tdck.fields.len() {
        return Err(SchemaShapeMismatch {
            path: "<root>".to_owned(),
            tdck: format!("<{} fields>", tdck.fields.len()),
            arrow: format!("<{} columns>", arrow_fields.len()),
        });
    }
    // PySpark-parity dedup: `ArrowTableToRowsConversion.convert` looks up
    // struct field values by `_dedup_names(schema.names)`, so the Arrow-wire
    // names MUST be dedup'd at every struct level (top-level Schema included).
    let tdck_names: Vec<&str> = tdck.fields.iter().map(|f| f.name.as_str()).collect();
    let wire_names = dedup_names(&tdck_names);
    let mut new_fields: Vec<Arc<Field>> = Vec::with_capacity(arrow_fields.len());
    for ((arrow_field, tdck_field), wire_name) in arrow_fields
        .iter()
        .zip(tdck.fields.iter())
        .zip(wire_names.iter())
    {
        let rebuilt = rewrite_field(
            wire_name,
            arrow_field.as_ref(),
            wire_name,
            &tdck_field.data_type,
        )?;
        new_fields.push(Arc::new(rebuilt));
    }
    Ok(Schema::new(new_fields).with_metadata(arrow.metadata.clone()))
}

/// Rebuild one Arrow `Field` at position `path` so its `name` matches
/// `tdck_name` and its `DataType` matches τ's `tdck_ty` shape (with nested
/// names rewritten). Preserves Arrow's `is_nullable()` and per-field
/// metadata — the stamp is a name-only transformation.
fn rewrite_field(
    path: &str,
    arrow: &Field,
    tdck_name: &str,
    tdck_ty: &TdckDt,
) -> Result<Field, SchemaShapeMismatch> {
    let new_dt = rewrite_data_type(path, arrow.data_type(), tdck_ty)?;
    Ok(Field::new(tdck_name, new_dt, arrow.is_nullable()).with_metadata(arrow.metadata().clone()))
}

/// Rebuild one Arrow `DataType` at position `path` so nested `Struct` /
/// `List` / `Map` field names match τ's `tdck_ty`. Primitives are returned
/// unchanged (`Arrow` type is preserved verbatim); the compound arms recurse
/// pairwise into their children.
///
/// The `match` on τ's [`TdckDt`] is exhaustive — new variants must be
/// handled explicitly so latent shapes cannot silently pass through.
fn rewrite_data_type(
    path: &str,
    arrow: &ArrowDt,
    tdck: &TdckDt,
) -> Result<ArrowDt, SchemaShapeMismatch> {
    match tdck {
        // Primitives — no structural recursion, preserve Arrow's data type
        // verbatim (buffer layout must not change). The rename happens on
        // the enclosing Field (see `rewrite_field`).
        TdckDt::Boolean
        | TdckDt::Byte
        | TdckDt::Short
        | TdckDt::Integer
        | TdckDt::Long
        | TdckDt::Float
        | TdckDt::Double
        | TdckDt::Decimal { .. }
        | TdckDt::String
        | TdckDt::Binary
        | TdckDt::Date
        | TdckDt::Timestamp
        | TdckDt::TimestampNtz
        | TdckDt::YearMonthInterval
        | TdckDt::DayTimeInterval
        | TdckDt::Interval
        | TdckDt::Null => Ok(arrow.clone()),

        // `Unresolved` should never survive to the response path — the
        // AnalyzePlan boundary guard at `service.rs` already rejects it. If
        // we see one here, τ's analyzer skipped a resolution step: treat as
        // a shape mismatch (soft-fallback in release, panic in debug).
        TdckDt::Unresolved => Err(SchemaShapeMismatch {
            path: path.to_owned(),
            tdck: "Unresolved".to_owned(),
            arrow: format!("{arrow:?}"),
        }),

        TdckDt::Array(elem_ty, _contains_null) => match arrow {
            ArrowDt::List(inner)
            | ArrowDt::LargeList(inner)
            | ArrowDt::ListView(inner)
            | ArrowDt::LargeListView(inner) => {
                let child_path = format!("{path}[]");
                let new_inner_dt = rewrite_data_type(&child_path, inner.data_type(), elem_ty)?;
                let new_inner = Arc::new(
                    Field::new(inner.name(), new_inner_dt, inner.is_nullable())
                        .with_metadata(inner.metadata().clone()),
                );
                let wrapped = match arrow {
                    ArrowDt::List(_) => ArrowDt::List(new_inner),
                    ArrowDt::LargeList(_) => ArrowDt::LargeList(new_inner),
                    ArrowDt::ListView(_) => ArrowDt::ListView(new_inner),
                    ArrowDt::LargeListView(_) => ArrowDt::LargeListView(new_inner),
                    _ => unreachable!("outer match narrowed to list variants"),
                };
                Ok(wrapped)
            }
            ArrowDt::FixedSizeList(inner, size) => {
                let child_path = format!("{path}[]");
                let new_inner_dt = rewrite_data_type(&child_path, inner.data_type(), elem_ty)?;
                let new_inner = Arc::new(
                    Field::new(inner.name(), new_inner_dt, inner.is_nullable())
                        .with_metadata(inner.metadata().clone()),
                );
                Ok(ArrowDt::FixedSizeList(new_inner, *size))
            }
            _ => Err(SchemaShapeMismatch {
                path: path.to_owned(),
                tdck: format!("Array<{elem_ty}>"),
                arrow: format!("{arrow:?}"),
            }),
        },

        TdckDt::Map { key, value, .. } => match arrow {
            ArrowDt::Map(entries_field, sorted) => match entries_field.data_type() {
                ArrowDt::Struct(entry_fields) if entry_fields.len() == 2 => {
                    let key_arrow = &entry_fields[0];
                    let val_arrow = &entry_fields[1];
                    let new_key_dt =
                        rewrite_data_type(&format!("{path}.key"), key_arrow.data_type(), key)?;
                    let new_val_dt =
                        rewrite_data_type(&format!("{path}.value"), val_arrow.data_type(), value)?;
                    let new_entry_fields = Fields::from(vec![
                        Arc::new(
                            Field::new(key_arrow.name(), new_key_dt, key_arrow.is_nullable())
                                .with_metadata(key_arrow.metadata().clone()),
                        ),
                        Arc::new(
                            Field::new(val_arrow.name(), new_val_dt, val_arrow.is_nullable())
                                .with_metadata(val_arrow.metadata().clone()),
                        ),
                    ]);
                    let new_entries = Arc::new(
                        Field::new(
                            entries_field.name(),
                            ArrowDt::Struct(new_entry_fields),
                            entries_field.is_nullable(),
                        )
                        .with_metadata(entries_field.metadata().clone()),
                    );
                    Ok(ArrowDt::Map(new_entries, *sorted))
                }
                _ => Err(SchemaShapeMismatch {
                    path: path.to_owned(),
                    tdck: format!("Map<{key}, {value}>"),
                    arrow: format!("{arrow:?}"),
                }),
            },
            _ => Err(SchemaShapeMismatch {
                path: path.to_owned(),
                tdck: format!("Map<{key}, {value}>"),
                arrow: format!("{arrow:?}"),
            }),
        },

        TdckDt::Struct(inner_struct) => match arrow {
            ArrowDt::Struct(arrow_fields) => {
                if arrow_fields.len() != inner_struct.fields.len() {
                    return Err(SchemaShapeMismatch {
                        path: path.to_owned(),
                        tdck: format!("Struct<{} fields>", inner_struct.fields.len()),
                        arrow: format!("Struct<{} fields>", arrow_fields.len()),
                    });
                }
                // Dedup nested struct field names for wire parity with Spark
                // (see module doc). PyArrow's `Array.to_pylist()` and
                // `StructScalar.as_py()` refuse duplicate field names; the
                // Spark-visible duplicates live only in `df.schema` via
                // AnalyzePlan, and PySpark's `ArrowTableToRowsConversion`
                // pairs `_dedup_names(schema.names)` against the wire dict.
                let tdck_names: Vec<&str> = inner_struct
                    .fields
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect();
                let wire_names = dedup_names(&tdck_names);
                let mut new_children: Vec<Arc<Field>> = Vec::with_capacity(arrow_fields.len());
                for ((arrow_child, tdck_child), wire_name) in arrow_fields
                    .iter()
                    .zip(inner_struct.fields.iter())
                    .zip(wire_names.iter())
                {
                    let child_path = format!("{path}.{}", tdck_child.name);
                    let rebuilt = rewrite_field(
                        &child_path,
                        arrow_child.as_ref(),
                        wire_name,
                        &tdck_child.data_type,
                    )?;
                    new_children.push(Arc::new(rebuilt));
                }
                Ok(ArrowDt::Struct(Fields::from(new_children)))
            }
            _ => Err(SchemaShapeMismatch {
                path: path.to_owned(),
                tdck: format!("Struct<{} fields>", inner_struct.fields.len()),
                arrow: format!("{arrow:?}"),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, Int64Array, ListArray, StringArray, StructArray};
    use arrow::buffer::OffsetBuffer;
    use thunderduck_core::types::StructField as TdckField;

    /// Convenience: build a τ `StructType` from a list of `(name, dt, nullable)`.
    fn tdck_struct(fields: Vec<(&str, TdckDt, bool)>) -> TdckStruct {
        TdckStruct::new(
            fields
                .into_iter()
                .map(|(n, dt, nullable)| TdckField::new(n, dt, nullable))
                .collect(),
        )
    }

    /// Convenience: single-column `RecordBatch` from a schema + column.
    fn batch_of(schema: Arc<Schema>, column: ArrayRef) -> RecordBatch {
        RecordBatch::try_new(schema, vec![column]).expect("test batch construction must succeed")
    }

    // ── 1. Primitive passthrough ────────────────────────────────────────────

    #[test]
    fn stamp_primitive_column_is_identity() {
        let arrow_schema = Arc::new(Schema::new(vec![Field::new("x", ArrowDt::Int64, true)]));
        let col: ArrayRef = Arc::new(Int64Array::from(vec![1, 2, 3]));
        let input = batch_of(Arc::clone(&arrow_schema), col);
        let tdck = tdck_struct(vec![("x", TdckDt::Long, true)]);

        let out = stamp_batch_schemas(vec![input.clone()], &tdck);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].schema().as_ref(), arrow_schema.as_ref());
        // Data preserved.
        let col_out = out[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("Int64 column");
        assert_eq!(col_out.values(), &[1, 2, 3]);
    }

    // ── 2. Struct field rename ──────────────────────────────────────────────

    #[test]
    fn stamp_renames_struct_children_from_positional_to_named() {
        // Arrow: {out: Struct<0: Int64, 1: Int64>} — DuckDB's substrate view.
        let inner_arrow = Fields::from(vec![
            Arc::new(Field::new("0", ArrowDt::Int64, true)),
            Arc::new(Field::new("1", ArrowDt::Int64, true)),
        ]);
        let arrow_schema = Arc::new(Schema::new(vec![Field::new(
            "out",
            ArrowDt::Struct(inner_arrow.clone()),
            true,
        )]));
        let struct_arr = StructArray::try_new(
            inner_arrow,
            vec![
                Arc::new(Int64Array::from(vec![10])) as ArrayRef,
                Arc::new(Int64Array::from(vec![20])) as ArrayRef,
            ],
            None,
        )
        .expect("StructArray");
        let input = batch_of(Arc::clone(&arrow_schema), Arc::new(struct_arr));

        // τ: {out: Struct<a: Long, b: Long>} — Spark-visible view.
        let tdck = tdck_struct(vec![(
            "out",
            TdckDt::Struct(tdck_struct(vec![
                ("a", TdckDt::Long, true),
                ("b", TdckDt::Long, true),
            ])),
            true,
        )]);

        let out = stamp_batch_schemas(vec![input], &tdck);

        let out_schema = out[0].schema();
        match out_schema.field(0).data_type() {
            ArrowDt::Struct(inner) => {
                assert_eq!(inner[0].name(), "a");
                assert_eq!(inner[1].name(), "b");
            }
            other => panic!("expected Struct, got {other:?}"),
        }
    }

    // ── 3. Duplicate struct field names (the arr-012 shape) ─────────────────

    #[test]
    fn stamp_supports_duplicate_struct_field_names_arr012_shape() {
        // Arrow: {z: List<Struct<0: Utf8, 1: Utf8>>} — DuckDB's arrays_zip output.
        let struct_fields = Fields::from(vec![
            Arc::new(Field::new("0", ArrowDt::Utf8, true)),
            Arc::new(Field::new("1", ArrowDt::Utf8, true)),
        ]);
        let list_inner = Arc::new(Field::new(
            "l",
            ArrowDt::Struct(struct_fields.clone()),
            true,
        ));
        let arrow_schema = Arc::new(Schema::new(vec![Field::new_list("z", list_inner, true)]));

        // Build a minimal one-row List<Struct> value: outer offset [0, 1],
        // inner struct with one row of ("a", "b").
        let struct_arr = StructArray::try_new(
            struct_fields,
            vec![
                Arc::new(StringArray::from(vec!["a"])) as ArrayRef,
                Arc::new(StringArray::from(vec!["b"])) as ArrayRef,
            ],
            None,
        )
        .expect("inner struct");
        let offsets = OffsetBuffer::<i32>::new(vec![0, 1].into());
        let list_field = Arc::new(Field::new(
            "l",
            arrow::array::Array::data_type(&struct_arr).clone(),
            true,
        ));
        let list_arr = ListArray::new(list_field, offsets, Arc::new(struct_arr), None);
        let input = batch_of(Arc::clone(&arrow_schema), Arc::new(list_arr));

        // τ: {z: Array<Struct<tags: String, tags: String>>} — duplicates legal.
        let tdck = tdck_struct(vec![(
            "z",
            TdckDt::Array(
                Box::new(TdckDt::Struct(tdck_struct(vec![
                    ("tags", TdckDt::String, true),
                    ("tags", TdckDt::String, true),
                ]))),
                true,
            ),
            true,
        )]);

        let out = stamp_batch_schemas(vec![input], &tdck);

        let out_schema = out[0].schema();
        let list_dt = out_schema.field(0).data_type();
        let inner_dt = match list_dt {
            ArrowDt::List(inner) => inner.data_type(),
            other => panic!("expected List, got {other:?}"),
        };
        // Wire names are dedup'd (`tags_0`, `tags_1`) — Spark's Arrow-wire
        // contract. `df.schema` (from AnalyzePlan) retains the duplicates.
        match inner_dt {
            ArrowDt::Struct(fields) => {
                assert_eq!(fields[0].name(), "tags_0");
                assert_eq!(fields[1].name(), "tags_1");
            }
            other => panic!("expected Struct inside List, got {other:?}"),
        }
    }

    // ── 4. Nested List<Struct<List<Struct>>> ────────────────────────────────

    #[test]
    fn stamp_recurses_through_nested_list_of_struct_of_list_of_struct() {
        // Build the innermost Struct<0: Int64> Arrow shape.
        let inner_struct_fields =
            Fields::from(vec![Arc::new(Field::new("0", ArrowDt::Int64, true))]);
        let inner_list_field = Arc::new(Field::new(
            "l",
            ArrowDt::Struct(inner_struct_fields.clone()),
            true,
        ));
        let mid_struct_fields = Fields::from(vec![Arc::new(Field::new(
            "0",
            ArrowDt::List(inner_list_field.clone()),
            true,
        ))]);
        let outer_list_field = Arc::new(Field::new(
            "l",
            ArrowDt::Struct(mid_struct_fields.clone()),
            true,
        ));
        let arrow_schema = Arc::new(Schema::new(vec![Field::new_list(
            "root",
            outer_list_field,
            true,
        )]));

        // Build a minimal single-row nested batch. Innermost struct has one
        // row {0: 7}; wrap as List(len=1), then Struct{0: List(...)},
        // then List(len=1).
        let innermost_struct = StructArray::try_new(
            inner_struct_fields,
            vec![Arc::new(Int64Array::from(vec![7])) as ArrayRef],
            None,
        )
        .expect("innermost struct");
        let inner_list = ListArray::new(
            inner_list_field,
            OffsetBuffer::<i32>::new(vec![0, 1].into()),
            Arc::new(innermost_struct),
            None,
        );
        let mid_struct = StructArray::try_new(
            mid_struct_fields.clone(),
            vec![Arc::new(inner_list) as ArrayRef],
            None,
        )
        .expect("mid struct");
        let outer_list_inner_field =
            Arc::new(Field::new("l", ArrowDt::Struct(mid_struct_fields), true));
        let outer_list = ListArray::new(
            outer_list_inner_field,
            OffsetBuffer::<i32>::new(vec![0, 1].into()),
            Arc::new(mid_struct),
            None,
        );
        let input = batch_of(Arc::clone(&arrow_schema), Arc::new(outer_list));

        // τ: {root: Array<Struct<mid: Array<Struct<inner: Long>>>>}
        let tdck = tdck_struct(vec![(
            "root",
            TdckDt::Array(
                Box::new(TdckDt::Struct(tdck_struct(vec![(
                    "mid",
                    TdckDt::Array(
                        Box::new(TdckDt::Struct(tdck_struct(vec![(
                            "inner",
                            TdckDt::Long,
                            true,
                        )]))),
                        true,
                    ),
                    true,
                )]))),
                true,
            ),
            true,
        )]);

        let out = stamp_batch_schemas(vec![input], &tdck);
        let root_dt = out[0].schema().field(0).data_type().clone();
        let mid_struct_dt = match root_dt {
            ArrowDt::List(f) => f.data_type().clone(),
            other => panic!("expected root List, got {other:?}"),
        };
        let mid_list_dt = match mid_struct_dt {
            ArrowDt::Struct(fields) => {
                assert_eq!(fields[0].name(), "mid");
                fields[0].data_type().clone()
            }
            other => panic!("expected root Struct, got {other:?}"),
        };
        let inner_struct_dt = match mid_list_dt {
            ArrowDt::List(f) => f.data_type().clone(),
            other => panic!("expected mid List, got {other:?}"),
        };
        match inner_struct_dt {
            ArrowDt::Struct(fields) => {
                assert_eq!(fields[0].name(), "inner");
            }
            other => panic!("expected inner Struct, got {other:?}"),
        }
    }

    // ── 5. Map traversal ────────────────────────────────────────────────────

    #[test]
    fn stamp_renames_inside_map_value_struct() {
        // Arrow: {m: Map<key: Utf8, value: Struct<0: Int64>>}
        let val_struct_fields = Fields::from(vec![Arc::new(Field::new("0", ArrowDt::Int64, true))]);
        let entries_fields = Fields::from(vec![
            Arc::new(Field::new("key", ArrowDt::Utf8, false)),
            Arc::new(Field::new(
                "value",
                ArrowDt::Struct(val_struct_fields),
                true,
            )),
        ]);
        let entries_field = Arc::new(Field::new(
            "entries",
            ArrowDt::Struct(entries_fields),
            false,
        ));
        let arrow_schema = Arc::new(Schema::new(vec![Field::new(
            "m",
            ArrowDt::Map(entries_field.clone(), false),
            true,
        )]));

        // τ: {m: Map<String, Struct<a: Long>>}
        let tdck = tdck_struct(vec![(
            "m",
            TdckDt::Map {
                key: Box::new(TdckDt::String),
                value: Box::new(TdckDt::Struct(tdck_struct(vec![("a", TdckDt::Long, true)]))),
                value_nullable: true,
            },
            true,
        )]);

        // Only exercise the schema-rewrite path (no data buffer needed here —
        // the paired data-buffer test lives in test #7).
        let rebuilt = rewrite_top_schema(arrow_schema.as_ref(), &tdck)
            .expect("map schema rewrite must succeed");
        let map_dt = rebuilt.field(0).data_type();
        let entries = match map_dt {
            ArrowDt::Map(f, _) => f.data_type(),
            other => panic!("expected Map, got {other:?}"),
        };
        match entries {
            ArrowDt::Struct(entry_fields) => {
                // Arrow key/value wrapper names are preserved by convention.
                assert_eq!(entry_fields[0].name(), "key");
                assert_eq!(entry_fields[1].name(), "value");
                match entry_fields[1].data_type() {
                    ArrowDt::Struct(val_fields) => {
                        assert_eq!(val_fields[0].name(), "a");
                    }
                    other => panic!("expected value Struct, got {other:?}"),
                }
            }
            other => panic!("expected Map entries Struct, got {other:?}"),
        }
    }

    // ── 6. Structural mismatch → soft-fallback (Err from rewrite_top_schema) ─

    #[test]
    fn rewrite_top_schema_returns_err_on_length_mismatch() {
        let arrow_schema = Schema::new(vec![Field::new("x", ArrowDt::Int64, true)]);
        let tdck = tdck_struct(vec![("x", TdckDt::Long, true), ("y", TdckDt::Long, true)]);
        let err = rewrite_top_schema(&arrow_schema, &tdck)
            .expect_err("length mismatch must surface as Err");
        assert_eq!(err.path, "<root>");
    }

    #[test]
    fn rewrite_top_schema_returns_err_on_compound_vs_primitive() {
        // τ says Struct at position "out", Arrow has a primitive Int64.
        let arrow_schema = Schema::new(vec![Field::new("out", ArrowDt::Int64, true)]);
        let tdck = tdck_struct(vec![(
            "out",
            TdckDt::Struct(tdck_struct(vec![("a", TdckDt::Long, true)])),
            true,
        )]);
        let err = rewrite_top_schema(&arrow_schema, &tdck)
            .expect_err("compound-vs-primitive mismatch must surface as Err");
        assert!(
            err.path.starts_with("out"),
            "expected path to identify the offending field; got: {}",
            err.path
        );
        assert!(err.tdck.contains("Struct"));
    }

    /// In debug builds, `stamp_batch_schemas` must trip a `debug_assert!` on
    /// structural mismatch so τ analyzer bugs surface during `cargo test` —
    /// pins the loud-fail contract from the architecture plan.
    #[test]
    #[should_panic(expected = "shape mismatch")]
    fn stamp_debug_asserts_on_structural_mismatch() {
        let arrow_schema = Arc::new(Schema::new(vec![Field::new("x", ArrowDt::Int64, true)]));
        let col: ArrayRef = Arc::new(Int64Array::from(vec![1]));
        let input = batch_of(arrow_schema, col);
        // Extra tdck field vs. Arrow — forces the top-schema length check.
        let tdck = tdck_struct(vec![("x", TdckDt::Long, true), ("y", TdckDt::Long, true)]);
        let _ = stamp_batch_schemas(vec![input], &tdck);
    }

    // ── 7. Data buffer preservation ─────────────────────────────────────────

    #[test]
    fn stamp_preserves_data_buffers() {
        let arrow_schema = Arc::new(Schema::new(vec![Field::new("x", ArrowDt::Int64, true)]));
        let col: ArrayRef = Arc::new(Int64Array::from(vec![1, 2, 3, 4, 5]));
        let input = batch_of(Arc::clone(&arrow_schema), Arc::clone(&col));

        // Rename x → renamed_x via τ.
        let tdck = tdck_struct(vec![("renamed_x", TdckDt::Long, true)]);
        let out = stamp_batch_schemas(vec![input.clone()], &tdck);

        assert_eq!(out[0].schema().field(0).name(), "renamed_x");
        let col_out = out[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("Int64 column preserved");
        assert_eq!(col_out.values(), &[1, 2, 3, 4, 5]);
        // Buffer identity: the column pointer is the same Arc (no clone-copy).
        assert!(
            Arc::ptr_eq(&col, &out[0].column(0).clone()),
            "stamp must be a metadata-only swap — column Arc identity preserved"
        );
    }

    // ── Empty-input sanity ───────────────────────────────────────────────────

    #[test]
    fn stamp_empty_batch_list_is_noop() {
        let tdck = tdck_struct(vec![("x", TdckDt::Long, true)]);
        let out = stamp_batch_schemas(vec![], &tdck);
        assert!(out.is_empty());
    }
}
