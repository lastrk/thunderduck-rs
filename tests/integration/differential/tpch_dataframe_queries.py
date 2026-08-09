"""TPC-H queries implemented in pure PySpark DataFrame API.

Extracted VERBATIM from the retired legacy TPC-H DataFrame test file
(each function was that test's inner `build_qNN(session)` closure) so the
DataFrame corpus's tpch cluster runs byte-identical query logic to the
legacy suite. Each function takes a SparkSession whose catalog has the
TPC-H temp views registered (lineitem, orders, customer, part, supplier,
partsupp, nation, region — see conftest._register_tpc_views).

EPSILONS preserves each legacy test's float-comparison tolerance
(assert_dataframes_equal epsilon; None = harness default).
"""
from pyspark.sql import functions as F
from pyspark.sql.window import Window


def build_q01(session):
    return session.table("lineitem") \
        .filter(F.col("l_shipdate") <= F.date_sub(F.lit("1998-12-01").cast("date"), 90)) \
        .groupBy("l_returnflag", "l_linestatus") \
        .agg(
            F.sum("l_quantity").alias("sum_qty"),
            F.sum("l_extendedprice").alias("sum_base_price"),
            F.sum(F.col("l_extendedprice") * (1 - F.col("l_discount"))).alias("sum_disc_price"),
            F.sum(F.col("l_extendedprice") * (1 - F.col("l_discount")) * (1 + F.col("l_tax"))).alias("sum_charge"),
            F.avg("l_quantity").alias("avg_qty"),
            F.avg("l_extendedprice").alias("avg_price"),
            F.avg("l_discount").alias("avg_disc"),
            F.count("*").alias("count_order")
        ) \
        .orderBy("l_returnflag", "l_linestatus")


def build_q02(session):
    # Captured from the legacy test's enclosing scope (the inner build_q02 was
    # a closure over this Window spec).
    window_spec = Window.partitionBy("p_partkey")
    return session.table("part") \
        .filter((F.col("p_size") == 15) & F.col("p_type").like("%BRASS")) \
        .join(session.table("partsupp"), F.col("p_partkey") == F.col("ps_partkey")) \
        .join(session.table("supplier"), F.col("s_suppkey") == F.col("ps_suppkey")) \
        .join(session.table("nation"), F.col("s_nationkey") == F.col("n_nationkey")) \
        .join(session.table("region"), F.col("n_regionkey") == F.col("r_regionkey")) \
        .filter(F.col("r_name") == "EUROPE") \
        .withColumn("min_supplycost", F.min("ps_supplycost").over(window_spec)) \
        .filter(F.col("ps_supplycost") == F.col("min_supplycost")) \
        .select("s_acctbal", "s_name", "n_name", "p_partkey", "p_mfgr", "s_address", "s_phone", "s_comment") \
        .orderBy(F.col("s_acctbal").desc(), "n_name", "s_name", "p_partkey") \
        .limit(100)


def build_q03(session):
    return session.table("customer") \
        .filter(F.col("c_mktsegment") == "BUILDING") \
        .join(session.table("orders"), F.col("c_custkey") == F.col("o_custkey")) \
        .filter(F.col("o_orderdate") < F.lit("1995-03-15")) \
        .join(session.table("lineitem"), F.col("o_orderkey") == F.col("l_orderkey")) \
        .filter(F.col("l_shipdate") > F.lit("1995-03-15")) \
        .groupBy("l_orderkey", "o_orderdate", "o_shippriority") \
        .agg(
            F.sum(F.col("l_extendedprice") * (1 - F.col("l_discount"))).alias("revenue")
        ) \
        .orderBy(F.col("revenue").desc(), "o_orderdate") \
        .limit(10)


def build_q04(session):
    late_lineitems = session.table("lineitem") \
        .filter(F.col("l_commitdate") < F.col("l_receiptdate")) \
        .select("l_orderkey").distinct()

    return session.table("orders") \
        .filter(
            (F.col("o_orderdate") >= F.lit("1993-07-01")) &
            (F.col("o_orderdate") < F.add_months(F.lit("1993-07-01").cast("date"), 3))
        ) \
        .join(late_lineitems, F.col("o_orderkey") == F.col("l_orderkey"), "left_semi") \
        .groupBy("o_orderpriority") \
        .agg(F.count("*").alias("order_count")) \
        .orderBy("o_orderpriority")


