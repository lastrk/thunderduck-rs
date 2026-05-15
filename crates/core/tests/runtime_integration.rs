use std::sync::Arc;

use thunderduck_core::runtime::compat_mode::RuntimeCompatMode;
use thunderduck_core::runtime::{DuckDbSession, SessionManager, StreamingConfig};

// ── session_round_trip ─────────────────────────────────────────────────────────

/// Basic end-to-end: create a view, query it, verify schema + values.
#[tokio::test]
#[ignore]
async fn session_round_trip() {
    let session = DuckDbSession::spawn(
        "test-1",
        RuntimeCompatMode::Relaxed,
        &StreamingConfig::default(),
    )
    .expect("spawn failed");

    // 1. Create a simple view via range().
    session
        .create_temp_view("nums", "SELECT \"range\" AS n FROM range(1, 6, 1)")
        .await
        .expect("create_temp_view failed");

    // 2. Execute a query against the view.
    let batches = session
        .execute("SELECT n, n*n AS squared FROM nums ORDER BY n")
        .await
        .expect("execute failed");

    // 3. Verify schema.
    assert!(!batches.is_empty(), "expected at least one batch");
    let schema = batches[0].schema();
    assert_eq!(schema.field(0).name(), "n");
    assert_eq!(schema.field(1).name(), "squared");

    // 4. Verify data — collect all rows across batches.
    let all_n: Vec<i64> = batches
        .iter()
        .flat_map(|b| {
            let col = b
                .column(0)
                .as_any()
                .downcast_ref::<duckdb::arrow::array::Int64Array>()
                .expect("n column is not Int64Array");
            col.values().to_vec()
        })
        .collect();

    assert_eq!(all_n, vec![1, 2, 3, 4, 5]);
}

// ── session_manager_isolation ──────────────────────────────────────────────────

/// Two sessions must have fully isolated DuckDB databases.
#[tokio::test]
#[ignore]
async fn session_manager_isolation() {
    let mgr = SessionManager::new(RuntimeCompatMode::Relaxed, StreamingConfig::default());

    let s1 = mgr
        .get_or_create("session-a")
        .await
        .expect("get_or_create session-a failed");
    let s2 = mgr
        .get_or_create("session-b")
        .await
        .expect("get_or_create session-b failed");

    // Create a table in session-a.
    s1.execute("CREATE TABLE t (x INT)")
        .await
        .expect("CREATE TABLE failed");

    // session-b must NOT see table t.
    let result = s2.execute("SELECT * FROM t").await;
    assert!(
        result.is_err(),
        "sessions must be isolated: session-b should not see session-a's table t"
    );
}

// ── generator_to_duckdb ────────────────────────────────────────────────────────

/// Full pipeline: LogicalPlan → SQL string (Phase 1) → DuckDB execution → Arrow.
#[tokio::test]
#[ignore]
async fn generator_to_duckdb() {
    use thunderduck_core::{
        expression::{Expression, UnresolvedColumn},
        functions::CompatMode,
        generator::SqlGenerator,
        logical::{LogicalPlan, Project, RangeRelation},
    };

    let plan = LogicalPlan::Project(Project {
        input: Box::new(LogicalPlan::RangeRelation(RangeRelation {
            start: 1,
            end: 4,
            step: 1,
            num_partitions: None,
        })),
        projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
            name: "id".into(),
            qualifier: None,
        })],
    });

    let sql = SqlGenerator::new(CompatMode::Relaxed)
        .generate(&plan)
        .expect("SQL generation failed");

    let session = DuckDbSession::spawn(
        "gen-test",
        RuntimeCompatMode::Relaxed,
        &StreamingConfig::default(),
    )
    .expect("spawn failed");

    let batches = session.execute(&sql).await.expect("execute failed");

    assert!(!batches.is_empty(), "expected result batches");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 3, "range(1, 4, 1) should yield 3 rows");
}

// ── get_or_create_is_race_free (Bug 6: TOCTOU) ────────────────────────────────

/// Demonstrates the TOCTOU race in `SessionManager::get_or_create`.
///
/// With the buggy implementation (plain get → spawn → insert), concurrent callers
/// all miss the fast-path check and each spawn their own DuckDB thread.  The last
/// insert wins the map slot, but earlier callers return different Arc instances —
/// which means later `get_or_create("same-id")` callers are talking to a *different*
/// session than earlier ones.
///
/// The fix is to use DashMap's `entry` API so check-and-insert is atomic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn get_or_create_is_race_free() {
    const CONCURRENCY: usize = 8;
    let mgr = Arc::new(SessionManager::new(
        RuntimeCompatMode::Relaxed,
        StreamingConfig::default(),
    ));
    // Barrier ensures all tasks execute get_or_create at the same instant.
    let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY));

    let mut handles = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let mgr = Arc::clone(&mgr);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            mgr.get_or_create("shared-session").await.unwrap()
        }));
    }

    let mut sessions = Vec::with_capacity(CONCURRENCY);
    for h in handles {
        sessions.push(h.await.unwrap());
    }

    // Every caller must receive an Arc pointing to the SAME session object.
    let first = &sessions[0];
    for (i, other) in sessions[1..].iter().enumerate() {
        assert!(
            Arc::ptr_eq(first, other),
            "TOCTOU: caller 0 and caller {} got different session instances",
            i + 1,
        );
    }
}

