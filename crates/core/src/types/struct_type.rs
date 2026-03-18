use super::DataType;

/// A named, typed field within a struct schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StructField {
    pub name: std::string::String,
    pub data_type: DataType,
    pub nullable: bool,
}

impl StructField {
    pub fn new(name: impl Into<std::string::String>, data_type: DataType, nullable: bool) -> Self {
        Self { name: name.into(), data_type, nullable }
    }

    pub fn nullable(name: impl Into<std::string::String>, data_type: DataType) -> Self {
        Self::new(name, data_type, true)
    }

    pub fn not_null(name: impl Into<std::string::String>, data_type: DataType) -> Self {
        Self::new(name, data_type, false)
    }
}

/// The schema of a relation: an ordered list of named, typed fields.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct StructType {
    pub fields: Vec<StructField>,
}

impl StructType {
    pub fn new(fields: Vec<StructField>) -> Self {
        Self { fields }
    }

    /// An empty schema (used as a sentinel for unresolvable plans).
    pub fn empty() -> Self {
        Self { fields: vec![] }
    }

    /// Schema whose single column is the given type — convenience for scalars.
    pub fn single(name: impl Into<std::string::String>, data_type: DataType) -> Self {
        Self { fields: vec![StructField::nullable(name, data_type)] }
    }

    /// Lookup a field by name (case-insensitive, matches Spark behaviour).
    pub fn field_by_name(&self, name: &str) -> Option<&StructField> {
        let lower = name.to_lowercase();
        self.fields.iter().find(|f| f.name.to_lowercase() == lower)
    }

    /// Lookup index by name (case-insensitive).
    pub fn field_index(&self, name: &str) -> Option<usize> {
        let lower = name.to_lowercase();
        self.fields.iter().position(|f| f.name.to_lowercase() == lower)
    }

    /// All field names in order.
    pub fn field_names(&self) -> Vec<&str> {
        self.fields.iter().map(|f| f.name.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Merge two schemas (used for JOIN output: left fields then right fields).
    /// Duplicate names are kept — callers must qualify with table aliases.
    pub fn merge(left: &StructType, right: &StructType) -> StructType {
        let mut fields = left.fields.clone();
        fields.extend(right.fields.clone());
        StructType { fields }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> StructType {
        StructType::new(vec![
            StructField::nullable("id", DataType::Long),
            StructField::nullable("Name", DataType::String),
            StructField::not_null("amount", DataType::Decimal { precision: 10, scale: 2 }),
        ])
    }

    #[test]
    fn field_by_name_case_insensitive() {
        let s = schema();
        assert!(s.field_by_name("id").is_some());
        assert!(s.field_by_name("ID").is_some());
        assert!(s.field_by_name("name").is_some()); // "Name" → found
        assert!(s.field_by_name("missing").is_none());
    }

    #[test]
    fn field_index() {
        let s = schema();
        assert_eq!(s.field_index("id"), Some(0));
        assert_eq!(s.field_index("AMOUNT"), Some(2));
        assert_eq!(s.field_index("x"), None);
    }

    #[test]
    fn merge() {
        let left = StructType::new(vec![StructField::nullable("a", DataType::Integer)]);
        let right = StructType::new(vec![StructField::nullable("b", DataType::String)]);
        let merged = StructType::merge(&left, &right);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged.fields[0].name, "a");
        assert_eq!(merged.fields[1].name, "b");
    }
}
