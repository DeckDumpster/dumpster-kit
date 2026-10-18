//! `catalog.prices` and `catalog.sealed_prices`, as `pkdump_lake.prices`
//! created them.
//!
//! Field ids, names, types, requiredness and the identity partition on
//! `observed_date` (partition field id 1000) are copied, not redesigned: the
//! tables already exist in Nessie and a schema that differs by an id would be a
//! schema evolution this crate has no business causing.

use std::sync::Arc;

use arrow_array::{ArrayRef, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use iceberg::arrow::schema_to_arrow_schema;
use iceberg::expr::Reference;
use iceberg::spec::{
    DataFileFormat, Datum, Literal, NestedField, PartitionKey, PartitionSpec, PrimitiveType,
    Schema, Struct, Transform, Type, UnboundPartitionField, UnboundPartitionSpec,
};
use iceberg::transaction::{ApplyTransactionAction, Transaction};
use iceberg::writer::base_writer::data_file_writer::DataFileWriterBuilder;
use iceberg::writer::file_writer::ParquetWriterBuilder;
use iceberg::writer::file_writer::location_generator::{
    DefaultFileNameGenerator, DefaultLocationGenerator,
};
use iceberg::writer::file_writer::rolling_writer::RollingFileWriterBuilder;
use iceberg::writer::{IcebergWriter, IcebergWriterBuilder};
use iceberg::{Catalog, NamespaceIdent, TableCreation, TableIdent};
use parquet::file::properties::WriterProperties;

use crate::{Error, Result};

pub const NAMESPACE: &str = "catalog";
pub const PRICES_TABLE: &str = "prices";
pub const SEALED_TABLE: &str = "sealed_prices";

/// One `catalog.prices` row.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceRow {
    pub tcgplayer_product_id: i64,
    pub sub_type_name: String,
    pub price_type: String,
    pub price: f64,
}

/// One `catalog.sealed_prices` row — no `sub_type_name`.
#[derive(Debug, Clone, PartialEq)]
pub struct SealedPriceRow {
    pub tcgplayer_product_id: i64,
    pub price_type: String,
    pub price: f64,
}

fn req(id: i32, name: &str, ty: PrimitiveType) -> Arc<NestedField> {
    Arc::new(NestedField::required(id, name, Type::Primitive(ty)))
}

pub fn prices_schema() -> Result<Schema> {
    Ok(Schema::builder()
        .with_fields(vec![
            req(1, "tcgplayer_product_id", PrimitiveType::Long),
            req(2, "sub_type_name", PrimitiveType::String),
            req(3, "price_type", PrimitiveType::String),
            req(4, "price", PrimitiveType::Double),
            req(5, "observed_date", PrimitiveType::Date),
        ])
        .build()?)
}

pub fn sealed_schema() -> Result<Schema> {
    Ok(Schema::builder()
        .with_fields(vec![
            req(1, "tcgplayer_product_id", PrimitiveType::Long),
            req(2, "price_type", PrimitiveType::String),
            req(3, "price", PrimitiveType::Double),
            req(4, "observed_date", PrimitiveType::Date),
        ])
        .build()?)
}

/// Identity partition on the `observed_date` field (`source_id`), field id 1000.
fn partition_spec(source_id: i32) -> UnboundPartitionSpec {
    // Nessie rejects a create whose spec omits the ids, so both are sent.
    UnboundPartitionSpec::builder()
        .with_spec_id(0)
        .add_partition_fields([UnboundPartitionField::builder()
            .source_id(source_id)
            .field_id_opt(Some(1000))
            .name("observed_date".to_string())
            .transform(Transform::Identity)
            .build()])
        .expect("one partition field")
        .build()
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    #[test]
    fn partition_spec_carries_spec_and_field_ids() {
        let spec = partition_spec(5);
        assert_eq!(spec.spec_id(), Some(0));
        assert_eq!(spec.fields()[0].field_id, Some(1000));
    }
}

/// The `catalog.prices` identity and its creation parameters.
pub fn prices_ident() -> TableIdent {
    TableIdent::new(NamespaceIdent::new(NAMESPACE.into()), PRICES_TABLE.into())
}

pub fn sealed_ident() -> TableIdent {
    TableIdent::new(NamespaceIdent::new(NAMESPACE.into()), SEALED_TABLE.into())
}

async fn load_or_create(
    cat: &dyn Catalog,
    ident: &TableIdent,
    schema: Schema,
    observed_id: i32,
) -> Result<iceberg::table::Table> {
    if cat.table_exists(ident).await? {
        return Ok(cat.load_table(ident).await?);
    }
    let ns = ident.namespace();
    if !cat.namespace_exists(ns).await? {
        cat.create_namespace(ns, Default::default()).await?;
    }
    let creation = TableCreation::builder()
        .name(ident.name().to_string())
        .schema(schema)
        .partition_spec(partition_spec(observed_id))
        .build();
    Ok(cat.create_table(ns, creation).await?)
}

/// Both tables, created with the schemas above when absent.
pub async fn ensure_tables(
    cat: &dyn Catalog,
) -> Result<(iceberg::table::Table, iceberg::table::Table)> {
    let prices = load_or_create(cat, &prices_ident(), prices_schema()?, 5).await?;
    let sealed = load_or_create(cat, &sealed_ident(), sealed_schema()?, 4).await?;
    Ok((prices, sealed))
}

fn days_since_epoch(date: &str) -> Result<i32> {
    let datum = Datum::date_from_str(date)
        .map_err(|e| Error::Invalid(format!("observed_date {date:?} is not YYYY-MM-DD: {e}")))?;
    match datum.literal() {
        iceberg::spec::PrimitiveLiteral::Int(d) => Ok(*d),
        other => Err(Error::Invalid(format!("date literal was {other:?}"))),
    }
}

