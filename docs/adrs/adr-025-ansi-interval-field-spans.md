# ADR-025 — ANSI interval field spans live on `DataType`

**Status:** Proposed
**Depends on:** ADR-005 (type inference), ADR-015 (Spark differential oracle), ADR-016 (Spark 4.1.1 pin), ADR-021 (τ owns its substrate), ADR-022 (honest errors)
**Depended on by:** interval SQL lowering, `Date ± interval` inference, AnalyzePlan schemas, Connect type conversion, and Arrow LocalRelation values

**Context.** Spark's `DayTimeIntervalType` and `YearMonthIntervalType` are parameterized by inclusive start and end fields. Spark Connect carries those fields, but τ currently discards them because its interval `DataType` variants are field-less. That loss forces single-field `DAY` literals to masquerade as calendar intervals, makes every day-time interval column appear to contain a sub-day field, and reports full-span types for narrower literals. These are silent type changes, not honest unsupported boundaries.

**Decision.** The declared span is durable value-type structure:

```rust
enum DayTimeField { Day, Hour, Minute, Second }
enum YearMonthField { Year, Month }

DayTimeInterval { start: DayTimeField, end: DayTimeField }
YearMonthInterval { start: YearMonthField, end: YearMonthField }
```

The separate field enums prevent cross-family states. Both derive Spark's most-significant-to-least-significant ordering. Missing Connect fields use Spark's full-span defaults (`DAY TO SECOND` and `YEAR TO MONTH`); present fields round-trip exactly. Literal `IntervalKind` carries the same family-specific span, so `Expression::data_type` is a pure mapping. Generic `CalendarInterval` remains unparameterized.

Every span-dependent decision consumes this stored fact. `Date ± DayTimeInterval` promotes to `Timestamp` exactly when the end field is below `DAY`; a day-only span stays `Date`. Type unification widens compatible interval spans to their union. AnalyzePlan and Connect responses emit the exact span. Arrow encodings that omit the logical span use the appropriate full-span default; the physical wire transcoder remains in connect-server per INV10.

Arrow interval values lower to the typed `IntervalExpression { months, days, microseconds, kind }`. They never enter τ as SQL text. Once those are the last producers, `RawSqlExpression` is deleted.

**Alternatives rejected.** Literal-only metadata cannot represent interval columns. Value inspection cannot distinguish a day-only type from a `DAY TO SECOND` value whose sub-day component is zero. A shared field enum permits illegal cross-family states. An out-of-band side table duplicates type structure and can drift at wire boundaries.

**Consequences.** Interval `Eq`/`Hash` now distinguishes spans; every equality and keying site must be audited. Existing construction sites without more precise information use full-span constructors. The compensating `DAY → Calendar` lowering and the long "sub-day by construction" rationale disappear. Differential witnesses pin day-only column arithmetic, exact literal schemas, mixed-span widening, and unchanged Arrow behavior.

---