def build_q05(session):
    return session.table("customer") \
        .join(session.table("orders"), F.col("c_custkey") == F.col("o_custkey")) \
        .filter(
            (F.col("o_orderdate") >= F.lit("1994-01-01")) &
            (F.col("o_orderdate") < F.add_months(F.lit("1994-01-01").cast("date"), 12))
        ) \
        .join(session.table("lineitem"), F.col("l_orderkey") == F.col("o_orderkey")) \
        .join(session.table("supplier"), F.col("l_suppkey") == F.col("s_suppkey")) \
        .filter(F.col("c_nationkey") == F.col("s_nationkey")) \
        .join(session.table("nation"), F.col("s_nationkey") == F.col("n_nationkey")) \
        .join(session.table("region"), F.col("n_regionkey") == F.col("r_regionkey")) \
        .filter(F.col("r_name") == "ASIA") \
        .groupBy("n_name") \
        .agg(F.sum(F.col("l_extendedprice") * (1 - F.col("l_discount"))).alias("revenue")) \
        .orderBy(F.col("revenue").desc())


def build_q06(session):
    return session.table("lineitem") \
        .filter(
            (F.col("l_shipdate") >= F.lit("1994-01-01")) &
            (F.col("l_shipdate") < F.add_months(F.lit("1994-01-01").cast("date"), 12)) &
            (F.col("l_discount").between(0.05, 0.07)) &
            (F.col("l_quantity") < 24)
        ) \
        .agg(
            F.sum(F.col("l_extendedprice") * F.col("l_discount")).alias("revenue")
        )


def build_q07(session):
    n1 = session.table("nation").alias("n1")
    n2 = session.table("nation").alias("n2")

    shipping = session.table("supplier") \
        .join(session.table("lineitem"), F.col("s_suppkey") == F.col("l_suppkey")) \
        .join(session.table("orders"), F.col("l_orderkey") == F.col("o_orderkey")) \
        .join(session.table("customer"), F.col("o_custkey") == F.col("c_custkey")) \
        .join(n1, F.col("s_nationkey") == F.col("n1.n_nationkey")) \
        .join(n2, F.col("c_nationkey") == F.col("n2.n_nationkey")) \
        .filter(
            (
                (F.col("n1.n_name") == "FRANCE") & (F.col("n2.n_name") == "GERMANY")
            ) | (
                (F.col("n1.n_name") == "GERMANY") & (F.col("n2.n_name") == "FRANCE")
            )
        ) \
        .filter(
            (F.col("l_shipdate") >= F.lit("1995-01-01")) &
            (F.col("l_shipdate") <= F.lit("1996-12-31"))
        ) \
        .select(
            F.col("n1.n_name").alias("supp_nation"),
            F.col("n2.n_name").alias("cust_nation"),
            F.year("l_shipdate").alias("l_year"),
            (F.col("l_extendedprice") * (1 - F.col("l_discount"))).alias("volume")
        )

    return shipping \
        .groupBy("supp_nation", "cust_nation", "l_year") \
        .agg(F.sum("volume").alias("revenue")) \
        .orderBy("supp_nation", "cust_nation", "l_year")


def build_q08(session):
    n1 = session.table("nation").alias("n1")
    n2 = session.table("nation").alias("n2")

    all_nations = session.table("part") \
        .filter(F.col("p_type") == "ECONOMY ANODIZED STEEL") \
        .join(session.table("lineitem"), F.col("p_partkey") == F.col("l_partkey")) \
        .join(session.table("supplier"), F.col("l_suppkey") == F.col("s_suppkey")) \
        .join(session.table("orders"), F.col("l_orderkey") == F.col("o_orderkey")) \
        .filter(
            (F.col("o_orderdate") >= F.lit("1995-01-01")) &
            (F.col("o_orderdate") <= F.lit("1996-12-31"))
        ) \
        .join(session.table("customer"), F.col("o_custkey") == F.col("c_custkey")) \
        .join(n1, F.col("c_nationkey") == F.col("n1.n_nationkey")) \
        .join(session.table("region"), F.col("n1.n_regionkey") == F.col("r_regionkey")) \
        .filter(F.col("r_name") == "AMERICA") \
        .join(n2, F.col("s_nationkey") == F.col("n2.n_nationkey")) \
        .select(
            F.year("o_orderdate").alias("o_year"),
            (F.col("l_extendedprice") * (1 - F.col("l_discount"))).alias("volume"),
            F.col("n2.n_name").alias("nation")
        )

    return all_nations \
        .groupBy("o_year") \
        .agg(
            (F.sum(F.when(F.col("nation") == "BRAZIL", F.col("volume")).otherwise(0)) /
             F.sum("volume")).alias("mkt_share")
        ) \
        .orderBy("o_year")