/// Rows already in `observed_date`.
pub async fn count_day(table: &iceberg::table::Table, observed_date: &str) -> Result<usize> {
    let scan = table
        .scan()
        .select(["tcgplayer_product_id"])
        .with_filter(Reference::new("observed_date").equal_to(Datum::date_from_str(observed_date)?))
        .build()?;
    let batches: Vec<RecordBatch> = scan.to_arrow().await?.try_collect().await?;
    Ok(batches.iter().map(RecordBatch::num_rows).sum())
}

fn prices_batch(
    table: &iceberg::table::Table,
    rows: &[PriceRow],
    days: i32,
) -> Result<RecordBatch> {
    let schema = Arc::new(schema_to_arrow_schema(table.metadata().current_schema())?);
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.tcgplayer_product_id),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.sub_type_name.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.price_type.as_str()),
        )),
        Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.price))),
        Arc::new(Date32Array::from(vec![days; rows.len()])),
    ];
    Ok(RecordBatch::try_new(schema, cols)?)
}

fn sealed_batch(
    table: &iceberg::table::Table,
    rows: &[SealedPriceRow],
    days: i32,
) -> Result<RecordBatch> {
    let schema = Arc::new(schema_to_arrow_schema(table.metadata().current_schema())?);
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.tcgplayer_product_id),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.price_type.as_str()),
        )),
        Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.price))),
        Arc::new(Date32Array::from(vec![days; rows.len()])),
    ];
    Ok(RecordBatch::try_new(schema, cols)?)
}

/// Append one day's batch to `table` in a single commit, carrying `provenance`
/// as snapshot properties. Refuses a day that already has rows: this client
/// cannot replace one (see the crate docs), and appending would double it.
async fn append_batch(
    cat: &dyn Catalog,
    table: iceberg::table::Table,
    batch: RecordBatch,
    observed_date: &str,
    days: i32,
    provenance: std::collections::HashMap<String, String>,
) -> Result<iceberg::table::Table> {
    let existing = count_day(&table, observed_date).await?;
    if existing > 0 {
        return Err(Error::Invalid(format!(
            "{} already holds {existing} row(s) for observed_date={observed_date}; \
             this client can only append, so refusing to double the day \
             (replace it with the Python lake job)",
            table.identifier().name()
        )));
    }
    if batch.num_rows() == 0 {
        return Ok(table);
    }

    let meta = table.metadata();
    let schema = meta.current_schema().clone();
    let spec: PartitionSpec = meta.default_partition_spec().as_ref().clone();
    let key = PartitionKey::new(
        spec,
        schema.clone(),
        Struct::from_iter([Some(Literal::date(days))]),
    );
    let location = DefaultLocationGenerator::new(meta)?;
    let names = DefaultFileNameGenerator::new(
        format!("dk-{}", uuid::Uuid::new_v4()),
        None,
        DataFileFormat::Parquet,
    );
    let parquet = ParquetWriterBuilder::new(WriterProperties::builder().build(), schema);
    let rolling = RollingFileWriterBuilder::new_with_default_file_size(
        parquet,
        table.file_io().clone(),
        location,
        names,
    );
    let mut writer = DataFileWriterBuilder::new(rolling).build(Some(key)).await?;
    writer.write(batch).await?;
    let files = writer.close().await?;

    let tx = Transaction::new(&table);
    let tx = tx
        .fast_append()
        .set_snapshot_properties(provenance)
        .add_data_files(files)
        .apply(tx)?;
    Ok(tx.commit(cat).await?)
}

/// Append one `observed_date` of `catalog.prices`.
pub async fn append_prices_day(
    cat: &dyn Catalog,
    table: iceberg::table::Table,
    observed_date: &str,
    rows: &[PriceRow],
    provenance: std::collections::HashMap<String, String>,
) -> Result<iceberg::table::Table> {
    let days = days_since_epoch(observed_date)?;
    let batch = prices_batch(&table, rows, days)?;
    append_batch(cat, table, batch, observed_date, days, provenance).await
}

/// Append one `observed_date` of `catalog.sealed_prices`.
pub async fn append_sealed_day(
    cat: &dyn Catalog,
    table: iceberg::table::Table,
    observed_date: &str,
    rows: &[SealedPriceRow],
    provenance: std::collections::HashMap<String, String>,
) -> Result<iceberg::table::Table> {
    let days = days_since_epoch(observed_date)?;
    let batch = sealed_batch(&table, rows, days)?;
    append_batch(cat, table, batch, observed_date, days, provenance).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_match_the_python_lake_field_for_field() {
        let p = prices_schema().unwrap();
        let got: Vec<_> = p
            .as_struct()
            .fields()
            .iter()
            .map(|f| (f.id, f.name.clone(), f.required))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, "tcgplayer_product_id".into(), true),
                (2, "sub_type_name".into(), true),
                (3, "price_type".into(), true),
                (4, "price".into(), true),
                (5, "observed_date".into(), true),
            ]
        );
        let s = sealed_schema().unwrap();
        assert_eq!(s.as_struct().fields().len(), 4);
        assert!(s.field_by_name("sub_type_name").is_none());
        assert_eq!(s.field_by_name("observed_date").unwrap().id, 4);
    }

    #[test]
    fn dates_are_days_since_epoch_and_bad_ones_are_refused() {
        assert_eq!(days_since_epoch("1970-01-02").unwrap(), 1);
        assert_eq!(days_since_epoch("2026-10-16").unwrap(), 20742);
        assert!(days_since_epoch("16/10/2026").is_err());
    }
}
