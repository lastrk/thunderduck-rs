#pragma once

#include "duckdb/function/scalar_function.hpp"
#include "duckdb/common/vector_operations/generic_executor.hpp"
#include "yyjson.hpp"

namespace duckdb {

// ---------------------------------------------------------------------------
// spark_schema_of_json: Parses a JSON string and returns Spark DDL schema format
//
// Example:
//   spark_schema_of_json('{"a": 1, "b": "hello"}')
//   → 'STRUCT<a: BIGINT, b: STRING>'
//
// Type mapping (JSON → Spark DDL):
//   null    → VOID
//   boolean → BOOLEAN
//   integer → BIGINT
//   real    → DOUBLE
//   string  → STRING
//   array   → ARRAY<element_type>
//   object  → STRUCT<field1: type1, field2: type2, ...>
// ---------------------------------------------------------------------------

using namespace duckdb_yyjson; // NOLINT

static std::string InferSparkType(yyjson_val *val) {
	if (!val) {
		return "VOID";
	}

	yyjson_type type = yyjson_get_type(val);

	switch (type) {
	case YYJSON_TYPE_NULL:
		return "VOID";

	case YYJSON_TYPE_BOOL:
		return "BOOLEAN";

	case YYJSON_TYPE_NUM:
		if (yyjson_is_int(val) || yyjson_is_sint(val) || yyjson_is_uint(val)) {
			return "BIGINT";
		}
		return "DOUBLE";

	case YYJSON_TYPE_STR:
		return "STRING";

	case YYJSON_TYPE_ARR: {
		// Infer element type from first element (or VOID if empty)
		yyjson_arr_iter iter;
		yyjson_arr_iter_init(val, &iter);
		yyjson_val *elem = yyjson_arr_iter_next(&iter);
		if (!elem) {
			return "ARRAY<STRING>"; // Spark defaults empty arrays to ARRAY<STRING>
		}
		return "ARRAY<" + InferSparkType(elem) + ">";
	}

	case YYJSON_TYPE_OBJ: {
		std::string result = "STRUCT<";
		yyjson_obj_iter iter;
		yyjson_obj_iter_init(val, &iter);
		bool first = true;
		yyjson_val *key;
		while ((key = yyjson_obj_iter_next(&iter))) {
			yyjson_val *field_val = yyjson_obj_iter_get_val(key);
			if (!first) {
				result += ", ";
			}
			first = false;
			result += yyjson_get_str(key);
			result += ": ";
			result += InferSparkType(field_val);
		}
		result += ">";
		return result;
	}

	default:
		return "STRING";
	}
}

static void SparkSchemaOfJsonExec(DataChunk &args, ExpressionState &state, Vector &result) {
	auto &input = args.data[0];
	idx_t count = args.size();

	UnaryExecutor::ExecuteWithNulls<string_t, string_t>(input, result, count,
	                                                    [&](string_t input_str, ValidityMask &mask, idx_t idx) {
		                                                    const char *json_cstr = input_str.GetData();
		                                                    idx_t json_len = input_str.GetSize();

		                                                    yyjson_doc *doc = yyjson_read(json_cstr, json_len, 0);
		                                                    if (!doc) {
			                                                    mask.SetInvalid(idx);
			                                                    return string_t();
		                                                    }

		                                                    yyjson_val *root = yyjson_doc_get_root(doc);
		                                                    std::string ddl = InferSparkType(root);
		                                                    yyjson_doc_free(doc);

		                                                    return StringVector::AddString(result, ddl);
	                                                    });
}

inline ScalarFunction CreateSparkSchemaOfJsonFunction() {
	ScalarFunction func("spark_schema_of_json", {LogicalType::VARCHAR}, LogicalType::VARCHAR, SparkSchemaOfJsonExec);
	func.null_handling = FunctionNullHandling::SPECIAL_HANDLING;
	return func;
}

} // namespace duckdb