def build_q09(session):
    profit = session.table("part") \
        .filter(F.col("p_name").like("%green%")) \
        .join(session.table("lineitem"), F.col("p_partkey") == F.col("l_partkey")) \
        .join(session.table("supplier"), F.col("l_suppkey") == F.col("s_suppkey")) \
        .join(session.table("partsupp"),
              (F.col("l_suppkey") == F.col("ps_suppkey")) &
              (F.col("l_partkey") == F.col("ps_partkey"))) \
        .join(session.table("orders"), F.col("l_orderkey") == F.col("o_orderkey")) \
        .join(session.table("nation"), F.col("s_nationkey") == F.col("n_nationkey")) \
        .select(
            F.col("n_name").alias("nation"),
            F.year("o_orderdate").alias("o_year"),
            (F.col("l_extendedprice") * (1 - F.col("l_discount")) -
             F.col("ps_supplycost") * F.col("l_quantity")).alias("amount")
        )

    return profit \
        .groupBy("nation", "o_year") \
        .agg(F.sum("amount").alias("sum_profit")) \
        .orderBy("nation", F.col("o_year").desc())


def build_q10(session):
    return session.table("customer") \
        .join(session.table("orders"), F.col("c_custkey") == F.col("o_custkey")) \
        .filter(
            (F.col("o_orderdate") >= F.lit("1993-10-01")) &
            (F.col("o_orderdate") < F.add_months(F.lit("1993-10-01").cast("date"), 3))
        ) \
        .join(session.table("lineitem"), F.col("l_orderkey") == F.col("o_orderkey")) \
        .filter(F.col("l_returnflag") == "R") \
        .join(session.table("nation"), F.col("c_nationkey") == F.col("n_nationkey")) \
        .groupBy(
            "c_custkey", "c_name", "c_acctbal", "c_phone",
            "n_name", "c_address", "c_comment"
        ) \
        .agg(
            F.sum(F.col("l_extendedprice") * (1 - F.col("l_discount"))).alias("revenue")
        ) \
        .select(
            "c_custkey", "c_name", "revenue", "c_acctbal",
            "n_name", "c_address", "c_phone", "c_comment"
        ) \
        .orderBy(F.col("revenue").desc()) \
        .limit(20)


def build_q11(session):
    # Base: partsupp joined with supplier and nation (Germany)
    base = session.table("partsupp") \
        .join(session.table("supplier"), F.col("ps_suppkey") == F.col("s_suppkey")) \
        .join(session.table("nation"), F.col("s_nationkey") == F.col("n_nationkey")) \
        .filter(F.col("n_name") == "GERMANY")

    # Threshold: SUM(ps_supplycost * ps_availqty) * 0.0001
    threshold = base \
        .agg(
            (F.sum(F.col("ps_supplycost") * F.col("ps_availqty")) * 0.0001).alias("threshold_value")
        )

    # Group by part, filter by threshold using crossJoin
    return base \
        .groupBy("ps_partkey") \
        .agg(
            F.sum(F.col("ps_supplycost") * F.col("ps_availqty")).alias("value")
        ) \
        .crossJoin(threshold) \
        .filter(F.col("value") > F.col("threshold_value")) \
        .select("ps_partkey", "value") \
        .orderBy(F.col("value").desc())


def build_q12(session):
    return session.table("orders") \
        .join(session.table("lineitem"), F.col("o_orderkey") == F.col("l_orderkey")) \
        .filter(
            F.col("l_shipmode").isin("MAIL", "SHIP") &
            (F.col("l_commitdate") < F.col("l_receiptdate")) &
            (F.col("l_shipdate") < F.col("l_commitdate")) &
            (F.col("l_receiptdate") >= F.lit("1994-01-01")) &
            (F.col("l_receiptdate") < F.add_months(F.lit("1994-01-01").cast("date"), 12))
        ) \
        .groupBy("l_shipmode") \
        .agg(
            F.sum(F.when(
                F.col("o_orderpriority").isin("1-URGENT", "2-HIGH"), 1
            ).otherwise(0)).alias("high_line_count"),
            F.sum(F.when(
                ~F.col("o_orderpriority").isin("1-URGENT", "2-HIGH"), 1
            ).otherwise(0)).alias("low_line_count")
        ) \
        .orderBy("l_shipmode")


