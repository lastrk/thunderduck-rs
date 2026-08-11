use super::expression::Expression;
use super::function_registry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeneratorKind {
    Explode,
    PosExplode,
    Inline,
    JsonTuple,
    Stack,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Generator {
    pub kind: GeneratorKind,
    pub args: Vec<Expression>,
    pub aliases: Vec<String>,
    pub outer: bool,
}

impl Generator {
    pub fn is_function(name: &str) -> bool {
        Self::classify(name).is_some()
    }

    pub fn from_function(name: &str, args: Vec<Expression>) -> Option<Self> {
        let (kind, outer) = Self::classify(name)?;
        Some(Self {
            kind,
            args,
            aliases: Vec::new(),
            outer,
        })
    }

    fn classify(name: &str) -> Option<(GeneratorKind, bool)> {
        function_registry::generator_spec(name).map(|spec| (spec.kind, spec.outer))
    }

    pub fn name(&self) -> &'static str {
        function_registry::generator_name(self.kind, self.outer)
            .expect("every Generator kind/outer pair is registry-backed")
    }
}
