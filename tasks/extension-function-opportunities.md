# τ → `thdck_spark_funcs`: extension-function opportunities

Audit (2026-07-14) of the transpiler's Spark-emulation surface for places where a
**native DuckDB extension function** in `thdck_spark_funcs` would emulate Spark
behaviour *precisely* and delete substantial SQL-composition complexity from
emission. This is the "push it into the extension" counterpart to
[`v2-corpus-followups.md`](v2-corpus-followups.md): those are per-case parity
gaps; THESE are structural gaps where the emitted SQL composition is inherently
lossy and an extension function is the correct altitude for the fix.

Precedent: the extension already hosts `spark_avg`, `spark_hash`,
`spark_xxhash64`, `spark_decimal_div`, `spark_try_{divide,sum,avg}`,
`spark_skewness`, `spark_schema_of_json`. Adding functions is an established
pattern, not new mechanism.

**Verification note:** no DuckDB CLI was available in the audit environment.
Rows marked `src` are confirmed from emission source; rows marked `doc` also
rest on documented DuckDB semantics (e.g. `split`/`string_split` take a
*literal* separator; regex split is `regexp_split_to_array`); rows marked
`plaus` need a live probe to pin the exact divergence.

Legend — **class**: `wrong` (silent wrong answer on input Spark accepts),
`boundary` (τ rejects input Spark accepts), `perf`, `altitude`.

## Tier 1 — silent wrong answers on common inputs

| id | fn | site | class | ev | defect → proposed extension fn |
|----|----|------|-------|----|-------------------------------|
| X-sha2-bits | `sha2` | emission.rs:5223 | wrong | src | `min_args<1>` drops the bit-width arg; always emits `sha256`. `sha2(x,512)` returns a 64-hex SHA-256 digest, not the 128-hex SHA-512. Same for 384/224. → **`spark_sha2(str, bits)`** dispatching on width. |
| X-datefmt | `spark_fmt_to_duckdb` | emission.rs:3147 | wrong | src | Naive 8-token `replace()` chain (`yyyy MM dd HH mm ss yy a`). `'MMMM'`→`'%m%m'` (July → `0707`); `EEE`/`D`/`SSS`/`z`/`X`, single-char `M`/`d`/`H`, and quoted literals (`'T'`) all mistranslated. Feeds **6 arms** (`to_date`, `to_timestamp`, `unix_timestamp`, `from_unixtime`, `date_format`, `to_char`). → **`spark_date_format(ts, javaPattern)`** + **`spark_to_timestamp(str, javaPattern)`** implementing Java `DateTimeFormatter`; deletes the translator and the strptime/strftime composition. |
| X-split-regex | `split` | emission.rs:5191 | wrong | doc | Emits DuckDB `split`, a *literal* separator; Spark's pattern is a Java regex. `split('a1b2c','[0-9]')` → `['a1b2c']` (want `['a','b','c']`); `split('a.b','\\.')` → `['a.b']`. → **`spark_split(str, javaRegex, limit)`**; also deletes the `limit`-slicing CASE. |
| X-monthsbetween | `months_between` | emission.rs:5284 | wrong | src | `datediff('month',b,a) + (day(a)-day(b))/31.0` ignores (1) Spark's last-day-of-month rule — `months_between('2020-02-29','2020-01-31')` → τ `0.9355`, Spark `1.0` — and (2) the seconds-within-day term folded into the /31 fraction. → **`spark_months_between(a, b)`** matching the Catalyst algorithm. |

## Tier 2 — divergence on realistic (less-common) inputs