def build_q13(session):
    # Count orders per customer (excluding special complaints)
    c_orders = session.table("customer") \
        .join(
            session.table("orders").filter(~F.col("o_comment").like("%special%requests%")),
            F.col("c_custkey") == F.col("o_custkey"),
            "left_outer"
        ) \
        .groupBy("c_custkey") \
        .agg(F.count("o_orderkey").alias("c_count"))

    # Distribution: how many customers have each order count
    return c_orders \
        .groupBy("c_count") \
        .agg(F.count("*").alias("custdist")) \
        .orderBy(F.col("custdist").desc(), F.col("c_count").desc())


def build_q14(session):
    return session.table("lineitem") \
        .join(session.table("part"), F.col("l_partkey") == F.col("p_partkey")) \
        .filter(
            (F.col("l_shipdate") >= F.lit("1995-09-01")) &
            (F.col("l_shipdate") < F.add_months(F.lit("1995-09-01").cast("date"), 1))
        ) \
        .agg(
            (F.lit(100.00) *
             F.sum(F.when(
                 F.col("p_type").like("PROMO%"),
                 F.col("l_extendedprice") * (1 - F.col("l_discount"))
             ).otherwise(0)) /
             F.sum(F.col("l_extendedprice") * (1 - F.col("l_discount")))
            ).alias("promo_revenue")
        )


def build_q15(session):
    # Revenue "view" (CTE equivalent)
    revenue = session.table("lineitem") \
        .filter(
            (F.col("l_shipdate") >= F.lit("1996-01-01")) &
            (F.col("l_shipdate") < F.add_months(F.lit("1996-01-01").cast("date"), 3))
        ) \
        .groupBy(F.col("l_suppkey").alias("supplier_no")) \
        .agg(
            F.sum(F.col("l_extendedprice") * (1 - F.col("l_discount"))).alias("total_revenue")
        )

    # Max revenue
    max_revenue = revenue.agg(F.max("total_revenue").alias("max_revenue"))

    return session.table("supplier") \
        .join(revenue, F.col("s_suppkey") == F.col("supplier_no")) \
        .crossJoin(max_revenue) \
        .filter(F.col("total_revenue") == F.col("max_revenue")) \
        .select("s_suppkey", "s_name", "s_address", "s_phone", "total_revenue") \
        .orderBy("s_suppkey")


def build_q16(session):
    # Suppliers with complaints (to exclude)
    complaint_suppliers = session.table("supplier") \
        .filter(F.col("s_comment").like("%Customer%Complaints%")) \
        .select("s_suppkey")

    return session.table("partsupp") \
        .join(
            session.table("part"),
            F.col("ps_partkey") == F.col("p_partkey")
        ) \
        .filter(
            (F.col("p_brand") != "Brand#45") &
            (~F.col("p_type").like("MEDIUM POLISHED%")) &
            F.col("p_size").isin(49, 14, 23, 45, 19, 3, 36, 9)
        ) \
        .join(complaint_suppliers, F.col("ps_suppkey") == F.col("s_suppkey"), "left_anti") \
        .groupBy("p_brand", "p_type", "p_size") \
        .agg(F.countDistinct("ps_suppkey").alias("supplier_cnt")) \
        .orderBy(F.col("supplier_cnt").desc(), "p_brand", "p_type", "p_size")


def build_q17(session):
    # Pre-compute average quantity per part
    # Rename l_partkey to avoid ambiguity when joining back to lineitem
    avg_qty = session.table("lineitem") \
        .groupBy("l_partkey") \
        .agg((F.avg("l_quantity") * 0.2).alias("avg_qty_threshold")) \
        .withColumnRenamed("l_partkey", "avg_l_partkey")

    return session.table("part") \
        .filter(
            (F.col("p_brand") == "Brand#23") &
            (F.col("p_container") == "MED BOX")
        ) \
        .join(session.table("lineitem"), F.col("p_partkey") == F.col("l_partkey")) \
        .join(avg_qty, F.col("l_partkey") == F.col("avg_l_partkey")) \
        .filter(F.col("l_quantity") < F.col("avg_qty_threshold")) \
        .agg(
            (F.sum("l_extendedprice") / 7.0).alias("avg_yearly")
        )