// ── check_parquet_types ────────────────────────────────────────────────────────

/// Quick check: what types does DuckDB return for TPC-H parquet columns?
#[tokio::test]
#[ignore]
async fn check_parquet_types() {
    let session = DuckDbSession::spawn(
        "parquet-type-check",
        RuntimeCompatMode::Relaxed,
        &StreamingConfig::default(),
    )
    .expect("spawn failed");

    // Check supplier schema
    let batches = session.execute("DESCRIBE SELECT * FROM read_parquet('/workspace/tests/integration/tpch_sf001/supplier.parquet')").await.expect("failed");
    println!("Supplier schema:");
    for batch in &batches {
        for row in 0..batch.num_rows() {
            let name = batch
                .column(0)
                .as_any()
                .downcast_ref::<duckdb::arrow::array::StringArray>()
                .unwrap()
                .value(row);
            let dtype = batch
                .column(1)
                .as_any()
                .downcast_ref::<duckdb::arrow::array::StringArray>()
                .unwrap()
                .value(row);
            println!("  {name}: {dtype}");
        }
    }

    // Check arithmetic type (use LIMIT on FROM to avoid GROUP BY requirement)
    let batches = session.execute("SELECT typeof(1 - l_discount) AS t1, typeof(l_extendedprice * (1 - l_discount)) AS t2 FROM read_parquet('/workspace/tests/integration/tpch_sf001/lineitem.parquet') LIMIT 1").await.expect("failed");
    let batches2 = session.execute("SELECT typeof(SUM(l_extendedprice * (1 - l_discount))) AS t3 FROM read_parquet('/workspace/tests/integration/tpch_sf001/lineitem.parquet')").await.expect("failed");
    let batches = batches;
    for batch in &batches {
        let t1 = batch
            .column(0)
            .as_any()
            .downcast_ref::<duckdb::arrow::array::StringArray>()
            .unwrap()
            .value(0);
        let t2 = batch
            .column(1)
            .as_any()
            .downcast_ref::<duckdb::arrow::array::StringArray>()
            .unwrap()
            .value(0);
        println!("1-DECIMAL type: {t1}");
        println!("DECIMAL*DECIMAL type: {t2}");
    }
    for batch in &batches2 {
        let t3 = batch
            .column(0)
            .as_any()
            .downcast_ref::<duckdb::arrow::array::StringArray>()
            .unwrap()
            .value(0);
        println!("SUM(DECIMAL*DECIMAL) type: {t3}");
    }

    // Register lineitem view (simulating createOrReplaceTempView)
    session.create_temp_view("lineitem", "SELECT * FROM read_parquet('/workspace/tests/integration/tpch_sf001/lineitem.parquet')").await.expect("create view failed");

    // Check ARROW schema types via view (like Q1 DataFrame generates)
    let batches3 = session.execute("SELECT \"l_returnflag\", \"l_linestatus\", SUM(\"l_extendedprice\" * (1 - \"l_discount\")) AS \"sum_disc_price\" FROM (SELECT * FROM \"lineitem\") GROUP BY \"l_returnflag\", \"l_linestatus\" LIMIT 1").await.expect("failed");
    if let Some(batch) = batches3.first() {
        let schema = batch.schema();
        println!("Arrow schema of batch:");
        for field in schema.fields() {
            println!("  {}: {:?}", field.name(), field.data_type());
        }
    }

    // Check schema_of (the LIMIT 0 path used by analyze_plan)
    let q1_sql = "SELECT \"l_returnflag\", \"l_linestatus\", SUM(\"l_extendedprice\" * (1 - \"l_discount\")) AS \"sum_disc_price\" FROM (SELECT * FROM \"lineitem\") GROUP BY \"l_returnflag\", \"l_linestatus\"";
    let schema = session.schema_of(q1_sql).await.expect("schema_of failed");
    println!("schema_of (LIMIT 0) result:");
    for field in schema.fields() {
        println!("  {}: {:?}", field.name(), field.data_type());
    }
}
