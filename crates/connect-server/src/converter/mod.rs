pub mod expression_converter;
pub mod plan_converter;
pub mod relation_converter;
pub mod type_converter;
pub mod v2_relation_converter;

// `PlanConverter` re-export removed in Slice A.3 — dispatch relocated to the
// τ boundary and no longer consumes the legacy converter. The module remains
// (Slice K owns full legacy deletion) but is no longer part of the public
// crate surface.