def build_q18(session):
    # Subquery: orderkeys where total quantity > 300
    large_orders = session.table("lineitem") \
        .groupBy("l_orderkey") \
        .agg(F.sum("l_quantity").alias("total_qty")) \
        .filter(F.col("total_qty") > 300) \
        .select("l_orderkey")

    return session.table("customer") \
        .join(session.table("orders"), F.col("c_custkey") == F.col("o_custkey")) \
        .join(large_orders, F.col("o_orderkey") == large_orders["l_orderkey"], "left_semi") \
        .join(session.table("lineitem"), F.col("o_orderkey") == F.col("l_orderkey")) \
        .groupBy("c_name", "c_custkey", "o_orderkey", "o_orderdate", "o_totalprice") \
        .agg(F.sum("l_quantity").alias("total_qty")) \
        .orderBy(F.col("o_totalprice").desc(), "o_orderdate") \
        .limit(100)


def build_q19(session):
    joined = session.table("lineitem") \
        .join(session.table("part"), F.col("l_partkey") == F.col("p_partkey"))

    # Three segments with brand/container/size/quantity filters
    seg1 = (
        (F.col("p_brand") == "Brand#12") &
        F.col("p_container").isin("SM CASE", "SM BOX", "SM PACK", "SM PKG") &
        (F.col("l_quantity") >= 1) & (F.col("l_quantity") <= 11) &
        (F.col("p_size").between(1, 5))
    )
    seg2 = (
        (F.col("p_brand") == "Brand#23") &
        F.col("p_container").isin("MED BAG", "MED BOX", "MED PKG", "MED PACK") &
        (F.col("l_quantity") >= 10) & (F.col("l_quantity") <= 20) &
        (F.col("p_size").between(1, 10))
    )
    seg3 = (
        (F.col("p_brand") == "Brand#34") &
        F.col("p_container").isin("LG CASE", "LG BOX", "LG PACK", "LG PKG") &
        (F.col("l_quantity") >= 20) & (F.col("l_quantity") <= 30) &
        (F.col("p_size").between(1, 15))
    )

    return joined \
        .filter(
            (seg1 | seg2 | seg3) &
            (F.col("l_shipmode").isin("AIR", "AIR REG")) &
            (F.col("l_shipinstruct") == "DELIVER IN PERSON")
        ) \
        .agg(
            F.sum(F.col("l_extendedprice") * (1 - F.col("l_discount"))).alias("revenue")
        )


def build_q20(session):
    # Inner subquery: parts starting with 'forest'
    forest_parts = session.table("part") \
        .filter(F.col("p_name").like("forest%")) \
        .select("p_partkey")

    # Middle subquery: lineitem aggregation for threshold
    li_agg = session.table("lineitem") \
        .filter(
            (F.col("l_shipdate") >= F.lit("1994-01-01")) &
            (F.col("l_shipdate") < F.add_months(F.lit("1994-01-01").cast("date"), 12))
        ) \
        .groupBy("l_partkey", "l_suppkey") \
        .agg((F.sum("l_quantity") * 0.5).alias("qty_threshold"))

    # Partsupp filtered by forest parts and quantity threshold
    qualifying_ps = session.table("partsupp") \
        .join(forest_parts, F.col("ps_partkey") == F.col("p_partkey"), "left_semi") \
        .join(li_agg,
              (F.col("ps_suppkey") == F.col("l_suppkey")) &
              (F.col("ps_partkey") == F.col("l_partkey"))) \
        .filter(F.col("ps_availqty") > F.col("qty_threshold")) \
        .select("ps_suppkey")

    return session.table("supplier") \
        .join(session.table("nation"), F.col("s_nationkey") == F.col("n_nationkey")) \
        .filter(F.col("n_name") == "CANADA") \
        .join(qualifying_ps, F.col("s_suppkey") == F.col("ps_suppkey"), "left_semi") \
        .select("s_name", "s_address") \
        .orderBy("s_name")


