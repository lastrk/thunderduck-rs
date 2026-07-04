pub mod relation_converter;
pub mod type_converter;
pub mod v2_relation_converter;

// The legacy `plan_converter` / `expression_converter` modules and the
// `RelationConverter` struct were deleted once the τ path
// (`v2_relation_converter`) became the only production converter (ADR-022).
// `relation_converter` now retains only `parse_json_schema`, still used by the
// v2 path to decode `createDataFrame` schema payloads (Slice K tracks removing
// that last dependency).