| id | fn | site | class | ev | defect → proposed extension fn |
|----|----|------|-------|----|-------------------------------|
| X-fromcsv | `from_csv` | emission.rs:5427 | wrong | src | `split_part(csv, ',', i)` per field ignores CSV quoting/escaping. `from_csv('"a,b",c', 'x STRING,y STRING')` → `{x:'"a', y:'b"'}`. Arm's own "KNOWN DEVIATION" note flags it. → **`spark_from_csv(str, schema)`** (univocity-equivalent); deletes `from_csv_ddl_to_struct` + split composition. |
| X-tocsv | `to_csv` | emission.rs:4147 | wrong | src | `concat_ws(',', CAST(f AS VARCHAR), …)` with no RFC-4180 quoting; only accepts a literal `struct(...)`/`named_struct(...)`. `to_csv(struct('a,b' AS x,'c' AS y))` → `'a,b,c'`. Arm comment names option "(b) new `spark_to_csv` extension" as the fix. → **`spark_to_csv(struct)`** accepting any struct-typed column. |
| X-tochar-dispatch | `to_char` | emission.rs:3870 | wrong | src | Arm fires on `args.len() == 2` regardless of arg type → numeric-format `to_char(1234.5,'9,999.9')` routed through `strftime`. Comment concedes number-format is "out of scope." → **`spark_to_char(x, fmt)`** covering date + number forms. |
| X-parseurl | `parse_url` | emission.rs:5542 | wrong | plaus | Eight hand-written `regexp_extract` patterns + `regex_escape` + `NULLIF`; diverges from `java.net` URL parsing on IPv6 hosts (`http://[::1]:8080/p`), `@` in path, encoded/substring query keys. → **`spark_parse_url(url, part, key?)`** delegating to the JDK parser. |
| X-tonumber | `to_number` / `try_to_number` | emission.rs:6901 (`parse_number_format`) | boundary | src | Format model is only `9/0/./,`; sign (`MI`,`PR`,`S`), currency (`$`), exponent (`EEEE`), `L`/`G` all → boundary error on valid Spark formats. Supported path also doesn't enforce fixed-width/grouping-position mismatch. → **`spark_to_number(str, fmt)`** (full `ToNumberParser`); deletes `parse_number_format` + `render_to_number_cast` + ANSI-throw CASE. |

## Tier 3 — efficiency / precision / coverage / structure

| id | fn | site | class | ev | defect → proposed extension fn |
|----|----|------|-------|----|-------------------------------|
| X-arrayset | `array_distinct`/`union`/`except`/`intersect` | emission.rs:3461 (`order_preserving_distinct`, `null_safe_member`) | perf | src | Helper interpolates the list SQL **twice** and scans O(n²) via `list_position`; `array_union` passes `list_concat(a,b)` so the concat runs twice. "Callers must pass an expression safe to repeat" is a latent trap. → **`spark_array_distinct/union/except/intersect`** — single-pass linked-hash-set, side-effect-safe. Backs 4 operators + 2 shared helpers. |
| X-bround | `bround` | emission.rs:5001 | wrong | src | HALF_EVEN emulated via DOUBLE `pow(10,n)`: precision loss for DECIMAL inputs and the `frac == 0.5` branch only fires on a bit-exact double `.5`. → **`spark_bround(x, n)`** rounding on the exact decimal. |
| X-conv | `conv` | emission.rs:5048 | boundary | src | Implemented only for `to_base ∈ {2,16}`; `from_base` is ignored (rendered only for its error path), so `conv('10',2,16)` reads `'10'` as decimal. → **`spark_conv(str, from, to)`** over the full 2..36 unsigned range. |
| X-dual-layer | array set-ops / `substring_index` / 2-arg `to_char` | session.rs:405–447 | altitude | src | These have BOTH a `CREATE MACRO` (session.rs) and an early-returning emission arm; the arm shadows the macro (see `v2-corpus-followups.md` → `F-dead-macros`, "safe to delete"). Two implementations per function that can silently diverge; a parity fix to the macro is dead. → Consolidate each into a single `spark_*` extension function; delete the shadowed macros. |

## Priority

- **X-datefmt** — largest complexity removed + widest blast radius (6 call sites); currently wrong for month/day-name formats, which are common.
- **X-arrayset** — one extension change retires the two shared helpers that back four operators.
- **X-sha2-bits**, **X-split-regex** — small surface, but silent wrong answers on ordinary inputs.

Both X-datefmt and X-arrayset are already self-documented in emission as
"best-effort / will diverge".