def build_q21(session):
    lineitem = session.table("lineitem")

    # L1: the main lineitem (late delivery)
    l1 = lineitem.alias("l1")

    # EXISTS: another supplier for same order (l2.l_suppkey != l1.l_suppkey)
    l2_exists = lineitem.select(
        F.col("l_orderkey").alias("l2_orderkey"),
        F.col("l_suppkey").alias("l2_suppkey")
    ).distinct()

    # NOT EXISTS: no other supplier that also delivered late
    l3_late = lineitem \
        .filter(F.col("l_receiptdate") > F.col("l_commitdate")) \
        .select(
            F.col("l_orderkey").alias("l3_orderkey"),
            F.col("l_suppkey").alias("l3_suppkey")
        ).distinct()

    base = session.table("supplier") \
        .join(l1, F.col("s_suppkey") == F.col("l1.l_suppkey")) \
        .join(session.table("orders"), F.col("l1.l_orderkey") == F.col("o_orderkey")) \
        .filter(F.col("o_orderstatus") == "F") \
        .filter(F.col("l1.l_receiptdate") > F.col("l1.l_commitdate")) \
        .join(session.table("nation"), F.col("s_nationkey") == F.col("n_nationkey")) \
        .filter(F.col("n_name") == "SAUDI ARABIA")

    # EXISTS: at least one other supplier on same order
    base_with_exists = base \
        .join(l2_exists,
              (F.col("l1.l_orderkey") == F.col("l2_orderkey")) &
              (F.col("l1.l_suppkey") != F.col("l2_suppkey")),
              "left_semi")

    # NOT EXISTS: no other supplier with late delivery on same order
    base_final = base_with_exists \
        .join(l3_late,
              (F.col("l1.l_orderkey") == F.col("l3_orderkey")) &
              (F.col("l1.l_suppkey") != F.col("l3_suppkey")),
              "left_anti")

    return base_final \
        .groupBy("s_name") \
        .agg(F.count("*").alias("numwait")) \
        .orderBy(F.col("numwait").desc(), "s_name") \
        .limit(100)


def build_q22(session):
    country_codes = ["13", "31", "23", "29", "30", "18", "17"]

    # Average account balance for customers with positive balance in target countries
    avg_bal = session.table("customer") \
        .filter(
            (F.col("c_acctbal") > 0.00) &
            F.substring("c_phone", 1, 2).isin(*country_codes)
        ) \
        .agg(F.avg("c_acctbal").alias("avg_acctbal"))

    # Customers with no orders (anti-join)
    custsale = session.table("customer") \
        .filter(F.substring("c_phone", 1, 2).isin(*country_codes)) \
        .join(session.table("orders"), F.col("c_custkey") == F.col("o_custkey"), "left_anti") \
        .crossJoin(avg_bal) \
        .filter(F.col("c_acctbal") > F.col("avg_acctbal")) \
        .select(
            F.substring("c_phone", 1, 2).alias("cntrycode"),
            "c_acctbal"
        )

    return custsale \
        .groupBy("cntrycode") \
        .agg(
            F.count("*").alias("numcust"),
            F.sum("c_acctbal").alias("totacctbal")
        ) \
        .orderBy("cntrycode")


QUERY_IMPLEMENTATIONS = {
    1: build_q01,
    2: build_q02,
    3: build_q03,
    4: build_q04,
    5: build_q05,
    6: build_q06,
    7: build_q07,
    8: build_q08,
    9: build_q09,
    10: build_q10,
    11: build_q11,
    12: build_q12,
    13: build_q13,
    14: build_q14,
    15: build_q15,
    16: build_q16,
    17: build_q17,
    18: build_q18,
    19: build_q19,
    20: build_q20,
    21: build_q21,
    22: build_q22,
}

# Per-test float tolerance (None = assert_dataframes_equal default).
EPSILONS = {
    1: 0.01,
    2: 0.01,
    3: 0.01,
    4: None,
    5: 0.01,
    6: 0.01,
    7: 0.01,
    8: 0.01,
    9: 0.01,
    10: 0.01,
    11: 0.01,
    12: None,
    13: None,
    14: 0.01,
    15: 0.01,
    16: None,
    17: 0.01,
    18: 0.01,
    19: 0.01,
    20: None,
    21: None,
    22: 0.01,
}
