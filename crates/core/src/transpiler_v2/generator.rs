use super::expression::Expression;

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
        let (kind, outer) = match name {
            "explode" => (GeneratorKind::Explode, false),
            "explode_outer" => (GeneratorKind::Explode, true),
            "posexplode" => (GeneratorKind::PosExplode, false),
            "posexplode_outer" => (GeneratorKind::PosExplode, true),
            "inline" => (GeneratorKind::Inline, false),
            "inline_outer" => (GeneratorKind::Inline, true),
            "json_tuple" => (GeneratorKind::JsonTuple, false),
            "stack" => (GeneratorKind::Stack, false),
            _ => return None,
        };
        Some((kind, outer))
    }

    pub fn name(&self) -> &'static str {
        match (self.kind, self.outer) {
            (GeneratorKind::Explode, false) => "explode",
            (GeneratorKind::Explode, true) => "explode_outer",
            (GeneratorKind::PosExplode, false) => "posexplode",
            (GeneratorKind::PosExplode, true) => "posexplode_outer",
            (GeneratorKind::Inline, false) => "inline",
            (GeneratorKind::Inline, true) => "inline_outer",
            (GeneratorKind::JsonTuple, _) => "json_tuple",
            (GeneratorKind::Stack, _) => "stack",
        }
    }
}
